# winit and softbuffer on wasmer-gui

winit 0.30.13 and softbuffer 0.4.8 for WASIX (`wasm32-wasmer-wasi`), each
with a backend for `<wasmer/gui.h>`: winit's windows and events come from
the Wasmer runtime, and softbuffer's frames go to it. A program written for
winit runs unchanged in native windows, in a browser's canvases, and on the
headless window system. winit's own `window` example is built and run here
as it comes from its repository.

These are not the browser builds of the two crates: there is no
wasm-bindgen and no JavaScript in the program. The same `.wasm` file runs
under `wasmer run` and in a page.

| | |
|---|---|
| [`build.sh`](build.sh) | clones winit and softbuffer at the pinned releases, applies the patches, copies the backends in, builds winit's `window` example and the test program |
| [`winit-wasix.patch`](winit-wasix.patch) | the place for the backend in winit: a cfg alias, the platform module, one dependency. 4 files, +21 -3 |
| [`backend/`](backend) | winit's backend: `src/platform_impl/wasix/`. About 2,400 lines, 500 of them the generated key tables |
| [`platform/wasix.rs`](platform/wasix.rs) | `winit::platform::wasix`: what only this platform has |
| [`softbuffer-wasix.patch`](softbuffer-wasix.patch) | the place for the backend in softbuffer. 4 files, +31 -7 |
| [`softbuffer/wasix.rs`](softbuffer/wasix.rs) | softbuffer's backend: `src/backends/wasix.rs` |
| [`tests/`](tests) | a program that prints every winit event, scripts that play its user, and what it should print |
| [`example/window.json`](example/window.json) | a script that walks winit's `window` example through its key bindings |
| [`NOTES.md`](NOTES.md) | the notes of the port: what a winit backend is made of, and what each of its calls needs |

Both backends call the host through [`wasmer-gui-sys`](../../sys), the
declarations of the header for Rust. How each winit call maps to the API is
in [`docs/mapping-winit.md`](../../docs/mapping-winit.md).

## Building

```bash
make winit-programs   # integrations/winit/example/out/window.wasm, tests/out/winit_events.wasm
make test-winit       # the test program with scripts for a user, and winit's example walked through
```

`build.sh` needs git and [cargo-wasix](https://github.com/wasix-org/cargo-wasix)
with its toolchain. `WINIT_WORK=<dir>` puts the clones and the build
somewhere else.

An application names winit and softbuffer as it would anywhere, and says
where they come from:

```toml
[dependencies]
winit = "0.30"
softbuffer = "0.4"

[patch.crates-io]
winit = { path = "<work>/winit" }
softbuffer = { path = "<work>/softbuffer" }
wasmer-gui-sys = { path = "<wasmer-gui>/sys" }
```

```bash
CARGO_WASIX_NO_REGISTRY_CONFIG=1 cargo wasix build --release
gui_wasmer --window target/wasm32-wasmer-wasi/release/app.wasm
wasmer run --experimental-gui target/wasm32-wasmer-wasi/release/app.wasm
```

(`CARGO_WASIX_NO_REGISTRY_CONFIG=1`: cargo-wasix otherwise points Cargo at
a registry of crates forked for WASIX, which lacks what winit depends on for
its other platforms, and Cargo resolves those too.)

## What the backend does

| winit | On the host |
|---|---|
| `EventLoop`, `ControlFlow::{Wait, WaitUntil, Poll}` | the loop sleeps in `wguiInstanceWaitEvents` until there is a record, the time has come, or it is woken; `EventLoopProxy::send_event` wakes it from any thread |
| `Window` | a host window. Every method works from every thread: handles are numbers that do, and the host answers what a window is like from what it last saw of it |
| `request_redraw` | `wguiWindowRequestRedraw`: `RedrawRequested` comes when the display is ready for the next frame, once per refresh, and not while the window cannot be seen |
| `Resized`, `ScaleFactorChanged` (with its size writer), `Moved`, `Focused`, `Occluded`, `ThemeChanged`, `CloseRequested`, `Destroyed` | the window records |
| `KeyboardInput` | physical keys by name (both sides follow the W3C `code` table), the logical key with and without modifiers, text on the key event, repeats, and the releases the host makes up when a window loses the keyboard as synthetic ones |
| `ModifiersChanged` | the state, and which side's keys where the host can tell |
| `Ime` | `set_ime_allowed` is text entry on the host; `Preedit` and `Commit` are what an input method composes and commits, with an empty `Preedit` before a commit. What a key types directly stays on its key event |
| `CursorMoved`, `CursorEntered`, `CursorLeft`, `MouseInput`, `MouseWheel` | the mouse's pointer records; lines or pixels for the wheel, the way winit counts them |
| `Touch` | pointer records of fingers, with pressure where there is any |
| `DeviceEvent::MouseMotion` and the other device events | what devices report, where the host passes it on: what a locked cursor is steered by |
| `set_cursor_grab` | confined or locked, each only where the host has that kind |
| `set_cursor`, custom cursors, `set_cursor_visible`, `set_cursor_position` | the cursor calls |
| `set_fullscreen` | borderless. A video mode is asked for as its display: the host has one mode a display |
| Monitors | the host's displays: name, size, position, scale factor, refresh rate |
| `set_title`, sizes and their limits, visible, resizable, minimized, maximized, decorations, level, focus, attention | the window calls. Where the host cannot (a canvas has no title bar), nothing happens, and the getters say what is |
| raw-window-handle | `RawWindowHandle::Web` with the host's handle of the window as its id: raw-window-handle has nothing for WASIX, and the web's pair is the one that is plain data. softbuffer's backend here reads it |
| Ctrl-C in the program's terminal | SIGINT, and SIGTERM, end the program at once with status 128 plus the signal's number, unless the program installed a handler itself. The backend installs this one because wasix-libc's default actions do not work ([`backend/signals.rs`](backend/signals.rs)) |

`winit::platform::wasix` adds `WindowAttributesExtWasix::with_target` (the
target a window adopts: one of the page's canvases, by the name the embedder
gave it), `WindowExtWasix::surface_selector` (what a WebGPU surface for the
window is made with) and `host_window` (the `WGUIWindow`).

softbuffer's buffer is the program's own memory, in the format the host
takes as it is (`0x00RRGGBB`, which is blue first in memory).
`present` hands it to `wguiWindowPresentPixels`, which copies it during the
call, and `present_with_damage` says what changed.

### What there is not

- OpenGL. Window icons, resize increments, drag and resize by the program,
  hit tests, window menus, blur, content protection, themes set by the
  program, exclusive fullscreen.
- Drag and drop, gestures, gamepads: the API has room for them, and no
  calls yet.

## What was checked

On macOS 27 (arm64), with cargo-wasix 0.1.34:

- **`make test-winit`**: [`tests/winit_events`](tests/winit_events/src/main.rs)
  on the headless host, five runs with scripted input: keys with modifiers
  and repeats, dead keys, an input method composing and committing,
  the mouse and the wheel, fingers, fullscreen, the cursor lock with device
  motion, a display of scale factor 2, resizing, a theme change. What winit
  reports is compared line by line ([`tests/expected`](tests/expected)).
- **winit's `window` example**, unchanged, walked through its key bindings
  by [`example/window.json`](example/window.json): the dark theme, fullscreen
  and back, the three cursor grab modes, the next cursor and a custom one
  (decoded from its PNG), a hidden cursor, maximize, a requested resize,
  decorations, resizable, input methods off and on with a composition, a
  second window, and both closed. The script checks what the example asked
  of the host at each step, and what its windows showed: white, and its dark
  grey once the theme was changed.
- **Real windows** (`gui_wasmer --window`): the test program opens a window
  of 640 by 480 points on a display of scale factor 2, draws it, and ends
  when another thread sends the loop an event; with `--animate` it drew 361
  frames in 3 seconds on a 120 Hz display. winit's example opens its window
  too. Input in a real window was not checked by hand.
- **Idle**: 20 seconds in the loop's wait used no measurable CPU.
- **Signals**: the test program sends itself SIGINT from another thread,
  while its loop waits and while it draws frame after frame, and ends with
  status 130 both times: on the headless host (`make test-winit`), and in
  Chrome.
- **In a browser** (`npm run test:browser-gui-winit` in the Wasmer SDK,
  Chrome): the test program with real key, mouse and wheel input, an input
  method, fullscreen and the cursor lock; a loop that waits and one that
  draws every frame (60 a second there); and winit's `window` example,
  switched to its dark theme with its own key binding.
- **In wasmer.sh**, started from its shell: both programs open their window
  when the terminal is resized for it, and Ctrl-C ends them within a tenth
  of a second, waiting or drawing.

Not checked: Linux and Windows as hosts, browsers other than Chrome, and
winit 0.31, which reshapes the traits a backend implements
([`NOTES.md`](NOTES.md), section 6).
