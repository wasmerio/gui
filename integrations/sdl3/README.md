# SDL3 on wasmer-gui

SDL 3.4.18 built for WASIX, with a video driver of its own, `wasmer`, that
gives SDL programs windows, the keyboard, the mouse, touch, text input with
input methods, the clipboard, cursors, fullscreen, relative mouse mode and
the software renderer, on every host of `<wasmer/gui.h>`: native windows,
a browser's canvases, and the headless window system.

Programs written for SDL run unchanged. Dear ImGui's own SDL3 +
SDL_Renderer example is built and run here as it comes from its repository.

| | |
|---|---|
| [`build.sh`](build.sh) | clones SDL at the pinned release, applies the patches, copies the driver in, builds `libSDL3.a` |
| [`sdl3-wasix.patch`](sdl3-wasix.patch) | SDL on WASIX: a `WASI` platform in SDL's CMake, and one function. 3 files, +45 -4 |
| [`sdl3-wasmer.patch`](sdl3-wasmer.patch) | the place for the driver in SDL's build and its list of drivers, and vsync for the software renderer through the driver. 5 files, +43 |
| [`driver/`](driver) | the driver: `src/video/wasmer/` in SDL's tree. About 2,000 lines of C |
| [`wasix-toolchain.cmake`](wasix-toolchain.cmake) | CMake toolchain file for `wasixcc` |
| [`tests/`](tests) | a program that prints every SDL event, scripts that play its user, and what it should print |
| [`imgui/`](imgui) | builds Dear ImGui's example, and a script that clicks and types in it |
| [`NOTES.md`](NOTES.md) | the notes of the port: what SDL needed to build, and what each of its 98 driver hooks does when it is missing |

How each SDL hook maps to the API is in
[`docs/mapping-sdl3.md`](../../docs/mapping-sdl3.md).

## Building

```bash
make sdl             # integrations/sdl3/work/install/lib/libSDL3.a
make test-sdl        # the programs of tests/ on the headless host
make imgui-demo      # integrations/sdl3/imgui/out/imgui_demo.wasm
make test-imgui      # the demo with a script for a user; leaves pictures
```

`make sdl` runs `build.sh`, which needs `wasixcc` 0.4.7 or later, CMake,
Ninja and git, and takes a few minutes the first time. `SDL_WORK=<dir>`
puts the clone and the build somewhere else. `SDL_TESTS=1 ./build.sh` also
builds SDL's own test programs (`work/build/test/*.wasm`).

A program is compiled and linked like this:

```bash
wasixcc -O2 game.c -Iwork/install/include -Lwork/install/lib -lSDL3 -o game.wasm
```

and runs wherever the GUI imports are:

```bash
gui_wasmer --window game.wasm                 # this repository's runner
wasmer run --experimental-gui game.wasm       # the Wasmer CLI, with integrations/wasmer-cli.patch
```

In a browser, with the Wasmer SDK: `sandboxes.create({ gui: { canvas } })`.

A program linked with this SDL imports the GUI functions whether it opens a
window or not, so it needs a host that has them. Where the host has no
window system for it, `SDL_Init(SDL_INIT_VIDEO)` fails with SDL's "No
available video device"; the `dummy` driver is there for programs that want
none (`SDL_VIDEO_DRIVER=dummy`).

## What the driver does

| SDL | On the host |
|---|---|
| Windows: create, title, size and its limits, position, show, hide, raise, minimize, maximize, restore, borders, always on top, flash | the window calls; what a host cannot do (positions on Wayland or in a browser, most window states in a browser) SDL reports as unsupported |
| `SDL_WaitEvent` | the thread sleeps in `wguiInstanceWaitEvents` until there is something to read; `SDL_PushEvent` from another thread wakes it |
| Keyboard | scancodes are the host's physical keys (both are USB usages); keycodes are the key of the layout without modifiers; Caps Lock and Num Lock follow the host |
| Mouse, wheel, touch | pointer records; a pen is a mouse for now |
| `SDL_SetWindowRelativeMouseMode` | the cursor lock. Lent, not owned: when the user ends it (Escape in a browser), the next click asks for it again |
| `SDL_SetWindowMouseGrab` | cursor confinement, where the host has it |
| `SDL_StartTextInput`, `SDL_SetTextInputArea` | text entry at the caret the program names; `SDL_EVENT_TEXT_EDITING` while an input method composes, `SDL_EVENT_TEXT_INPUT` for what it commits and for keys typed directly |
| `SDL_SetClipboardText`, `SDL_GetClipboardText` | the clipboard. Where the host has to ask for its content first (a browser), reading asks and waits up to half a second |
| Cursors | the system's shapes, and images of the program's own |
| `SDL_SetWindowFullscreen` | borderless fullscreen. Answered when the host says it happened: a browser waits for something the user does |
| Displays, their scale and refresh rate | the host's displays; windows are told when they move to another |
| `SDL_GetWindowSurface`, the software renderer | `wguiWindowPresentPixels`. With vsync on, each frame waits for the display |
| Theme, app going to the background, quit requests | the application records |
| Ctrl-C in the program's terminal | SDL's own handler for SIGINT and SIGTERM, which becomes `SDL_EVENT_QUIT`. The signal reaches the program where SDL asks the host for events, so a loop that only polls and draws gets it too |

Three properties and one hint are the driver's own:

| | |
|---|---|
| `SDL.window.create.wasmer.target` (string, for `SDL_CreateWindowWithProperties`) | the target the window adopts: a canvas of the page, by the name the embedder gave it |
| `SDL_VIDEO_WASMER_TARGET` (hint, and so also an environment variable) | the same, for programs that know nothing of targets |
| `SDL.window.wasmer.surface_selector` (string, of a window) | what a WebGPU surface for the window is made with: the selector of `WGPUWasixSurfaceSourceCanvas` |
| `SDL.window.wasmer.window` (number, of a window) | the window's `WGUIWindow` handle |

### Sizes

A window coordinate is one of the host's logical units, as on macOS and in a
browser: a window of 800 by 600 is that big on every display. A window made
with `SDL_WINDOW_HIGH_PIXEL_DENSITY` draws at the display's pixels (1600 by
1200 where the scale factor is 2, which `SDL_GetWindowSizeInPixels` and
`SDL_GetWindowPixelDensity` say). One made without draws one pixel per window
coordinate, and the host stretches its frames. A display's content scale is
1, as on macOS.

### Drawing

SDL has no WebGPU renderer, so its renderer here is the software one. It is
fast enough for interfaces: the Dear ImGui demo at 1280 by 800 takes 8 ms a
frame on the machine this was written on (an M-series Mac), and runs at the
display's 60 frames a second in Chrome.

`SDL_SetRenderVSync(renderer, 1)` is what paces a program: each
`SDL_RenderPresent` then asks the host for a redraw and waits for it, which
comes when the display is ready for the next frame, and not at all while the
window cannot be seen (the wait then ends after 100 ms, so a hidden program
slows to ten frames a second). Unpatched SDL sleeps between the frames of the
software renderer instead, which neither follows the display nor rests while
hidden; `sdl3-wasmer.patch` gives the software renderer a `SetVSync` that
goes through the video driver.

A program that wants the GPU uses `webgpu.h` itself on the window's surface
selector, and SDL for everything else.

### What there is not

- OpenGL, Vulkan and SDL's GPU API. Message boxes. Popup windows, window
  icons, opacity, hit tests, drag and drop, keyboard grabs, global mouse
  positions.
- Audio devices, joysticks and sensors: SDL's dummy backends, so that
  programs asking for those subsystems start. Audio is a project of its own.
- Clipboard content other than text.

### Things to know

- **Paste.** A host lets a program read the clipboard right after its user
  pasted, by the platform's shortcut, unless it was granted more. A program
  compiled for WASIX looks for Control with V, and a Mac's shortcut is
  Command with V: in a browser on a Mac the program has to accept the GUI
  modifier too (`SDL_KMOD_GUI`), as Dear ImGui does when told it runs on one.
- **`SDL_HasClipboardText`** answers true where the host cannot say without
  being asked (a browser): there may be text, and reading finds out.
- **A canvas** takes the size a program asks for, unless the page says its
  layout decides (`gui: { resizable: false }`). The window then has the
  canvas's size, whatever was asked.

## What was checked

On macOS 27 (arm64), against SDL `release-3.4.18`:

- **`make test-sdl`**: [`tests/sdl_events.c`](tests/sdl_events.c) on the
  headless host, ten runs with scripted input: keys with modifiers and
  repeats, the mouse, the wheel, touch, an input method composing and
  committing, dead keys, the clipboard in its three policies and on a host
  that has to be asked, fullscreen, relative mouse mode with the lock taken
  away and given back, resizing, a display of scale factor 2 with and without
  `SDL_WINDOW_HIGH_PIXEL_DENSITY`, and the window surface without a renderer.
  What SDL reports is compared line by line ([`tests/expected`](tests/expected)).
- **SDL's own test suite**, `testautomation` (`SDL_TESTS=1 ./build.sh`, then
  `gui_wasmer work/build/test/testautomation.wasm --audio dummy`): of 377
  tests 361 pass, 12 are skipped (x86 intrinsics, colour spaces the software
  renderer does not have) and 4 fail for want of something outside video:
  three need SDL's virtual joystick driver, which its CMake only builds with
  HIDAPI, and one opens an audio device. The Video (23), Events, Keyboard,
  Mouse, Clipboard and Surface suites pass whole, and Render but for the two
  colour space tests it skips.
- **Dear ImGui** 1.92.9, `examples/example_sdl3_sdlrenderer3`, unchanged
  (`make test-imgui`): driven by [`imgui/demo.json`](imgui/demo.json), which
  clicks a button, turns a slider into a text field with Control and a click,
  types a number into it, opens a tree, scrolls with the wheel and resizes
  the window. The pictures it leaves show each of those having happened.
- **Real windows** (`gui_wasmer --window`, and `wasmer run
  --experimental-gui` with the patched CLI): the test program opens its
  window, shows its frames and ends when a timer on another thread pushes a
  quit event. Input in a real window was not checked by hand: see
  [`docs/manual-checks.md`](../../docs/manual-checks.md).
- **Frames**: `sdl_events.wasm --animate 3000`, a loop that polls and
  presents with vsync, drew 360 frames in 3 seconds in a window on a 120 Hz
  display, three times out of three; without pacing (the headless host's
  default) it draws about 800 a second.
- **Signals**: the test program sends itself SIGINT from SDL's timer
  thread, while it sits in `SDL_WaitEvent` and while it draws frame after
  frame, and leaves through its quit event both times: on the headless host
  (`make test-sdl`), and in Chrome. In wasmer.sh, Ctrl-C ends it and the
  Dear ImGui demo within a tenth of a second.
- **Idle**: a program sitting in `SDL_WaitEvent` for 20 seconds used 0.02 s
  of CPU on the headless host and 0.05 s in a real window; in Chrome, all of
  the browser's processes together used 0.03 s in 6 seconds.
- **In a browser** (`npm run test:browser-gui-sdl` in the Wasmer SDK,
  Chrome): the test program with real key, mouse and wheel input, text
  input, copy and paste, fullscreen and the cursor lock; a loop presenting
  with vsync, at the page's 60 frames a second; and the Dear ImGui demo,
  folded and unfolded with the mouse, its slider dragged, at 57 frames a
  second.

Not checked: Linux and Windows as hosts, browsers other than Chrome.
