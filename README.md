# wasmer-gui

Windows, input, text entry and the clipboard for WASIX programs running on
[Wasmer](https://wasmer.io): what a program needs, besides a GPU, to have a
user in front of it.

A guest includes one header, `<wasmer/gui.h>`, and calls plain C functions:

```c
#include <wasmer/gui.h>

WGUIInstance instance;
WGUIWindow window;
wguiCreateInstance(NULL, &instance);

WGUIWindowDescriptor descriptor = WGUI_WINDOW_DESCRIPTOR_INIT;
descriptor.title.data = "Hello";
wguiInstanceCreateWindow(instance, &descriptor, &window);

uint64_t buffer[WGUI_MAX_EVENT_SIZE / 8];
for (int running = 1; running;) {
    WGUIWaitReasons reasons;
    wguiInstanceWaitEvents(instance, WGUI_WAIT_FOREVER, &reasons);
    size_t written = 0;
    wguiInstancePollEvents(instance, buffer, sizeof buffer, &written);
    const WGUIEventHeader* event = wguiEventFirst(buffer, written);
    for (; event != NULL; event = wguiEventNext(buffer, written, event)) {
        if (event->type == WGUIEventType_WindowCloseRequested) running = 0;
    }
}
```

There is no library to link: every function of the header is a WebAssembly
import of the module `wasmer_gui_v0`, and this crate is what provides them.
To draw, a program passes the name `wguiWindowGetSurfaceSelector` gives it to
[wasmer-webgpu](https://github.com/wasmerio/webgpu) as its surface's canvas,
or, without a GPU, hands the frames it drew itself to
`wguiWindowPresentPixels`.

## Status

Under construction. What works today:

- the API: `include/wasmer/gui.h`, generated from `spec/gui.json`;
- the host that every window system shares: the event queue and its rules,
  waiting and waking, requests, the clipboard, limits and permissions;
- the headless window system, where windows work and nobody sees them, with
  scripted input for tests;
- native windows through [winit](https://docs.rs/winit) (feature `winit`),
  with the system clipboard (feature `clipboard`);
- drawing on either kind of window with wasmer-webgpu (feature `webgpu`), or
  without a GPU, with frames the program drew itself (natively through
  softbuffer, feature `pixels`);
- the browser, where a window is a canvas of the page (feature `js`): the
  keyboard, pointers, the wheel, text entry with input methods, the
  clipboard, fullscreen and the cursor lock;
- `SandboxBuilder::gui` and `gui::run_main` in the Wasmer SDK for Rust,
  `sandboxes.create({ gui })` in the one for JavaScript, and
  `wasmer run --experimental-gui` as a patch for the Wasmer CLI
  (`integrations/`);
- SDL3 on top of it: a video driver for this API
  ([`integrations/sdl3`](integrations/sdl3/README.md)), with which SDL
  programs run unchanged on every host above. SDL's own test suite and Dear
  ImGui's SDL3 example are what it is held to;
- winit and softbuffer on top of it
  ([`integrations/winit`](integrations/winit/README.md)): backends for
  WASIX, with which winit's own `window` example runs unchanged, and
  [`wasmer-gui-sys`](sys), the header for Rust, which they call the host
  through.

Native windows have been run on macOS. What a keyboard, a mouse, an input
method and the clipboard do in them has to be checked by a person:
`docs/manual-checks.md` is the list. Linux and Windows are built and tested
by CI only. The browser host is tested in Chrome, with real input; Firefox
and Safari have not been tried.

SDL3 and winit running on it is what the API was held open for. With both
there, `wasmer_gui_v0` only grows: new calls, new records, new fields at the
end of a chain. What would break a program built today waits for a
`wasmer_gui_v1`, served next to this one.

## How the API is meant

- **Nothing is called back.** What happens arrives as event records that the
  program drains, or waits for. Requests that take a while (fullscreen, a
  cursor grab, a clipboard read) complete as a record carrying a tag the
  program chose, exactly once.
- **Every function answers with a status.** A host that does not have a
  function answers 0, `WGUIStatus_Unimplemented`, and does nothing: a program
  built against a newer header starts on an older host and finds out what is
  missing when it asks.
- **What a host cannot do, it says.** Features are asked for
  (`wguiHasFeature`), never assumed, and nothing is imitated: a host without
  a way to confine the cursor does not lock it instead.
- **What a platform wants answered on the spot is declared ahead.** A program
  that polls cannot refuse a browser's default action in time, or say where
  its caret is when an input method asks. It states these things beforehand.
- **Pixels are physical**, with the scale factor beside them.
- **Records say how long they are.** They can grow, and a program steps over
  the kinds it does not know.

## Embedding

```rust
let gui = wasmer_gui::GuiCtx::default();           // headless
runtime.with_instantiation_hook(gui.runtime_hooks());
```

A `GuiCtx` holds the window system, what guests may do (`GuiPolicy`) and how
much (`GuiLimits`). An embedder that runs several programs makes a
`GuiScope` for each: closing a scope stops that program where it waits for
input, and takes its windows away.

### Windows on the desktop

A window system's event loop wants the process's main thread, so the
embedder's own work moves to another one:

```rust
fn main() -> anyhow::Result<()> {
    let (main_thread, windows) = wasmer_gui::native::GuiMainThread::new();
    // WebGPU guests draw on the same windows.
    let webgpu = wasmer_webgpu::WebGpuCtx::builder()
        .surface_provider(windows.surfaces(None))
        .build();
    let gui = wasmer_gui::GuiCtx::builder()
        .window_system(std::sync::Arc::new(windows))
        .build();
    main_thread.run(move || run_guests(gui, webgpu))?;
    Ok(())
}
```

Nothing touches the display until a guest opens a window, so a program that
never does runs without one. In the Wasmer SDK, `wasmer_sdk::gui::run_main`
does the same for an asynchronous `main`.

A window system of your own (a stream to a remote viewer, say) implements
`wasmer_gui::system::WindowSystem`. The headless one is the smallest example,
and lets its embedder watch windows open (`HeadlessSystem::on_window`) and
keep what is drawn on them.

### Windows that are views of your interface

An app, or a page, has an interface of its own, with a thread that must never
wait. `wasmer_gui::views::ViewSystem` is the window system for that: what
guests ask for is queued as `ViewCommand`s for the interface's thread to take
when it gets to them, and what the user does is reported through sinks that
never wait.

### In a browser

Built for `wasm32-unknown-unknown` with the `js` feature, as part of an
embedder that is itself WebAssembly (the Wasmer SDK), a window is a canvas:

```rust
// On the page's thread. `page` holds the embedder's JavaScript functions
// that find the canvases; see `createGuiPage` in js/gui_page.js.
let windows = wasmer_gui::web::open(scope, &page)?;
let gui = GuiCtx::builder().window_system(Arc::new(windows)).build();
```

`js/gui_page.js` is everything that touches the DOM, and ships with the crate.
What the user does on a canvas reaches a guest that waits for it through the
embedder's shared memory, without a message and without JSPI. See
`docs/architecture.md`.

A guest that waits for input blocks its worker meanwhile, and with it
whatever else completes there, such as WebGPU's callbacks. An embedder that
enters its guests with `Function::call_async` can let the wait suspend
instead, and hand the context's waker to that other work. Where the engine
has no JSPI or no `Atomics.waitAsync`, waits block as before; a guest
entered synchronously traps at its first wait.

```rust
let gui = GuiCtx::builder()
    .window_system(Arc::new(windows))
    .suspending_waits(true)
    .build();
let wake = gui.event_waker(); // for wasmer-webgpu's event waker, say
```

## Layout

| | |
|---|---|
| `spec/` | The API (`gui.json`) and the pinned W3C key tables (`keys.json`) |
| `codegen/` | Generates everything below marked `.gen.` and the headers |
| `include/wasmer/` | `gui.h`, and `gui_names.h` with the names of its values |
| `sys/` | `wasmer-gui-sys`: the header for Rust programs, generated, a crate of its own |
| `src/abi/` | The header as the host reads it: layouts, records, import glue |
| `src/host/` | The API's implementation, shared by all window systems |
| `src/system.rs` | The trait a window system implements |
| `src/headless.rs`, `src/script.rs` | The headless window system and scripted input |
| `src/native/` | Native windows: the one place that uses winit (and softbuffer, for frames without a GPU) |
| `src/views.rs` | Windows that are views of an interface whose thread may not wait |
| `src/web.rs`, `js/gui_page.js` | The browser: the page's thread, and everything that touches the DOM |
| `src/surface.rs`, `src/native/surface.rs` | Windows as something wasmer-webgpu draws on |
| `tests/` | Test programs, the scripts that play their users, and what they should print |
| `examples/` | A triangle with a window, input and WebGPU |
| `docs/` | The architecture, what SDL3 and winit need of this API, and the checks a person makes |
| `integrations/` | The Wasmer CLI patch, SDL3 with its video driver for this API, and winit and softbuffer with their backends for it |

## Working on it

```bash
make check          # generated files, formatting, lints, the header against the C compiler
make test           # unit tests, the ABI sweep, the test programs on the headless host
make test-gpu       # windows drawn on with WebGPU, on the headless host (needs a GPU adapter)
make test-windows   # the programs that need nobody at the keyboard, in real windows
make test-page      # the page half in the installed Chrome, with a core that only listens
make test-sdl       # SDL3 built for WASIX with its driver, and what SDL makes of scripted input
make test-imgui     # Dear ImGui's SDL3 example, with a script that clicks and types in it
make test-winit     # winit and softbuffer built for WASIX with their backends, and winit's own example
```

The header checks and the test programs need
[wasixcc](https://github.com/wasix-org/wasixcc); what draws needs
wasmer-webgpu checked out next to this repository (`../webgpu`). The SDL
targets also need CMake and Ninja, and the winit ones
[cargo-wasix](https://github.com/wasix-org/cargo-wasix); both clone what
they build, at pinned releases.
`test-windows` opens its windows behind what you are working in. A window
nobody can see has no frames to give, so behind a locked screen its drawing
part is reported as skipped.

`gui_wasmer` is a small stand-in for `wasmer run`. It runs a program on the
headless host, with a script playing the user, or in real windows:

```bash
cargo run --features cli --bin gui_wasmer -- --script tests/scripts/basics.json tests/programs/out/events.wasm
cargo run --features cli,winit,clipboard,webgpu,pixels --bin gui_wasmer -- --window --webgpu examples/out/triangle.wasm
```

A script can also walk a program that draws frame after frame through its
frames, and keep pictures of what its window showed (`--screenshots <dir>`):
`integrations/sdl3/imgui/demo.json` does both.
