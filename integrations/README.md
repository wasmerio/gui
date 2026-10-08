# Integrations

What it takes to use this crate from the projects around it: a patch for the
Wasmer CLI, and the ports of the two libraries most programs reach a window
system through. Both ports are kept as patches against pinned releases, with
their new code as plain files next to them, and a script that puts the two
together and builds.

| | What | State |
|---|---|---|
| [`wasmer-cli.patch`](wasmer-cli.patch) | `wasmer run --experimental-gui` | Builds and runs; see below |
| [`sdl3/`](sdl3/README.md) | SDL3 on WASIX, with a video driver for this API | Runs SDL's own tests and Dear ImGui's SDL3 example, natively and in Chrome |
| [`winit/`](winit/README.md) | winit and softbuffer backends for this API | Run winit's own `window` example, natively and in Chrome |

## `wasmer-cli.patch`: `wasmer run --experimental-gui`

Made against wasmerio/wasmer at `0df5d2ee6c6` (7.4.2), on top of
wasmer-webgpu's own patch (`integrations/wasmer-cli.patch` there): GUI
windows are what WebGPU guests draw on, so the two go together.

1. Check both repositories out under `lib/`:

   ```sh
   git submodule add https://github.com/wasmerio/webgpu lib/webgpu
   git submodule add https://github.com/wasmerio/gui lib/gui
   ```

2. Apply the patches in order and let Cargo record the new dependencies:

   ```sh
   git apply lib/webgpu/integrations/wasmer-cli.patch
   git apply lib/gui/integrations/wasmer-cli.patch
   cargo check -p wasmer-cli --features gui-window
   ```

What it changes:

- **`Cargo.toml`** excludes `lib/gui` from the workspace (it is a workspace of
  its own) and points `wasmer-webgpu` at `lib/webgpu`: this crate names it by
  version, like its Wasmer dependencies.
- **`lib/cli/Cargo.toml`** adds the optional dependency and two features:
  `gui` (guests get the imports; their windows are headless) and `gui-window`
  (their windows open on the desktop, with the system clipboard; WebGPU
  guests draw on them, which turns `webgpu` on, and so do guests without a
  GPU, with frames of their own).
- **`lib/cli/src/commands/run/mod.rs`** adds `--experimental-gui`. A module
  that imports the GUI without the flag is refused with a message that names
  it. With `gui-window` the command runs on a second thread so that the main
  one can serve the window system, which is only started when a guest opens a
  window. A user closing a window asks the guest to close; a guest that does
  not even read the request within two seconds is taken to be stuck, and the
  run ends. A smoke test runs a guest through the wrapped runtime.

The two flags are independent, as the two APIs are: a guest that draws on its
window with WebGPU needs both.

```sh
wasmer run --experimental-gui --experimental-webgpu triangle.wasm
```

With `--experimental-gui`, the windows are this crate's, and a WebGPU surface
that names none of them is drawn offscreen. Without it,
`--experimental-webgpu` behaves as before the patch.

The features are opt-in: add `gui-window` to the features a release build
enables to ship it.

### What was checked

Against a scratch clone at that commit, with both patches applied:

- `cargo check -p wasmer-cli` with no extra features, with `gui`, with
  `webgpu`, with `gui,webgpu-window` and with `gui-window,webgpu-window`.
- `cargo test -p wasmer-cli --features gui,webgpu,cranelift --lib
  runtime_wrapper_runs_wasix_guest`: the GUI and the WebGPU smoke tests.
- The built `wasmer` running this repository's test programs, headless and in
  windows (see the top-level README for how to build them).

The patch needs wasmer-webgpu's `WindowTarget` to have `pre_present` and
`wait_presentable`, which are newer than its revision `f1c1d4e`.
