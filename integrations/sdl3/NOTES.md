<!-- Notes from the build spike of 2026-10-08. Line references are to the tag named below. -->

> These are the notes of the spike that came before the driver: what SDL
> needed to build for WASIX, and what each driver hook does when it is
> missing. The driver has since been written (`driver/`, see `README.md`).
> The two small programs the spike ran (`test_dummy.c`, `verify_nofb.c`) and
> its driver skeleton (`sdl3-wasmer-driver-skeleton.patch`) are no longer in
> this directory: `tests/` and SDL's own `testautomation` took their place,
> and `build.sh` now builds the driver too, with more of SDL's subsystems on
> their dummy backends and SDL's own iconv (section 1.5 lists what the spike
> built). What the spike's programs printed is kept below as it was.

# SDL3 on WASIX: build spike and video-driver hook inventory

Spike for `wasmer-gui` (`<wasmer/gui.h>`). Two questions:

1. Can SDL3 be built as a static library for WASIX, and with which patches? **Yes. A 3-file, 45-line patch gives a `libSDL3.a` that initialises video and events, creates a window, runs threads and timers, and renders with the software renderer under `wasmer run`.**
2. What exactly does an SDL3 video driver implement? **98 function pointers in `SDL_VideoDevice` (4 of them required), 12 in the mouse table, and one bootstrap entry.** Each is listed below with its NULL behaviour, read from the sources at the tag and, where it mattered, checked by running.

Everything here was read or run at SDL `release-3.4.18`. Statements that were not verified are marked **not checked**.

## Part 1 - build

### 1.1 Versions

| | |
|---|---|
| SDL | tag `release-3.4.18` (the latest `release-3.*` tag on 2026-10-08), commit `829a65d769d935c4852f8159e964312c0957260a` ("Updated to version 3.4.18 for release", 2026-10-02), shallow clone of `https://github.com/libsdl-org/SDL.git` |
| Compiler | `wasixcc 0.4.7` ("WASIX clang version 21.1.2"); `-dumpmachine` prints `wasm32-unknown-wasi`; sysroot `~/.wasixcc/sysroot/sysroot-exnref-eh` |
| Build tools | cmake 4.3.2, ninja 1.13.2 |
| Runtime | `wasmer 7.4.2` CLI on macOS 27.0 arm64 |

### 1.2 Using wasixcc with CMake

wasixcc 0.4.7 ships **no CMake toolchain file**, and `wasixccenv` prints none: its subcommands install or download things and print the sysroot. The simplest reliable setup is a small toolchain file, `wasix-toolchain.cmake` (47 lines) in this directory:

- `CMAKE_SYSTEM_NAME WASI`. CMake 4.3.2 knows this platform: `Modules/Platform/WASI-Initialize.cmake` sets the variable `WASI=1`. `UNIX` is **not** set. Older CMake releases: not checked.
- Compiler, archiver and ranlib are `wasixcc`, `wasix++`, `wasixar`, `wasixranlib`. No `--target` or `--sysroot` is passed; the wrapper adds them.
- `CMAKE_FIND_ROOT_PATH` is the sysroot printed by `wasixccenv print-sysroot`; programs are searched on the host only.

Things about wasixcc that matter here, all observed:

- It predefines `__wasi__`, `__wasm__`, `__wasm32__` and `_REENTRANT`, and compiles with `-pthread` and `-fvisibility=hidden`.
- `__wasix__` is **not** predefined. It appears only after including some libc headers: `<stdio.h>`, `<unistd.h>` and `<pthread.h>` define it, `<stdlib.h>` alone does not. Source-level checks therefore have to use `__wasi__`.
- Linking an executable adds `-lc -lresolv -lrt -lm -lpthread -lutil` and `--shared-memory --import-memory --export-dynamic` by itself.
- Undefined symbols are hard link errors, so CMake's link-based feature checks can be trusted. The SDL configure step takes about 10 s.
- `wasixcc --version | head -1` kills wasixcc with SIGPIPE, which fails a script under `set -o pipefail`. `build.sh` uses `sed -n 1p`.

### 1.3 What unpatched SDL does

`cmake/sdlplatform.cmake` has no case for `CMAKE_SYSTEM_NAME=WASI`, so the platform is `unknown`. None of the platform branches in `CMakeLists.txt` is taken (`ANDROID`, `EMSCRIPTEN`, `UNIX AND NOT APPLE ...`, `WINDOWS`, `APPLE`, and so on). It does **not** stray into the Emscripten paths. The generic tail then stops because no thread backend was selected:

```
CMake Error at CMakeLists.txt:3696 (message):
Threads are needed by many SDL subsystems and may not be disabled


-- Configuring incomplete, errors occurred!
```

The timers check right after it would stop the same way. Two source-level problems sit behind that; both were hit in scratch builds:

- `src/dynapi/SDL_dynapi.c:482:2: error: Please define your platform.` `SDL_dynapi.h` leaves the dynamic API on for unknown platforms.
- `src/thread/pthread/SDL_systhread.c`: `pthread_getschedparam`, `pthread_setschedparam`, `sched_get_priority_min` and `sched_get_priority_max` are undeclared. wasix-libc's headers hide them behind `__wasilibc_unmodified_upstream` ("WASI has no CPU scheduling support").

Pretending to be Linux or generic UNIX from the toolchain file was rejected. That branch unconditionally compiles `src/core/linux/*`, `src/core/unix/*` and `src/misc/unix/*`, and probes for X11, Wayland, udev and friends.

### 1.4 The patch (`sdl3-wasix.patch`)

3 files, +45 -4, sha256 `c18c43f906d23c30f5032037c9ddd4041df068f0f542937785f1d455919f32f7`. It adds a `WASI` platform next to `EMSCRIPTEN` and touches no unrelated code.

| File | Change | Why | Needed? |
|---|---|---|---|
| `CMakeLists.txt`, defaults | `SDL_PTHREADS_DEFAULT` and `SDL_CLOCK_GETTIME_DEFAULT` are ON for `WASI`; `SDL_SHARED_AVAILABLE` is OFF | WASIX has pthreads and `clock_gettime`; the library is static only | Saves passing `-DSDL_PTHREADS=ON` by hand |
| `CMakeLists.txt`, new `elseif(WASI)` branch | Selects pthreads, `clock_gettime`, filesystem `unix`, file operations `posix`, time `unix`, timer `unix`, locale `unix`; sets `DYNAPI_NEEDS_DLOPEN` | Everything not set falls back to SDL's dummy backends | Yes: without it configure fails as in 1.3 |
| the `set(DYNAPI_NEEDS_DLOPEN 1)` line in that branch | With no `HAVE_DLOPEN`, this is SDL's own switch for "no dynamic API" (`SDL_dynapi.h`) | Avoids a source edit | Yes: without it `SDL_dynapi.c` fails to compile (checked) |
| `src/thread/pthread/SDL_systhread.c` | `SDL_SYS_SetThreadPriority` returns true early for `__wasi__`, sharing the existing RISC OS branch | The scheduling API is not declared | Yes: the only failing file of 189 (checked) |
| `cmake/sdlplatform.cmake` | `CMAKE_SYSTEM_NAME MATCHES "WASI.*"` sets the platform to `WASI` | Does not depend on CMake's own platform module | No with CMake 4.3.2: configure still picks the branch without it (checked). Kept for older CMake (not checked). The strict minimum is 2 files, +42 -4 |

The new branch, as it is in the patch:

```cmake
elseif(WASI)
# SDL_dynapi needs dlopen() or LoadLibrary(); this turns SDL_DYNAMIC_API off
# (see the DYNAPI_NEEDS_DLOPEN test in src/dynapi/SDL_dynapi.h).
set(DYNAPI_NEEDS_DLOPEN 1)

CheckO_CLOEXEC()
CheckPTHREAD()

if(SDL_CLOCK_GETTIME)
  check_symbol_exists(clock_gettime "time.h" HAVE_CLOCK_GETTIME)
endif()

set(SDL_FILESYSTEM_UNIX 1)
sdl_glob_sources("${SDL3_SOURCE_DIR}/src/filesystem/unix/*.c")
set(HAVE_SDL_FILESYSTEM TRUE)

set(SDL_FSOPS_POSIX 1)
sdl_sources("${SDL3_SOURCE_DIR}/src/filesystem/posix/SDL_sysfsops.c")
set(HAVE_SDL_FSOPS TRUE)

set(SDL_TIME_UNIX 1)
sdl_glob_sources("${SDL3_SOURCE_DIR}/src/time/unix/*.c")
set(HAVE_SDL_TIME TRUE)

set(SDL_TIMER_UNIX 1)
sdl_glob_sources("${SDL3_SOURCE_DIR}/src/timer/unix/*.c")
set(HAVE_SDL_TIMERS TRUE)

sdl_glob_sources("${SDL3_SOURCE_DIR}/src/locale/unix/*.c")
set(HAVE_SDL_LOCALE TRUE)
```

(The patch file has an explanatory comment at the top of the branch that is left out here.)

### 1.5 Configuration that was built

`build.sh` passes, with build type `Release`:

```
-DSDL_SHARED=OFF -DSDL_STATIC=ON -DSDL_TEST_LIBRARY=OFF -DSDL_TESTS=OFF -DSDL_EXAMPLES=OFF
-DSDL_ASSEMBLY=OFF -DSDL_AUDIO=ON -DSDL_DUMMYAUDIO=ON -DSDL_DISKAUDIO=OFF
-DSDL_VIDEO=ON -DSDL_DUMMYVIDEO=ON -DSDL_OFFSCREEN=OFF -DSDL_RENDER=ON -DSDL_GPU=OFF
-DSDL_OPENGL=OFF -DSDL_OPENGLES=OFF -DSDL_VULKAN=OFF
-DSDL_CAMERA=OFF -DSDL_JOYSTICK=OFF -DSDL_HAPTIC=OFF -DSDL_HIDAPI=OFF
-DSDL_SENSOR=OFF -DSDL_POWER=OFF -DSDL_DIALOG=OFF -DSDL_TRAY=OFF -DSDL_LIBUDEV=OFF
```

Result, from the generated `SDL_build_config.h` and the build log:

| Area | Backend | Notes |
|---|---|---|
| Video | `dummy`, the only driver | `SDL_VIDEO_DRIVER_DUMMY` |
| Render | `software`, the only driver | `SDL_VIDEO_RENDER_SW` comes from `src/SDL_internal.h`, which is why the CMake summary prints an empty "Render drivers:" line |
| Audio | `dummy` | never initialised (**not checked**) |
| Threads | pthread | recursive mutexes, `sem_timedwait`, `pthread_setname_np`, GCC atomics |
| Timers, time | `unix`, `unix` | `HAVE_CLOCK_GETTIME`, `HAVE_NANOSLEEP` |
| Filesystem | `unix` paths, `posix` file operations | see 1.8 for `SDL_GetBasePath` |
| Locale | `unix` | reads `LANG` and `LANGUAGE` |
| Process | `posix` | chosen by SDL's generic check (`posix_spawn` and `vfork` exist); compiled, **not checked** at run time |
| Shared objects | `dummy` | no `dlopen` |
| Dynamic API | off | only the `SDL_DYNAPI_entry` stub remains |
| URL opening, dialogs, tray, camera, sensors, joystick, haptic, HIDAPI, power, GPU | dummy or disabled | |
| libc | system (`SDL_LIBC=ON`), system `iconv` | |

Output: `libSDL3.a`, 2,056,338 bytes, 189 objects, **0 compiler warnings**. `cmake --install` also produces the headers, `lib/cmake/SDL3/*` and `lib/pkgconfig/sdl3.pc` (`Libs: -lSDL3 -lpthread -lm`). The library's undefined symbols are plain libc and pthread functions.

### 1.6 Test program and its exact output

`test_dummy.c` does the following; its exit code is the number of failed steps.

- Sets `SDL_HINT_VIDEO_DRIVER=dummy` and calls `SDL_Init(SDL_INIT_VIDEO | SDL_INIT_EVENTS)`.
- Creates a 320x240 window.
- Runs 10 iterations of `SDL_WaitEventTimeout(&ev, 20)` while an `SDL_CreateThread` thread and an `SDL_AddTimer` callback each push one user event.
- Uses the window surface: `SDL_GetWindowSurface`, fill, `SDL_UpdateWindowSurface`.
- Uses the software renderer: clear, fill rect, `SDL_RenderReadPixels` with a pixel check, present. Then creates a default-driver renderer.

Built with `wasixcc -O2 test_dummy.c -I$PREFIX/include -L$PREFIX/lib -lSDL3 -o test_dummy.wasm` (1,515,316 bytes) and run with `wasmer run test_dummy.wasm`. Output of the final run, exit code 0:

```
SDL linked version: 3.4.18 (compiled against 3.4.18)
SDL revision: SDL-3.4.18-release-3.4.18
SDL platform: Unknown (see SDL_platform.h)
compiled-in video drivers (1): dummy
compiled-in render drivers (1): software
ok    SDL_SetHint(SDL_HINT_VIDEO_DRIVER, "dummy")
ok    SDL_Init(SDL_INIT_VIDEO | SDL_INIT_EVENTS)
    current video driver: dummy
ok    SDL_GetPrimaryDisplay / SDL_GetDesktopDisplayMode
    display 1 "1": 1024x768 SDL_PIXELFORMAT_XRGB8888 density=1.00 refresh=0.00
ok    SDL_CreateWindow(320x240)
    window id=2 size=320x240 flags=0x600 pixel_density=1.00 format=SDL_PIXELFORMAT_XRGB8888
ok    SDL_RegisterEvents
ok    SDL_CreateThread
ok    SDL_AddTimer
    iter 0: event 0x20c SDL_EVENT_WINDOW_MOUSE_ENTER
    iter 1: event 0x20e SDL_EVENT_WINDOW_FOCUS_GAINED
    iter 2: event 0x202 SDL_EVENT_WINDOW_SHOWN
    iter 3: event 0x207 SDL_EVENT_WINDOW_PIXEL_SIZE_CHANGED
    iter 5: user event code=42
    iter 7: user event code=43
    10 x SDL_WaitEventTimeout(20ms): 6 events, 4 timeouts, 104.5 ms elapsed
ok    SDL_WaitEventTimeout loop took a plausible time (>= 60ms)
ok    SDL_WaitThread (pusher returned 0)
ok    user event pushed from another thread was received
ok    user event pushed from SDL_AddTimer callback was received
ok    SDL_GetWindowSurface
    window surface: 320x240 format=SDL_PIXELFORMAT_XRGB8888 pitch=1280 pixels=non-NULL
ok    SDL_FillSurfaceRect
ok    SDL_UpdateWindowSurface
ok    SDL_DestroyWindowSurface
ok    SDL_CreateRenderer(window, "software")
    renderer name=software output=320x240
ok    SDL_SetRenderDrawColor + SDL_RenderClear
ok    SDL_RenderFillRect
ok    SDL_RenderReadPixels
    pixel(20,20) = 255,128,0,255 (expected 255,128,0,255)
ok    rendered pixel has the expected colour
ok    SDL_RenderPresent
ok    SDL_CreateRenderer(window, NULL)
    default renderer name=software
done: 0 failure(s)
```

Only the elapsed time changes between runs (104.4 to 105.3 ms seen). Reading the output:

- The four window events come from SDL itself. The dummy driver has no `ShowWindow` hook, so SDL assigns mouse and keyboard focus and sends `SHOWN`.
- The thread's event arrives at about 30 ms and the timer's at about 60 ms. That is why iterations 4, 6, 8 and 9 time out and the loop takes about 100 ms.
- The window surface is `XRGB8888` with pitch 1280 = 320 * 4.

The module imports 41 host functions: 24 from `wasi_snapshot_preview1`, 16 from `wasix_32v1` (among them `futex_wait`, `futex_wake`, `futex_wake_all`, `thread_exit`, `thread_signal`, `callback_signal`) and `wasi.thread-spawn`, plus a shared imported memory. That is what a runtime has to provide for this configuration.

### 1.7 Additional checks

`verify_nofb.c` links against the same `libSDL3.a` but includes SDL's internal headers so that it can modify the live `SDL_VideoDevice`. It backs two statements in Part 2 (sections 2.7 and 2.8). Output of the final run:

```
[wait] hooks present in dummy driver: WaitEventTimeout=NULL SendWakeupEvent=NULL
[wait] no hooks:   SDL_WaitEventTimeout(200ms) -> timeout after 200.8 ms, PumpEvents called 91 times
[wait] with hooks: SDL_WaitEventTimeout(200ms) -> timeout after 202.1 ms, PumpEvents 2, WaitEventTimeout 1, SendWakeupEvent 0
[wait] with hooks: SDL_WaitEvent() + push from thread at 100ms -> event (type user) after 101.3 ms, PumpEvents 3, WaitEventTimeout 1, SendWakeupEvent 1
[fb] render drivers compiled in: 1 (software)
[fb] no hooks: SDL_GetWindowSurface            -> NULL (Window framebuffer support not available)
[fb] no hooks: SDL_CreateRenderer(NULL)        -> NULL (Couldn't find matching render driver)
[fb] no hooks: SDL_CreateRenderer("software")  -> NULL (Window framebuffer support not available)
[fb] no hooks: SDL_CreateSoftwareRenderer(surface) -> renderer (but nothing presents it to the window)
done
```

**Module size.** SDL marks its public API `__attribute__((visibility("default")))` and wasixcc links with `--export-dynamic`. Every public function in every linked object is therefore exported from the final module and cannot be garbage-collected.

| Build | `test_dummy.wasm` | wasm exports |
|---|---|---|
| default | 1,515,316 bytes | 1021, of which 1015 are `SDL_*` |
| `SDL_HIDE_API=1 ./build.sh` (adds `-DCMAKE_C_FLAGS=-DSDL_DECLSPEC=`) | 1,000,014 bytes | 6 |

Both variants pass the test. The flag is not in the default build because a dynamically linked setup, with side modules calling SDL in the main module, would need those exports.

### 1.8 Open problems and things not checked

Problems seen:

- **`SDL_GetBasePath()` returns NULL with an empty error string.** The `unix` backend reads `/proc/self/exe`, which does not exist here. Applications that build asset paths from it will break. Options: a small WASI case in `src/filesystem/unix/SDL_sysfilesystem.c` that returns the current directory or "/", or a dedicated backend (Emscripten has its own).
- **`SDL_GetPrefPath()` needs `HOME` or `XDG_DATA_HOME`.** With neither it fails with "neither XDG_DATA_HOME nor HOME environment is set". With `HOME=/work` on a mapped directory it created and returned `/work/.local/share/wasmer/sdl3-spike/`. `SDL_GetUserFolder(SDL_FOLDER_HOME)` behaves the same way.
- **`SDL_GetSystemRAM()` returns -15.** `sysconf(_SC_PHYS_PAGES)` returns -248 under wasmer 7.4.2 (`_SC_PAGESIZE` is 65536). The value comes from the libc or runtime; SDL only multiplies it.
- **`SDL_GetPlatform()` returns "Unknown (see SDL_platform.h)".** There is no `SDL_PLATFORM_WASI` in `SDL_platform_defines.h`. The patch does not add one because nothing needed it.
- `SDL_OpenURL` fails with "That operation is not supported" (dummy backend, expected).
- `SDL_SetThreadPriority` always reports success and does nothing (the patch).
- The 1 ms polling fallback of `SDL_WaitEventTimeout` measured about 2.2 ms per iteration: 91 pumps in 200 ms, roughly 450 wakeups per second while idle. A real driver should implement the wait hooks (2.8).

Seen working in a side probe: `SDL_GetCurrentDirectory` ("/"), `SDL_GetNumLogicalCPUCores` (18), `SDL_GetCurrentTime` and `SDL_TimeToDateTime`, `SDL_IOFromFile`, `SDL_GetPathInfo` and `SDL_RemovePath` in the current directory, `SDL_GetPreferredLocales` (from `LANG`), and hints read from the environment (`wasmer run --env SDL_VIDEO_DRIVER=dummy`).

Not checked:

- Audio of any kind. `SDL_INIT_AUDIO` was never initialised.
- The POSIX process backend (`SDL_CreateProcess`) at run time.
- SDL's own test suite (`SDL_TESTS=OFF`).
- Any runtime other than the native `wasmer` 7.4.2 CLI on macOS arm64. In particular **not checked in the browser** (`@wasmer/sdk`), where pthreads need `SharedArrayBuffer` and blocking on the main thread is restricted.
- Debug builds, LTO, `-fPIC`, shared SDL, `dlopen`.
- wasi-sdk or other non-WASIX toolchains (the patch keys on `WASI` and `__wasi__` but was only built with wasixcc), and CMake versions other than 4.3.2.
- Long-running behaviour and thread stress beyond the smoke test.

## Part 2 - hook inventory

Sources at `release-3.4.18`: `src/video/SDL_sysvideo.h` and the callers in `src/video/SDL_video.c`, `src/video/SDL_clipboard.c`, `src/events/SDL_events.c`, `src/events/SDL_keyboard.c` and `src/events/SDL_touch.c`.

Conventions in the lists:

- The bracket at the end of each line is the evidence. `v:N` means `src/video/SDL_video.c` line N.
- "Treated as unsupported" means the public function returns false (or NULL) after `SDL_Unsupported()`, whose message is "That operation is not supported".
- "Optional, skipped" means SDL checks the pointer and simply does not call it.

### 2.1 `struct SDL_VideoDevice` function pointers

There are 98, in this order (`src/video/SDL_sysvideo.h` lines 225 to 512). The names were extracted from the header by script and the declarations below are copied from it. The groups are the header's own comment blocks. Two differences from the grouping in the request: in this version window shaping (`UpdateWindowShape`) sits inside the window group, and hit-testing, drag-and-drop and the system menu are single trailing entries.

**Initialization/Query functions** (3)

1. `bool (*VideoInit)(SDL_VideoDevice *_this)` - **Required.** Called unconditionally by `SDL_VideoInit`. Must add at least one display, otherwise init fails with "The video driver did not add any displays" (v:718). [v:712]
2. `void (*VideoQuit)(SDL_VideoDevice *_this)` - **Required.** Called unconditionally by `SDL_VideoQuit`. [v:4707]
3. `void (*ResetTouch)(SDL_VideoDevice *_this)` - Optional. Only consulted when an unknown touch id shows up; NULL sets the error "Unknown touch device id %d, cannot reset". [src/events/SDL_touch.c:137]

**Display functions** (5)

4. `void (*RefreshDisplays)(SDL_VideoDevice *_this)` - Optional, skipped. (Called at the start of every window creation.) [v:2447]
5. `bool (*GetDisplayBounds)(SDL_VideoDevice *_this, SDL_VideoDisplay *display, SDL_Rect *rect)` - Optional. Falls back to: primary display at (0,0), other displays laid out left to right, size = `display->current_mode` w/h (v:1079-1089). [v:1072]
6. `bool (*GetDisplayUsableBounds)(SDL_VideoDevice *_this, SDL_VideoDisplay *display, SDL_Rect *rect)` - Optional. Falls back to `SDL_GetDisplayBounds` (after the `SDL_HINT_DISPLAY_USABLE_BOUNDS` override for the primary display). [v:1112]
7. `bool (*GetDisplayModes)(SDL_VideoDevice *_this, SDL_VideoDisplay *display)` - Optional. Called lazily when a display has no fullscreen modes; NULL means the list is whatever the driver added with `SDL_AddFullscreenDisplayMode` (possibly empty). [v:1220]
8. `bool (*SetDisplayMode)(SDL_VideoDevice *_this, SDL_VideoDisplay *display, SDL_DisplayMode *mode)` - Optional. NULL: SDL only updates its own `current_mode` bookkeeping and reports success. (Never called at all when `VIDEO_DEVICE_CAPS_MODE_SWITCHING_EMULATED` is set, v:1521.) [v:1538]

**Window functions** (43)

9. `bool (*CreateSDLWindow)(SDL_VideoDevice *_this, SDL_Window *window, SDL_PropertiesID create_props)` - Optional. NULL: the window exists only as SDL bookkeeping (the dummy driver has none). [v:2583]
10. `void (*SetWindowTitle)(SDL_VideoDevice *_this, SDL_Window *window)` - Optional, skipped; the title is stored in `window->title` either way. [v:2951]
11. `bool (*SetWindowIcon)(SDL_VideoDevice *_this, SDL_Window *window, SDL_Surface *icon)` - Treated as unsupported: `SDL_SetWindowIcon` returns false; the converted icon is still kept in `window->icon`. [v:2980]
12. `bool (*SetWindowPosition)(SDL_VideoDevice *_this, SDL_Window *window)` - Treated as unsupported: `SDL_SetWindowPosition` returns false. `window->pending.x/y` are set before the call. [v:3054-3062]
13. `void (*SetWindowSize)(SDL_VideoDevice *_this, SDL_Window *window)` - Treated as unsupported: `SDL_SetWindowSize` returns false, and so do `SDL_SetWindowMinimumSize`/`MaximumSize`/`AspectRatio`, which end by calling it. The hook must confirm with `SDL_SendWindowEvent(window, SDL_EVENT_WINDOW_RESIZED, w, h)`; only that event changes `window->w/h`. [v:3223-3228]
14. `void (*SetWindowMinimumSize)(SDL_VideoDevice *_this, SDL_Window *window)` - Optional, skipped; limits are stored in `window->min_w/min_h`. [v:3355]
15. `void (*SetWindowMaximumSize)(SDL_VideoDevice *_this, SDL_Window *window)` - Optional, skipped; limits are stored in `window->max_w/max_h`. [v:3395]
16. `void (*SetWindowAspectRatio)(SDL_VideoDevice *_this, SDL_Window *window)` - Optional, skipped; values are stored in `window->min_aspect/max_aspect`. [v:3251]
17. `bool (*GetWindowBordersSize)(SDL_VideoDevice *_this, SDL_Window *window, int *top, int *left, int *bottom, int *right)` - Treated as unsupported: `SDL_GetWindowBordersSize` returns false with all four outputs zeroed. [v:3296]
18. `float (*GetWindowContentScale)(SDL_VideoDevice *_this, SDL_Window *window)` - Optional. Falls back to `SDL_GetWindowPixelDensity(window) * SDL_GetDisplayContentScale(window->displayID)`. [v:1899]
19. `void (*GetWindowSizeInPixels)(SDL_VideoDevice *_this, SDL_Window *window, int *w, int *h)` - Optional. Falls back to `ceil(window size * pixel_density)` of the desktop mode (of the current mode for a fullscreen window with a fullscreen mode). [v:3317]
20. `bool (*SetWindowOpacity)(SDL_VideoDevice *_this, SDL_Window *window, float opacity)` - Treated as unsupported (`SDL_SetWindowOpacity` returns false). [v:3802]
21. `bool (*SetWindowParent)(SDL_VideoDevice *_this, SDL_Window *window, SDL_Window *parent)` - Treated as unsupported (`SDL_SetWindowParent` returns false). [v:3841]
22. `bool (*SetWindowModal)(SDL_VideoDevice *_this, SDL_Window *window, bool modal)` - Treated as unsupported (`SDL_SetWindowModal` returns false). [v:3864]
23. `void (*ShowWindow)(SDL_VideoDevice *_this, SDL_Window *window)` - Optional. NULL: SDL gives the window mouse and keyboard focus itself (`SDL_SetMouseFocus`/`SDL_SetKeyboardFocus`), then sends `SDL_EVENT_WINDOW_SHOWN`. With a hook, focus is the driver's job. [v:3432]
24. `void (*HideWindow)(SDL_VideoDevice *_this, SDL_Window *window)` - Optional. NULL: SDL clears mouse and keyboard focus itself; `SDL_EVENT_WINDOW_HIDDEN` is sent by SDL either way. [v:3475]
25. `void (*RaiseWindow)(SDL_VideoDevice *_this, SDL_Window *window)` - Optional, skipped; `SDL_RaiseWindow` still returns true. [v:3493]
26. `void (*MaximizeWindow)(SDL_VideoDevice *_this, SDL_Window *window)` - Treated as unsupported (`SDL_MaximizeWindow` returns false). [v:3504]
27. `void (*MinimizeWindow)(SDL_VideoDevice *_this, SDL_Window *window)` - Treated as unsupported (`SDL_MinimizeWindow` returns false). [v:3527]
28. `void (*RestoreWindow)(SDL_VideoDevice *_this, SDL_Window *window)` - Treated as unsupported (`SDL_RestoreWindow` returns false). [v:3546]
29. `void (*SetWindowBordered)(SDL_VideoDevice *_this, SDL_Window *window, bool bordered)` - Treated as unsupported; the `SDL_WINDOW_BORDERLESS` flag is not changed. [v:3119]
30. `void (*SetWindowResizable)(SDL_VideoDevice *_this, SDL_Window *window, bool resizable)` - Treated as unsupported; the `SDL_WINDOW_RESIZABLE` flag is not changed. (Emscripten registers an empty function only so the flag can change.) [v:3142]
31. `void (*SetWindowAlwaysOnTop)(SDL_VideoDevice *_this, SDL_Window *window, bool on_top)` - Treated as unsupported; the flag is not changed. [v:3166]
32. `SDL_FullscreenResult (*SetWindowFullscreen)(SDL_VideoDevice *_this, SDL_Window *window, SDL_VideoDisplay *display, SDL_FullscreenOp fullscreen)` - Optional. NULL is treated as immediate success: SDL sends `SDL_EVENT_WINDOW_ENTER_FULLSCREEN`/`LEAVE_FULLSCREEN` and synthesises `MOVED` + `RESIZED` to the display mode size (unless `VIDEO_DEVICE_CAPS_SENDS_FULLSCREEN_DIMENSIONS`). A hook returns `SDL_FULLSCREEN_SUCCEEDED`, `_PENDING` or `_FAILED`. [v:2026, v:2090]
33. `void *(*GetWindowICCProfile)(SDL_VideoDevice *_this, SDL_Window *window, size_t *size)` - Treated as unsupported (`SDL_GetWindowICCProfile` returns NULL). [v:2189]
34. `SDL_DisplayID (*GetDisplayForWindow)(SDL_VideoDevice *_this, SDL_Window *window)` - Optional. NULL (or a hook returning 0): the display containing the window rectangle, else the primary display. [v:1709]
35. `bool (*SetWindowMouseRect)(SDL_VideoDevice *_this, SDL_Window *window)` - Treated as unsupported (`SDL_SetWindowMouseRect` returns false). [v:4106]
36. `bool (*SetWindowMouseGrab)(SDL_VideoDevice *_this, SDL_Window *window, bool grabbed)` - Optional, skipped; SDL only tracks the flag and `grabbed_window` (no real grab). [v:4004]
37. `bool (*SetWindowKeyboardGrab)(SDL_VideoDevice *_this, SDL_Window *window, bool grabbed)` - Optional, skipped; SDL only tracks the flag. [v:4009]
38. `void (*DestroyWindow)(SDL_VideoDevice *_this, SDL_Window *window)` - Optional, skipped. [v:4582]
39. `bool (*CreateWindowFramebuffer)(SDL_VideoDevice *_this, SDL_Window *window, SDL_PixelFormat *format, void **pixels, int *pitch)` - NULL (or `UpdateWindowFramebuffer` NULL): `SDL_GetWindowSurface` fails with "Window framebuffer support not available" unless SDL managed to emulate the framebuffer with a texture on an accelerated renderer (v:3660-3682). The software renderer then cannot be created either. Verified by running, see 2.7. [v:3685-3690]
40. `bool (*SetWindowFramebufferVSync)(SDL_VideoDevice *_this, SDL_Window *window, int vsync)` - Treated as unsupported (`SDL_SetWindowSurfaceVSync` returns false). [v:3734]
41. `bool (*GetWindowFramebufferVSync)(SDL_VideoDevice *_this, SDL_Window *window, int *vsync)` - Treated as unsupported (`SDL_GetWindowSurfaceVSync` returns false). [v:3744]
42. `bool (*UpdateWindowFramebuffer)(SDL_VideoDevice *_this, SDL_Window *window, const SDL_Rect *rects, int numrects)` - Required together with `CreateWindowFramebuffer`: checked once when the surface is created, then called unconditionally by `SDL_UpdateWindowSurfaceRects`. [v:3685, v:3774]
43. `void (*DestroyWindowFramebuffer)(SDL_VideoDevice *_this, SDL_Window *window)` - Optional, skipped. [v:3789]
44. `void (*OnWindowEnter)(SDL_VideoDevice *_this, SDL_Window *window)` - Optional, skipped. [v:4411]
45. `bool (*UpdateWindowShape)(SDL_VideoDevice *_this, SDL_Window *window, SDL_Surface *shape)` - Optional. NULL: `SDL_SetWindowShape` stores the shape surface as a window property and returns true (the window must have `SDL_WINDOW_TRANSPARENT`); nothing is applied. [v:6202]
46. `bool (*FlashWindow)(SDL_VideoDevice *_this, SDL_Window *window, SDL_FlashOperation operation)` - Treated as unsupported (`SDL_FlashWindow` returns false). [v:4181]
47. `bool (*ApplyWindowProgress)(SDL_VideoDevice *_this, SDL_Window *window)` - Optional, skipped; state and value are stored in the window and the call returns true. [v:4199, v:4225]
48. `bool (*SetWindowFocusable)(SDL_VideoDevice *_this, SDL_Window *window, bool focusable)` - Optional. NULL: `SDL_SetWindowFocusable` returns true but does not change the flag. [v:3934]
49. `bool (*SetWindowFillDocument)(SDL_VideoDevice *_this, SDL_Window *window, bool fill)` - Optional. NULL: `SDL_WINDOW_FILL_DOCUMENT` is stripped at window creation and `SDL_SetWindowFillDocument` returns true without doing anything. [v:2571, v:3954]
50. `bool (*SyncWindow)(SDL_VideoDevice *_this, SDL_Window *window)` - Optional. NULL: `SDL_SyncWindow` returns true at once (the driver is assumed synchronous). [v:3599]
51. `bool (*ReconfigureWindow)(SDL_VideoDevice *_this, SDL_Window *window, SDL_WindowFlags flags)` - Optional. NULL: SDL falls back to `SDL_RecreateWindow` (destroy + `CreateSDLWindow`) when a window's graphics flags change. [v:2658, v:2870]

**OpenGL support** (11)

52. `bool (*GL_LoadLibrary)(SDL_VideoDevice *_this, const char *path)` - `SDL_GL_LoadLibrary` fails: "No dynamic OpenGL support in current SDL video driver (%s)". [v:4744]
53. `SDL_FunctionPointer (*GL_GetProcAddress)(SDL_VideoDevice *_this, const char *proc)` - `SDL_GL_GetProcAddress` returns NULL: "No dynamic GL support in current SDL video driver (%s)". [v:4768]
54. `void (*GL_UnloadLibrary)(SDL_VideoDevice *_this)` - Optional, skipped. [v:4698, v:4752, v:4814]
55. `SDL_GLContext (*GL_CreateContext)(SDL_VideoDevice *_this, SDL_Window *window)` - The gate for all OpenGL use. NULL: creating a window with `SDL_WINDOW_OPENGL` fails ("OpenGL support is either not configured in SDL or not available in current SDL video driver (%s) or platform"). [v:2505]
56. `bool (*GL_MakeCurrent)(SDL_VideoDevice *_this, SDL_Window *window, SDL_GLContext context)` - Required if `GL_CreateContext` is set (called without a NULL check). [v:5504]
57. `SDL_EGLSurface (*GL_GetEGLSurface)(SDL_VideoDevice *_this, SDL_Window *window)` - Optional. NULL: `SDL_EGL_GetWindowSurface` returns NULL (only compiled with `SDL_VIDEO_OPENGL_EGL`). [v:5579]
58. `bool (*GL_SetSwapInterval)(SDL_VideoDevice *_this, int interval)` - `SDL_GL_SetSwapInterval` fails: "Setting the swap interval is not supported". [v:5595]
59. `bool (*GL_GetSwapInterval)(SDL_VideoDevice *_this, int *interval)` - `SDL_GL_GetSwapInterval` fails: "not implemented". [v:5614]
60. `bool (*GL_SwapWindow)(SDL_VideoDevice *_this, SDL_Window *window)` - Required if `GL_CreateContext` is set (called without a NULL check). [v:5633]
61. `bool (*GL_DestroyContext)(SDL_VideoDevice *_this, SDL_GLContext context)` - Required if `GL_CreateContext` is set (called without a NULL check). [v:5649]
62. `void (*GL_DefaultProfileConfig)(SDL_VideoDevice *_this, int *mask, int *major, int *minor)` - Optional, skipped. [v:5009]

**Vulkan support** (6)

63. `bool (*Vulkan_LoadLibrary)(SDL_VideoDevice *_this, const char *path)` - `SDL_Vulkan_LoadLibrary` fails: "No dynamic Vulkan support in current SDL video driver (%s)". [v:6273]
64. `void (*Vulkan_UnloadLibrary)(SDL_VideoDevice *_this)` - Optional, skipped. [v:4702, v:6307]
65. `char const * const *(*Vulkan_GetInstanceExtensions)(SDL_VideoDevice *_this, Uint32 *count)` - **No NULL check and no `_this` check**: `SDL_Vulkan_GetInstanceExtensions` calls the pointer directly, so an application calling it on a driver without Vulkan calls a NULL function pointer (a trap in WebAssembly). [v:6315]
66. `bool (*Vulkan_CreateSurface)(SDL_VideoDevice *_this, SDL_Window *window, VkInstance instance, const struct VkAllocationCallbacks *allocator, VkSurfaceKHR *surface)` - The gate for Vulkan (also probed by SDL_GPU's Vulkan backend). NULL: `SDL_WINDOW_VULKAN` windows cannot be created; `SDL_Vulkan_CreateSurface` returns false. [v:2515, v:6325]
67. `void (*Vulkan_DestroySurface)(SDL_VideoDevice *_this, VkInstance instance, VkSurfaceKHR surface, const struct VkAllocationCallbacks *allocator)` - Optional, skipped. [v:6362]
68. `bool (*Vulkan_GetPresentationSupport)(SDL_VideoDevice *_this, VkInstance instance, VkPhysicalDevice physicalDevice, Uint32 queueFamilyIndex)` - Optional. NULL: `SDL_Vulkan_GetPresentationSupport` returns true ("always supported"). [v:6386]

**Metal support** (3)

69. `SDL_MetalView (*Metal_CreateView)(SDL_VideoDevice *_this, SDL_Window *window)` - The gate for Metal. NULL: `SDL_WINDOW_METAL` windows cannot be created; `SDL_Metal_CreateView` returns NULL. [v:2525, v:6401]
70. `void (*Metal_DestroyView)(SDL_VideoDevice *_this, SDL_MetalView view)` - Optional, skipped. [v:6424]
71. `void *(*Metal_GetLayer)(SDL_VideoDevice *_this, SDL_MetalView view)` - `SDL_Metal_GetLayer` returns NULL: "Metal is not supported.". [v:6431]

**Event manager functions** (3)

72. `int (*WaitEventTimeout)(SDL_VideoDevice *_this, Sint64 timeoutNS)` - Optional. Used only when `SendWakeupEvent` is also set and at least one window exists; otherwise SDL polls: pump, then `SDL_DelayNS(min(remaining, 1 ms))`. See 2.8. [src/events/SDL_events.c:1759, 1635]
73. `void (*SendWakeupEvent)(SDL_VideoDevice *_this, SDL_Window *window)` - Optional (see `WaitEventTimeout`). Called from whichever thread pushes an event while the main thread is waiting, so it must be thread safe. [src/events/SDL_events.c:1135, 1759]
74. `void (*PumpEvents)(SDL_VideoDevice *_this)` - **Required.** Called without a NULL check on every `SDL_PumpEvents`/`SDL_PollEvent`/`SDL_WaitEvent*`. [src/events/SDL_events.c:1523]

**Suspend/resume the screensaver** (1)

75. `bool (*SuspendScreenSaver)(SDL_VideoDevice *_this)` - `SDL_EnableScreenSaver`/`SDL_DisableScreenSaver` return false (unsupported) after toggling the internal flag. `SDL_VideoInit` calls `SDL_DisableScreenSaver` and ignores the result. [v:4649, v:4665]

**Text input** (4)

76. `bool (*StartTextInput)(SDL_VideoDevice *_this, SDL_Window *window, SDL_PropertiesID props)` - Optional, skipped; `window->text_input_active` is set to true anyway. A hook returning false makes `SDL_StartTextInput` fail. [v:5842]
77. `bool (*StopTextInput)(SDL_VideoDevice *_this, SDL_Window *window)` - Optional, skipped. [v:5865]
78. `bool (*UpdateTextInputArea)(SDL_VideoDevice *_this, SDL_Window *window)` - Optional, skipped; rectangle and cursor are stored in `window->text_input_rect`/`text_input_cursor`. [v:5892]
79. `bool (*ClearComposition)(SDL_VideoDevice *_this, SDL_Window *window)` - Optional. NULL: `SDL_ClearComposition` returns true. [v:5917]

**Screen keyboard** (4)

80. `bool (*HasScreenKeyboardSupport)(SDL_VideoDevice *_this)` - `SDL_HasScreenKeyboardSupport` returns false. [v:5925]
81. `void (*ShowScreenKeyboard)(SDL_VideoDevice *_this, SDL_Window *window, SDL_PropertiesID props)` - Optional, skipped. [v:5835]
82. `void (*HideScreenKeyboard)(SDL_VideoDevice *_this, SDL_Window *window)` - Optional, skipped. [v:5873]
83. `void (*SetTextInputProperties)(SDL_VideoDevice *_this, SDL_Window *window, SDL_PropertiesID props)` - Optional, skipped. [v:5829]

**Clipboard** (10)

84. `const char *const *(*GetTextMimeTypes)(SDL_VideoDevice *_this, size_t *num_mime_types)` - Optional. Falls back to the single type "text/plain;charset=utf-8". [src/video/SDL_clipboard.c:331]
85. `bool (*SetClipboardData)(SDL_VideoDevice *_this)` - Optional. NULL: SDL tries `SetClipboardText`; with neither, the data stays in SDL's in-process clipboard only. [src/video/SDL_clipboard.c:125]
86. `void *(*GetClipboardData)(SDL_VideoDevice *_this, const char *mime_type, size_t *size)` - Optional. NULL: `GetClipboardText` for "text*" types, else the in-process data offered by the application's own callback. [src/video/SDL_clipboard.c:217]
87. `bool (*HasClipboardData)(SDL_VideoDevice *_this, const char *mime_type)` - Optional. NULL: `HasClipboardText` for "text*" types, else the in-process list of offered types. [src/video/SDL_clipboard.c:259]
88. `bool (*SetClipboardText)(SDL_VideoDevice *_this, const char *text)` - Optional. Only consulted when `SetClipboardData` is NULL. [src/video/SDL_clipboard.c:129]
89. `char *(*GetClipboardText)(SDL_VideoDevice *_this)` - Optional. Only consulted when `GetClipboardData` is NULL. [src/video/SDL_clipboard.c:219]
90. `bool (*HasClipboardText)(SDL_VideoDevice *_this)` - Optional. Only consulted when `HasClipboardData` is NULL. [src/video/SDL_clipboard.c:261]
91. `bool (*SetPrimarySelectionText)(SDL_VideoDevice *_this, const char *text)` - Optional. NULL: the text is kept in `_this->primary_selection_text`. [src/video/SDL_clipboard.c:432]
92. `char *(*GetPrimarySelectionText)(SDL_VideoDevice *_this)` - Optional. NULL: returns a copy of the stored text ("" if none). [src/video/SDL_clipboard.c:458]
93. `bool (*HasPrimarySelectionText)(SDL_VideoDevice *_this)` - Optional. NULL: true if the stored text is non-empty. [src/video/SDL_clipboard.c:477]

**MessageBox** (1)

94. `bool (*ShowMessageBox)(SDL_VideoDevice *_this, const SDL_MessageBoxData *messageboxdata, int *buttonID)` - Optional. NULL: SDL tries the bootstrap-level `VideoBootStrap.ShowMessageBox` of the drivers named in `SDL_HINT_VIDEO_DRIVER`, or of every compiled-in driver; if none handles it the call fails with "No message system available". [v:6040]

**Hit-testing** (1)

95. `bool (*SetWindowHitTest)(SDL_Window *window, bool enabled)` - Treated as unsupported (`SDL_SetWindowHitTest` returns false). [v:6163]

**Tell window that app enabled drag'n'drop events** (1)

96. `void (*AcceptDragAndDrop)(SDL_Window *window, bool accept)` - Optional, skipped. [v:2226]

**Display the system-level window menu** (1)

97. `void (*ShowWindowSystemMenu)(SDL_Window *window, int x, int y)` - Treated as unsupported (`SDL_ShowWindowSystemMenu` returns false). [v:6151]

**The function used to dispose of this structure** (1)

98. `void (*free)(SDL_VideoDevice *_this)` - **Required.** Called unconditionally at the end of `SDL_VideoQuit`. [v:4724]

The struct also carries data the driver fills in or uses: `name` (set by SDL from the bootstrap entry), `device_caps`, `system_theme`, `internal` (driver data; windows and displays have their own `internal`), `gl_config`, `vulkan_config`. The `device_caps` flags are all optional:

- `VIDEO_DEVICE_CAPS_MODE_SWITCHING_EMULATED`
- `VIDEO_DEVICE_CAPS_HAS_POPUP_WINDOW_SUPPORT` (without it creating a popup window fails, v:2424)
- `VIDEO_DEVICE_CAPS_SENDS_FULLSCREEN_DIMENSIONS`
- `VIDEO_DEVICE_CAPS_FULLSCREEN_ONLY`
- `VIDEO_DEVICE_CAPS_SENDS_DISPLAY_CHANGES`
- `VIDEO_DEVICE_CAPS_SENDS_HDR_CHANGES`

**What a driver truly needs.** Four pointers are called without a NULL check on every run: `VideoInit`, `VideoQuit`, `PumpEvents`, `free`. `VideoInit` must register at least one display with `SDL_AddBasicVideoDisplay` or `SDL_AddVideoDisplay`. Add `CreateWindowFramebuffer` and `UpdateWindowFramebuffer` and the software renderer works. The dummy driver is exactly that: those six, plus `DestroyWindowFramebuffer`, `SetWindowSize` and `SetWindowPosition` (both just echo the request back as `RESIZED` and `MOVED` events), and the `SetRelativeMouseMode` mouse hook.

### 2.2 Mouse driver table (`SDL_Mouse` in `src/events/SDL_mouse_c.h`)

The driver fills these in during `VideoInit` through `SDL_GetMouse()`. The struct has 11 driver function pointers and two members of type `SDL_MouseMotionTransformCallback`, which is `void (SDLCALL *)(void *userdata, Uint64 timestamp, SDL_Window *window, SDL_MouseID mouseID, float *x, float *y)`. The request named ten hooks. The others present in this version are `CreateAnimatedCursor` (its frame struct is new in SDL 3.4.0), `ApplySystemScale` and `InputTransform`.

1. `SDL_Cursor *(*CreateCursor)(SDL_Surface *surface, int hot_x, int hot_y)` - SDL allocates an empty `SDL_Cursor` itself (`SDL_calloc`), so `SDL_CreateColorCursor`/`SDL_CreateCursor` still succeed, but the host is never told. The surface handed to the hook is always `SDL_PIXELFORMAT_ARGB8888` (converted at 1758). [SDL_mouse.c:1766]
2. `SDL_Cursor *(*CreateAnimatedCursor)(SDL_CursorFrameInfo *frames, int frame_count, int hot_x, int hot_y)` - SDL emulates the animation: one cursor per frame through the normal path (`SDL_CreateCursorAnimation`), advanced by `SDL_UpdateCursorAnimation` on every event pump (src/events/SDL_events.c:1489). [SDL_mouse.c:1701]
3. `SDL_Cursor *(*CreateSystemCursor)(SDL_SystemCursor id)` - `SDL_CreateSystemCursor` returns NULL: "CreateSystemCursor is not currently supported". [SDL_mouse.c:1786]
4. `bool (*ShowCursor)(SDL_Cursor *cursor)` - Skipped: `SDL_SetCursor`/`SDL_ShowCursor`/`SDL_HideCursor` only change SDL's own state. The hook receives the cursor to show, or NULL to hide. [SDL_mouse.c:1820]
5. `bool (*MoveCursor)(SDL_Cursor *cursor)` - Skipped. (Only for drivers that draw the cursor themselves.) [SDL_mouse.c:813]
6. `void (*FreeCursor)(SDL_Cursor *cursor)` - SDL frees the `SDL_Cursor` with `SDL_free`. The hook is only called when `cursor->internal` is non-NULL, and must then free both `internal` and the cursor. [SDL_mouse.c:491, 1914]
7. `bool (*WarpMouse)(SDL_Window *window, float x, float y)` - SDL synthesises a motion event to the target position (also what it does in relative mode); the host pointer does not move. [SDL_mouse.c:1288]
8. `bool (*WarpMouseGlobal)(float x, float y)` - `SDL_WarpMouseGlobal` returns false (unsupported). [SDL_mouse.c:1351]
9. `bool (*SetRelativeMouseMode)(bool enabled)` - `SDL_SetRelativeMouseMode` returns false (unsupported), so `SDL_SetWindowRelativeMouseMode` fails and reverts the window flag (src/video/SDL_video.c:4154). There is no warp-based emulation in this version; the dummy driver installs a hook that just returns true. [SDL_mouse.c:1373]
10. `bool (*CaptureMouse)(SDL_Window *window)` - `SDL_CaptureMouse` returns false (unsupported); the automatic capture while a button is held (`SDL_UpdateMouseCapture`) becomes a no-op. [SDL_mouse.c:1436, 1486]
11. `SDL_MouseButtonFlags (*GetGlobalMouseState)(float *x, float *y)` - `SDL_GetGlobalMouseState` falls back to `SDL_GetMouseState` (window-relative position). [SDL_mouse.c:1232]
12. `SDL_MouseMotionTransformCallback ApplySystemScale` - Skipped. Only consulted for relative motion when `SDL_HINT_MOUSE_RELATIVE_SYSTEM_SCALE` is on and no `InputTransform` is set. Only the Windows driver sets it. [SDL_mouse.c:737]
13. `SDL_MouseMotionTransformCallback InputTransform` - Not a driver hook: set by the application through `SDL_SetRelativeMouseTransform`. [SDL_mouse.c:732, 1195]

Related: if the driver has not called `SDL_SetDefaultCursor` by the end of `VideoInit`, `SDL_PostInitMouse` creates a 1x1 transparent cursor as the default. Mice are announced with `SDL_AddMouse(SDL_DEFAULT_MOUSE_ID, NULL)`.

### 2.3 How a driver registers itself

A driver is one global `VideoBootStrap`, declared at the end of `SDL_sysvideo.h`:

```c
typedef struct VideoBootStrap
{
  const char *name;
  const char *desc;
  SDL_VideoDevice *(*create)(void);
  bool (*ShowMessageBox)(const SDL_MessageBoxData *messageboxdata, int *buttonID);  // can be done without initializing backend!
  bool is_preferred;
} VideoBootStrap;
```

`create` allocates a zeroed `SDL_VideoDevice` with `SDL_calloc`, fills in the pointers and returns it, or returns NULL if the driver is not usable. Selection happens in `SDL_VideoInit` (v:617-758) over the static table `bootstrap[]` (v:88-162):

- With `SDL_HINT_VIDEO_DRIVER` set (it can also come from the environment variable `SDL_VIDEO_DRIVER`, checked here with `wasmer run --env`), SDL walks the comma-separated names and calls `create()` of the entry with that name. Entries with `is_preferred` set are skipped in this mode.
- Without the hint it calls `create()` of every entry in table order and takes the first non-NULL device. `dummy` and `offscreen` return NULL unless the hint names them, so a real driver placed anywhere in the table wins by default.
- SDL then sets `_this->name = bootstrap[i]->name` and `_this->thread`, calls `VideoInit`, and fails if no display was added.

Each driver is compiled in or out by a config macro `SDL_VIDEO_DRIVER_<NAME>`. It is a `#cmakedefine` in `include/build_config/SDL_build_config.h.cmake`, set by `CMakeLists.txt` together with the source globs. The driver sources wrap themselves in `#ifdef SDL_VIDEO_DRIVER_<NAME>`.

Files a new `wasmer` driver must touch (CMake build):

| # | File | Change |
|---|---|---|
| 1 | `src/video/wasmer/*.c`, `*.h` (new) | The driver. Defines `VideoBootStrap WASMER_bootstrap`. |
| 2 | `src/video/SDL_sysvideo.h` | `extern VideoBootStrap WASMER_bootstrap;` |
| 3 | `src/video/SDL_video.c` | `#ifdef SDL_VIDEO_DRIVER_WASMER`, `&WASMER_bootstrap,`, `#endif` in `bootstrap[]` |
| 4 | `include/build_config/SDL_build_config.h.cmake` | `#cmakedefine SDL_VIDEO_DRIVER_WASMER 1` |
| 5 | `CMakeLists.txt` | An option, for example `dep_option(SDL_WASMER_VIDEO "..." ON "SDL_VIDEO;WASI" OFF)`, and in the `WASI` branch `set(SDL_VIDEO_DRIVER_WASMER 1)`, `sdl_glob_sources(.../src/video/wasmer/*)`, `set(HAVE_SDL_VIDEO TRUE)` and `set(HAVE_WASMER_VIDEO TRUE)`. The last one only feeds the configure summary. |

Not needed for a CMake build: the hand-written `include/build_config/SDL_build_config_*.h` (other build systems), `build-scripts/rename_macros.py`, the Xcode and Visual Studio projects.

Optional: public window properties or hints in `include/SDL3/SDL_video.h` and `SDL_hints.h` (Emscripten has `SDL_PROP_WINDOW_EMSCRIPTEN_CANVAS_ID_STRING`), and adding the driver to the `#if` around `SDL_GetCSSCursorName` (v:6443 and `src/video/SDL_video_c.h:62`) if CSS cursor names are useful.

**This list was verified by doing it.** `sdl3-wasmer-driver-skeleton.patch` (5 files, +174, applies on top of `sdl3-wasix.patch`) adds a 158-line `src/video/wasmer/SDL_wasmervideo.c` with a heap framebuffer and no host calls, plus exactly edits 2 to 5. Results:

- Configure reports `Video drivers: dummy wasmer`.
- `SDL_GetVideoDriver` lists `wasmer dummy`.
- `SDL_Init` picks `wasmer` with no hint, and `dummy` with `SDL_VIDEO_DRIVER=dummy`.
- Window creation, `SDL_SetWindowSize`, the software renderer, present, relative mouse mode and the in-process clipboard work. `SDL_MaximizeWindow` fails with "That operation is not supported", as the table predicts for a missing hook.

It is a registration proof, not a driver.

### 2.4 What the driver calls back into SDL

The other half of the interface: internal functions a driver calls from `PumpEvents`. Signatures are copied from `src/events/*_c.h`. A timestamp of 0 lets SDL stamp the event.

```c
// windows (SDL_windowevents_c.h): SDL only updates window->x/y/w/h/flags from these
bool SDL_SendWindowEvent(SDL_Window *window, SDL_EventType windowevent, int data1, int data2);
// keyboard (SDL_keyboard_c.h)
void SDL_AddKeyboard(SDL_KeyboardID keyboardID, const char *name);
bool SDL_SetKeyboardFocus(SDL_Window *window);
void SDL_SetKeymap(SDL_Keymap *keymap, bool send_event);
bool SDL_SendKeyboardKey(Uint64 timestamp, SDL_KeyboardID keyboardID, int rawcode, SDL_Scancode scancode, bool down);
bool SDL_SendKeyboardKeyAndKeycode(Uint64 timestamp, SDL_KeyboardID keyboardID, int rawcode, SDL_Scancode scancode, SDL_Keycode keycode, bool down);
void SDL_SendKeyboardText(const char *text);
void SDL_SendEditingText(const char *text, int start, int length);
void SDL_SendEditingTextCandidates(char **candidates, int num_candidates, int selected_candidate, bool horizontal);
// mouse (SDL_mouse_c.h)
void SDL_AddMouse(SDL_MouseID mouseID, const char *name);
void SDL_SetMouseFocus(SDL_Window *window);
void SDL_SendMouseMotion(Uint64 timestamp, SDL_Window *window, SDL_MouseID mouseID, bool relative, float x, float y);
void SDL_SendMouseButton(Uint64 timestamp, SDL_Window *window, SDL_MouseID mouseID, Uint8 button, bool down);
void SDL_SendMouseWheel(Uint64 timestamp, SDL_Window *window, SDL_MouseID mouseID, float x, float y, SDL_MouseWheelDirection direction);
// touch (SDL_touch_c.h)
int SDL_AddTouch(SDL_TouchID id, SDL_TouchDeviceType type, const char *name);
void SDL_SendTouch(Uint64 timestamp, SDL_TouchID id, SDL_FingerID fingerid, SDL_Window *window, SDL_EventType type, float x, float y, float pressure);
void SDL_SendTouchMotion(Uint64 timestamp, SDL_TouchID id, SDL_FingerID fingerid, SDL_Window *window, float x, float y, float pressure);
// displays, drops, clipboard, application
void SDL_SendDisplayEvent(SDL_VideoDisplay *display, SDL_EventType displayevent, int data1, int data2);
bool SDL_SendDropFile(SDL_Window *window, const char *source, const char *file);
bool SDL_SendDropPosition(SDL_Window *window, float x, float y);
bool SDL_SendDropText(SDL_Window *window, const char *text);
bool SDL_SendDropComplete(SDL_Window *window);
void SDL_SendClipboardUpdate(bool owner, char **mime_types, size_t num_mime_types);
void SDL_SendAppEvent(SDL_EventType eventType);
void SDL_SendQuit(void);
```

Window state is event driven. `SDL_SetWindowSize` only stores the request in `window->pending` and calls the hook. `window->w/h` change when the driver sends `SDL_EVENT_WINDOW_RESIZED` (`src/events/SDL_windowevents.c:123-140`), which also triggers `SDL_EVENT_WINDOW_PIXEL_SIZE_CHANGED`. The same holds for position (`MOVED`), minimise, maximise and restore, focus, mouse enter and leave, and fullscreen enter and leave.

A close button is reported as `SDL_EVENT_WINDOW_CLOSE_REQUESTED`. SDL adds `SDL_EVENT_QUIT` when it was the last top-level window and `SDL_HINT_QUIT_ON_LAST_WINDOW_CLOSE` is on.

The keyboard and mouse state behind these calls is not locked, so they belong on the thread that pumps events.

### 2.5 The Emscripten driver as a reference (`src/video/emscripten/`)

Read only, not built. It sets 27 device pointers:

- `VideoInit`, `VideoQuit`, `GetDisplayUsableBounds`, `SetDisplayMode`, `PumpEvents`
- `CreateSDLWindow`, `SetWindowTitle`, `SetWindowIcon`, `SetWindowSize`, `SetWindowResizable`, `GetWindowSizeInPixels`, `DestroyWindow`, `SetWindowFullscreen`, `SetWindowFillDocument`
- the three framebuffer hooks
- nine `GL_*` hooks
- `free`

and 6 mouse hooks: `CreateCursor`, `ShowCursor`, `FreeCursor`, `CreateSystemCursor`, `SetRelativeMouseMode`, `GetGlobalMouseState`. It has **no** `WaitEventTimeout` or `SendWakeupEvent`, no text-input hooks and no clipboard hooks.

**Event pumping** (`SDL_emscriptenvideo.c`, `SDL_emscriptenevents.c`). `Emscripten_PumpEvents` does nothing except apply a deferred swap interval once. Events are delivered by browser callbacks registered through `emscripten_set_*_callback` and hand-written JS listeners, which call `SDL_Send*` directly between frames. This works only because the browser owns the loop. A WASIX driver has to pull events inside `PumpEvents`.

**Keyboard** (`SDL_emscriptenevents.c`: `Emscripten_HandleKey`, `Emscripten_MapScanCode`).

- Scancode comes from the DOM physical key: `emscripten_compute_dom_pk_code(keyEvent->code)` indexes `emscripten_scancode_table`, with a `switch` for extended codes and a few `keyEvent->key` names.
- Keycode comes from the DOM layout-dependent key. If `keyEvent->key` is exactly one UTF-8 character and `location == 0`, it is decoded and sent with `SDL_SendKeyboardKeyAndKeycode`. Otherwise `SDL_SendKeyboardKey` is used and SDL's default keymap decides. There is no `SDL_SetKeymap` call.
- Lock-key state is resynchronised from `event.getModifierState` with `SDL_SetModState`. A `blur` event calls `SDL_ResetKeyboard`.

**Text input** (`Emscripten_HandleKeyPress`). The `keypress` callback calls `SDL_SendKeyboardText(keyEvent->key)` when `SDL_TextInputActive(window)` and the key is one character. There is no `StartTextInput` or `StopTextInput`, no composition and no `SDL_SendEditingText`: no IME support.

**Clipboard.** None. No clipboard symbol appears anywhere in the directory, so SDL's in-process clipboard is used.

**Fullscreen** (`SDL_emscriptenvideo.c`, `SDL_emscriptenevents.c`). `SetWindowFullscreen` calls `emscripten_request_fullscreen_strategy` or `emscripten_exit_fullscreen`. It returns `SDL_FULLSCREEN_SUCCEEDED`, `SDL_FULLSCREEN_PENDING` (the browser deferred it to the next user gesture) or `SDL_FULLSCREEN_FAILED`. The `fullscreenchange` callback sends `ENTER_FULLSCREEN` or `LEAVE_FULLSCREEN` and calls `SDL_UpdateFullscreenMode`.

**Pointer lock and relative mode** (`SDL_emscriptenmouse.c`, `SDL_emscriptenevents.c`). The mouse hook `SetRelativeMouseMode` calls `emscripten_request_pointerlock(canvas, 1)` or `emscripten_exit_pointerlock()`. A `pointerlockchange` callback tracks `has_pointer_lock`. While locked, motion is sent as relative (`movementX/Y`). A button press while SDL wants relative mode but the lock was lost requests it again.

**Cursor** (`SDL_emscriptenmouse.c`). Cursors are CSS strings. System cursors use `SDL_GetCSSCursorName(id, NULL)`. Custom cursors are converted to RGBA32, drawn to a temporary canvas and turned into a `url(data:...) hot_x hot_y, auto` string. `ShowCursor` assigns the string, or `none`, to `canvas.style.cursor`. A default cursor is installed with `SDL_SetDefaultCursor`.

**Resize and pixel density** (`SDL_emscriptenvideo.c`, `Emscripten_HandleResize`).

- The per-window `pixel_ratio` is `emscripten_get_device_pixel_ratio()` if the window has `SDL_WINDOW_HIGH_PIXEL_DENSITY`, else 1.
- The canvas backing store is `window size * pixel_ratio`; its CSS size is the window size. `GetWindowSizeInPixels` returns `lround(window->w * pixel_ratio)`.
- The window `resize` callback recomputes the size for fill-document, fullscreen or resizable windows and sends `SDL_EVENT_WINDOW_RESIZED`. When the ratio changed it zeroes `window->w/h` first to force the event.
- The display mode is the screen size with `pixel_density` set to the device pixel ratio.

**Window framebuffer** (`SDL_emscriptenframebuffer.c`).

- `CreateWindowFramebuffer` destroys the previous surface, creates an `SDL_Surface` of the window's pixel size in `SDL_PIXELFORMAT_RGBA32`, keeps it in the window data and returns its `pixels` and `pitch`.
- `UpdateWindowFramebuffer` ignores the rectangles. It copies the whole buffer into an `ImageData`, calls `putImageData` on a 2D context, then `emscripten_sleep(0)` if Asyncify is available.
- `DestroyWindowFramebuffer` frees the surface.

**Mouse, touch, pen** (`SDL_emscriptenevents.c`). One set of JS Pointer Event listeners packs events into a C struct.

- Mouse: `SDL_SendMouseMotion` with positions scaled by `window->w / css_width`, `SDL_SendMouseButton`, and `SDL_SendMouseWheel` (pixel deltas / 100, line deltas / 3, page deltas * 80, y inverted).
- Touch: `SDL_AddTouch(1, SDL_TOUCH_DEVICE_DIRECT, "")` and normalised coordinates.
- `pointerenter` and `pointerleave` drive `SDL_SetMouseFocus`, with a fake capture while a button is held.

### 2.6 Displays and pixel density

Window sizes are in window coordinates. Pixel sizes come from the `GetWindowSizeInPixels` hook or, without it, from the display mode's `pixel_density`. `SDL_GetWindowPixelDensity` is the ratio of the two. A driver that wants HiDPI therefore either implements `GetWindowSizeInPixels` (as Emscripten does) or sets `pixel_density` on its display modes. In both cases it must allocate the framebuffer at the pixel size.

### 2.7 Window framebuffer contract and the "no hook" claim

Path: `SDL_GetWindowSurface` calls `SDL_CreateWindowFramebuffer` (v:3647-3701), and `SDL_UpdateWindowSurface` calls `UpdateWindowFramebuffer`. The software renderer is a thin user of this (`src/render/software/SDL_render_sw.c`): `SW_CreateRenderer` calls `SDL_GetWindowSurface(window)` at line 1231, and `SW_RenderPresent` calls `SDL_UpdateWindowSurface(window)` at line 1035.

What SDL needs from the hooks:

- **`CreateWindowFramebuffer(_this, window, &format, &pixels, &pitch)`** returns a pixel format, a pointer and a pitch. SDL wraps them with `SDL_CreateSurfaceFrom(w, h, format, pixels, pitch)`, where `w, h` come from `SDL_GetWindowSizeInPixels`. The buffer must hold at least `h * pitch` bytes at the window's *pixel* size.
- **Ownership.** The driver owns the memory. SDL's surface is only a wrapper and never frees the pixels. The buffer must stay valid until the next `CreateWindowFramebuffer` for that window or `DestroyWindowFramebuffer`.
- **Format.** For the software renderer the bits per pixel must be between 8 and 32 (`SDL_render_sw.c:1162`) and the output colourspace sRGB. In-tree drivers return `SDL_PIXELFORMAT_XRGB8888` (dummy, offscreen), `SDL_PIXELFORMAT_RGBA32` (Emscripten), `SDL_PIXELFORMAT_ABGR8888` (Vita) or the native visual format (X11, Windows). The software renderer adds the framebuffer's format first to its list of supported texture formats.
- **Pitch.** Chosen by the driver. In-tree drivers use the surface pitch or `(w * bytes_per_pixel + 3) & ~3`.
- **Resize.** When the pixel size changes SDL marks the surface invalid (`SDL_OnWindowPixelSizeChanged`, v:4324). The next `SDL_GetWindowSurface` destroys its wrapper and calls `CreateWindowFramebuffer` **again**, with no `DestroyWindowFramebuffer` in between. The hook must free or reuse its previous buffer. Emscripten destroys its old surface at the top of the hook; the dummy driver replaces a surface kept in a window property, which frees the old one.
- **`UpdateWindowFramebuffer(_this, window, rects, numrects)`** receives damage rectangles in pixels. `SDL_UpdateWindowSurface` and the software renderer always pass one full-window rectangle; only `SDL_UpdateWindowSurfaceRects` passes partial ones. This is the present call: nothing else tells the driver a frame is ready.
- **`DestroyWindowFramebuffer`** is called from `SDL_DestroyWindowSurface` (also when a software renderer is destroyed) and during window destruction.
- Optional vsync: `SetWindowFramebufferVSync` and `GetWindowFramebufferVSync` back `SDL_SetWindowSurfaceVSync` and `SDL_GetWindowSurfaceVSync`.

One trap. The *first* `SDL_GetWindowSurface` tries to build the framebuffer as a streaming texture on an accelerated renderer (`ShouldAttemptTextureFramebuffer`, v:3606-3645). It skips the attempt only if the device has `is_dummy` set, the hint `SDL_HINT_FRAMEBUFFER_ACCELERATION` disables it, or the platform is Emscripten. If the attempt succeeds, SDL **overwrites the driver's five framebuffer pointers** with its texture implementations. With only the software renderer compiled in, the attempt fails ("No hardware accelerated renderers available") and the driver's hooks are used.

**Claim: "with no CreateWindowFramebuffer hook and no GPU backend, SDL_CreateRenderer fails". Confirmed**, by source and by running (`verify_nofb.c`, output in 1.7). With both hooks set to NULL on a live device:

- `SDL_GetWindowSurface` returns NULL: "Window framebuffer support not available".
- `SDL_CreateRenderer(window, "software")` fails with the same message.
- `SDL_CreateRenderer(window, NULL)` fails with "Couldn't find matching render driver".

Precisely: it fails when `CreateWindowFramebuffer` *or* `UpdateWindowFramebuffer` is NULL and no accelerated render driver can create a renderer for the window. `SDL_CreateSoftwareRenderer(surface)` on an application-owned surface still works, but has no way to reach the window.

For a WASIX driver that later wants GPU output: SDL 3.4.18's SDL_GPU has only `vulkan`, `d3d12` and `metal` backends (`src/gpu/`), and the render drivers are Direct3D, Metal, OpenGL, GLES2, Vulkan, GPU and software. There is no WebGPU backend in this tree.

### 2.8 `SDL_WaitEventTimeout`

`SDL_WaitEventTimeout(event, ms)` converts to nanoseconds and calls `SDL_WaitEventTimeoutNS` (`src/events/SDL_events.c:1680-1795`):

1. It pumps once (the `PumpEvents` hook plus maintenance) and looks in the queue. A timeout of 0 returns here.
2. If the driver has **both** `WaitEventTimeout` and `SendWakeupEvent` **and** a window that is not being destroyed exists, `SDL_WaitEventTimeout_Device` (line 1594) loops: pump, check the queue, compute the remaining time, store the window in `_this->wakeup_window`, call `WaitEventTimeout(_this, timeoutNS)`, clear `wakeup_window`. A negative timeout means wait forever.
 - Return value greater than 0: something happened; loop again.
 - Return value 0: timed out. `SDL_WaitEventTimeout` returns false without re-checking the clock, so a hook must return 0 only when the full timeout has elapsed, and a positive value for spurious wakeups.
 - Return value less than 0: the backend cannot wait; SDL falls through to polling.
 - The timeout passed to the hook is capped when SDL has things to poll: 1 ms with open joysticks or sensors, 3 s while the joystick subsystem is only enumerating, 50 ms for trays on Unix, 3 s with D-Bus.
3. `SendWakeupEvent(_this, window)` is called by `SDL_SendWakeupEvent` (line 1129). That runs after any event is added to the queue (`SDL_PushEvent` from any thread, line 1213) and when a main-thread callback is queued (line 1431). It first swaps `wakeup_window` to NULL atomically, so the hook runs at most once per wait and only while a wait is in progress or about to start. It must make the pending or next `WaitEventTimeout` return, and it runs on the pushing thread.
4. Without the hooks, or without a window, the fallback loop pumps, checks the queue and calls `SDL_DelayNS(min(remaining, EVENT_POLL_INTERVAL_NS))`. `EVENT_POLL_INTERVAL_NS` is 1 ms (line 60).

Checked by running (`verify_nofb.c`):

- Without hooks, a 200 ms wait called `PumpEvents` 91 times.
- With a semaphore-based pair of hooks installed, it called `PumpEvents` twice and `WaitEventTimeout` once.
- An infinite `SDL_WaitEvent` was ended after 101 ms by exactly one `SendWakeupEvent` when another thread pushed an event at 100 ms.

Only Cocoa, Wayland, Windows and X11 implement these hooks in-tree. `SDL_PumpEvents`, and therefore `PumpEvents`, is expected on the main thread (`SDL_assert(SDL_IsMainThread())`, line 1500).

### 2.9 Text input and IME

Hooks: `StartTextInput`, `StopTextInput`, `UpdateTextInputArea`, `ClearComposition`, and under "screen keyboard" `HasScreenKeyboardSupport`, `ShowScreenKeyboard`, `HideScreenKeyboard`, `SetTextInputProperties`. Sources: v:5805-5921 and `src/events/SDL_keyboard.c:334-396, 764-909`.

The sequence:

1. The application calls `SDL_StartTextInput(window)` or `SDL_StartTextInputWithProperties(window, props)`. SDL copies the properties into `window->text_input_props`, then calls, in this order:
 - `SetTextInputProperties(_this, window, props)`.
 - `ShowScreenKeyboard(_this, window, props)`, if the screen keyboard should auto-show and is not already shown. It auto-shows when `SDL_HINT_ENABLE_SCREEN_KEYBOARD` is true, or is unset or "auto" and no keyboard was registered with `SDL_AddKeyboard`.
 - `StartTextInput(_this, window, props)`, if text input was not already active. A false return fails the call; otherwise `window->text_input_active` becomes true.
2. `SDL_SetTextInputArea(window, rect, cursor)` stores `window->text_input_rect` and `window->text_input_cursor` and calls `UpdateTextInputArea(_this, window)`. The driver reads the two fields to place the IME candidate window.
3. While the user composes, the driver calls `SDL_SendEditingText(text, start, length)` for every change of the preedit string. This posts `SDL_EVENT_TEXT_EDITING` with a copy of the UTF-8 text. `start` and `length` describe the cursor or selection inside it, with -1 for "not set". The Wayland driver passes them in characters, converting its byte offsets with `SDL_utf8strnlen`.
4. Optionally, `SDL_SendEditingTextCandidates(candidates, n, selected, horizontal)` posts `SDL_EVENT_TEXT_EDITING_CANDIDATES`. `(NULL, 0, -1, false)` clears the list.
5. On commit the driver calls `SDL_SendKeyboardText(text)`, which posts `SDL_EVENT_TEXT_INPUT`, and clears the composition with `SDL_SendEditingText("", 0, 0)`. This pattern appears in the Wayland, IBus, Fcitx, Cocoa, Windows and X11 code.
6. `SDL_ClearComposition(window)` calls `ClearComposition(_this, window)`. The driver cancels the host composition and sends the empty editing text.
7. `SDL_StopTextInput(window)` calls `StopTextInput(_this, window)` if active, clears the flag, then calls `HideScreenKeyboard` if the keyboard was auto-shown.
8. On focus changes: when the driver calls `SDL_SetKeyboardFocus`, SDL calls `StopTextInput` for the window losing focus if its text input was active, and `StartTextInput(video, window, window->text_input_props)` for the window gaining focus if its text input is active.

Properties and types:

- `SDL_PROP_TEXTINPUT_TYPE_NUMBER`, an `SDL_TextInputType`: `SDL_TEXTINPUT_TYPE_TEXT`, `_TEXT_NAME`, `_TEXT_EMAIL`, `_TEXT_USERNAME`, `_TEXT_PASSWORD_HIDDEN`, `_TEXT_PASSWORD_VISIBLE`, `_NUMBER`, `_NUMBER_PASSWORD_HIDDEN`, `_NUMBER_PASSWORD_VISIBLE`.
- `SDL_PROP_TEXTINPUT_CAPITALIZATION_NUMBER`: `SDL_CAPITALIZE_NONE`, `_SENTENCES`, `_WORDS`, `_LETTERS`.
- `SDL_PROP_TEXTINPUT_AUTOCORRECT_BOOLEAN`, `SDL_PROP_TEXTINPUT_MULTILINE_BOOLEAN`, `SDL_PROP_TEXTINPUT_ANDROID_INPUTTYPE_NUMBER`.
- Drivers read them with `SDL_GetTextInputType(props)`, `SDL_GetTextInputCapitalization(props)`, `SDL_GetTextInputAutocorrect(props)` and `SDL_GetTextInputMultiline(props)`. These apply the defaults: type text; sentence capitalisation for text and word capitalisation for names; autocorrect on; multiline on unless `SDL_HINT_RETURN_KEY_HIDES_IME` is set.

Rules enforced by SDL:

- `SDL_SendKeyboardText`, `SDL_SendEditingText` and `SDL_SendEditingTextCandidates` are silently dropped unless a window has keyboard focus and its text input is active.
- `SDL_SendKeyboardText` also drops text whose first byte is a control character.
- Key events are independent of all this. The driver keeps sending `SDL_SendKeyboardKey` for physical keys and separately sends the text they produce.
- A screen-keyboard driver reports visibility with `SDL_SendScreenKeyboardShown()` and `SDL_SendScreenKeyboardHidden()`.

None of the IME path was exercised here: **not checked** at run time, since the dummy driver has no such hooks.

### 2.10 Clipboard

SDL prefers the data hooks. The header says so ("If you implement *ClipboardData, you don't need to implement *ClipboardText") and the code in `src/video/SDL_clipboard.c` does it:

- The public text API is built on the data API. `SDL_SetClipboardText(text)` is `SDL_SetClipboardData(SDL_ClipboardTextCallback, SDL_free, SDL_strdup(text), mime_types, n)`. The MIME types come from `GetTextMimeTypes` or default to "text/plain;charset=utf-8". `SDL_GetClipboardText` and `SDL_HasClipboardText` loop over the same types calling `SDL_GetClipboardData` and `SDL_HasClipboardData`.
- `SDL_SetClipboardData` stores the application's callback, cleanup function, userdata and MIME list in the device. Then:
- if `SetClipboardData` is set, it calls `SetClipboardData(_this)`;
- else if `SetClipboardText` is set, it fetches the first "text*" type from the callback and calls `SetClipboardText(_this, text)`, or with "" if there is none;
- else nothing.
It finishes with `SDL_SendClipboardUpdate(true, ...)`.
- `SDL_GetClipboardData(mime, &size)` uses `GetClipboardData` if set; else `GetClipboardText` for "text*" types; else the in-process data from the application's own callback. `SDL_HasClipboardData` follows the same order: `HasClipboardData`, `HasClipboardText`, in-process list.
- A `SetClipboardData` hook takes no data argument. It reads `_this->clipboard_mime_types` and pulls bytes on demand with `SDL_GetInternalClipboardData(_this, mime_type, &size)`.
- `GetClipboardData` returns an `SDL_malloc`ed buffer; `GetClipboardText` returns an `SDL_malloc`ed string.
- When the clipboard changes outside the application, the driver calls `SDL_SendClipboardUpdate(false, mime_types, n)`. That drops the application's callback and records the new type list.
- The primary selection (`SetPrimarySelectionText` and so on) is separate and falls back to a string kept in the device.

All of these calls are synchronous. With no hooks at all the clipboard works inside the process only, as seen with the skeleton driver.

### 2.11 Scancodes

**Confirmed: `SDL_Scancode` values 4 to 231 are the usage ids of USB HID usage page 0x07 (Keyboard/Keypad), with one exception (165).** `include/SDL3/SDL_scancode.h` says the enum is "based on the USB usage page standard" and labels the block "Usage page 0x07". Checked by script against two independent tables:

- Apple's `IOHIDUsageTables.h` from the macOS SDK: 166 values with the same number and the same meaning, covering 0x04 to 0xA4 and 0xE0 to 0xE7.
- The HID Usage Tables transcription in the public repository `IntergatedCircuits/hid-usage-tables`, file `pages/0007-keyboard-keypad.txt`. It also covers 0xB0 to 0xDD: `SDL_SCANCODE_KP_00` = 176 through `SDL_SCANCODE_KP_HEXADECIMAL` = 221.

The usb.org PDF itself was not opened.

Exceptions and extensions:

- `SDL_SCANCODE_UNKNOWN` = 0. HID 0x01 to 0x03 (ErrorRollOver, POSTFail, ErrorUndefined) have no scancode.
- HID 0x82 to 0x84 (Locking Caps, Num and Scroll Lock) are commented out in the header.
- **`SDL_SCANCODE_FRONT` = 165 (0xA5) is not a HID keyboard usage.** 0xA5 to 0xAF are unassigned in that page. It is an SDL addition, commented "Front (Sun keyboards)".
- The gaps follow the HID page: 166 to 175, 222 to 223 and 232 to 256 are unused.
- `SDL_SCANCODE_MODE` = 257 is SDL-specific.
- 258 to 286 are SDL's own numbering of keys *taken from* usage page 0x0C (Consumer). They are not consumer usage ids: `SDL_SCANCODE_SLEEP` = 258, `WAKE` = 259, `CHANNEL_INCREMENT` = 260, `CHANNEL_DECREMENT` = 261, `MEDIA_PLAY` = 262 through `MEDIA_SELECT` = 272, `AC_NEW` = 273 through `AC_BOOKMARKS` = 286.
- 287 to 290 are mobile keys: `SOFTLEFT`, `SOFTRIGHT`, `CALL`, `ENDCALL`.
- `SDL_SCANCODE_RESERVED` = 400. 400 to 500 are reserved for dynamically assigned keycodes; SDL uses them itself in `SDL_SendKeyboardUnicodeKey`.
- `SDL_SCANCODE_COUNT` = 512 is the array bound.

So a host that reports HID keyboard-page usages can pass them through unchanged for 4 to 231. Media and browser keys need a small translation table.

Keycodes are a separate matter. With no keymap set, SDL derives them from a built-in US layout (`SDL_GetDefaultKeyFromScancode`). A driver should either call `SDL_SetKeymap` or use `SDL_SendKeyboardKeyAndKeycode` with the host's layout-aware key, as Emscripten does.

## Risks for a real driver

Ordered by how much they can hurt.

1. **Event model and waiting.** SDL pulls: everything must be delivered from inside `PumpEvents` on the main thread, unlike the Emscripten driver where the browser pushes. To avoid the 1 ms polling loop, the host API needs a blocking "wait for event with timeout" *and* a way to interrupt that wait from another guest thread (`SendWakeupEvent`). If `<wasmer/gui.h>` cannot offer the second, the driver must omit the hooks, or return a negative value from `WaitEventTimeout`, and accept the polling cost: about 450 wakeups per second measured here.
2. **The browser runtime is unverified.** Everything here ran on the native CLI. SDL as built needs real pthreads (timers and `SDL_CreateThread` use them, verified natively) and blocks the calling thread in `SDL_Delay`, semaphores and the wait loop.
3. **Framebuffer cost and semantics.** The software renderer presents a full frame every time, and SDL gives no "frame ready" signal other than `UpdateWindowFramebuffer`. The buffer lives in guest linear memory, so the host either copies it on each present or reads it in place. The create hook is re-entered on every pixel-size change without a destroy call. SDL 3.4.18 has no GPU path that a WASIX host could back.
4. **Keyboard layout and IME.** Scancodes are easy (HID). Correct keycodes on non-US layouts need the host's layout-aware key or a keymap. IME needs composition events and a candidate-window position from the host. The only wasm reference driver, Emscripten, has none, so there is no in-tree model for it on this kind of platform.
5. **Synchronous clipboard.** `GetClipboardData` and `GetClipboardText` must return the data immediately. An asynchronous host clipboard needs a host-side cache or a blocking host call.
6. **Hooks that look optional but are not.**
 - `SetWindowSize`: without it `SDL_SetWindowSize` and the min, max and aspect setters return false.
 - `SetRelativeMouseMode`: no emulation fallback, so first-person style applications fail.
 - `SetWindowResizable`, `SetWindowBordered`, `SetWindowAlwaysOnTop`: the flag cannot change without a hook.
 - `ShowWindow`: once provided, focus events become the driver's job.
7. **Platform identity.** There is no `SDL_PLATFORM_WASI`. `SDL_GetBasePath` returns NULL and `SDL_GetPrefPath` depends on `HOME`. Applications ported to the new driver will trip over these before they reach video code.
8. **Module size.** About 1.0 MB of wasm for SDL alone with the smallest configuration here, and 1.5 MB unless the API exports are suppressed.

## Files in this directory

See [`README.md`](README.md): the files of the spike that are still here are
`sdl3-wasix.patch`, `wasix-toolchain.cmake` and `build.sh`, which has since
grown to build the driver.
