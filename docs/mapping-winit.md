# winit on wasmer-gui

What a winit platform backend for WASIX needs, and which call of
`<wasmer/gui.h>` serves it. Read at winit `v0.30.13`; the notes behind this,
with file and line for every statement, are in
[`integrations/winit/NOTES.md`](../integrations/winit/NOTES.md).

## State

The backends exist: [`integrations/winit`](../integrations/winit/README.md)
has winit's (`backend/`) and softbuffer's (`softbuffer/`), built into winit
0.30.13 and softbuffer 0.4.8 for `wasm32-wasmer-wasi` by its `build.sh`.
winit's own `window` example runs on them unchanged: on the headless host
walked through its key bindings by a script, in a native window, and in
Chrome. The README there says what each call does and what was checked.

Three things the spike found that shape the port:

- `cargo wasix build` fails for winit 0.30 unless
  `CARGO_WASIX_NO_REGISTRY_CONFIG=1` is set: the WASIX registry overlay does
  not carry the `rustix` version winit's Linux-only dependencies name, and
  Cargo resolves every target.
- Unpatched, both crates take WASIX for the Web: softbuffer compiles with its
  Web backend and panics inside `js-sys` when run.
- winit 0.31 moves backends behind traits in `winit-core`, which builds for
  WASIX as it is. A backend there can be a crate of its own.

## The event loop

| winit | Served by |
|---|---|
| `EventLoop::run_app`, one iteration: `new_events`, events, redraws, `about_to_wait` | a loop over `wguiInstancePollEvents` |
| `ControlFlow::Poll` | `wguiInstanceWaitEvents` with timeout 0, or none |
| `ControlFlow::Wait` | `wguiInstanceWaitEvents(WGUI_WAIT_FOREVER)` |
| `ControlFlow::WaitUntil` | `wguiInstanceWaitEvents` with the time left |
| `StartCause` | the wait's reasons: none is `ResumeTimeReached`, any is `WaitCancelled` |
| `EventLoopProxy::send_event` (0.30), `wake_up` (0.31) | `wguiInstanceWake`, with the events in the backend's own queue |
| `Window::request_redraw` from any thread | `wguiWindowRequestRedraw` |
| `available_monitors`, `primary_monitor` | `wguiInstanceGetDisplays`, `wguiDisplayGetInfo`, `wguiDisplayGetName` |
| `listen_device_events` | `wguiInstanceSetEventMask` with `WGUIEventClass_Raw` |
| `system_theme` | `wguiInstanceGetInfo` |
| `create_custom_cursor` | `wguiInstanceCreateCursor` |
| `create_window` | `wguiInstanceCreateWindow` |

An interrupted wait (`WGUIWaitReasons_Interrupted`) is answered inside the
backend: the loop goes around once, as after any wake. Signals reach the
program where the loop asks the host for events and where its wait returns.
winit has no say about signals on any platform, and here a program without a
handler of its own would not survive one (see
[Signals and stopping](architecture.md#signals-and-stopping)), so the backend
installs one for SIGINT and SIGTERM when an event loop is made: it ends the
program at once, with status 128 plus the signal's number. A handler the
program installed itself is left alone.

## The window

winit's getters return plain values on any thread. They are answered from
`wguiWindowGetState`, which never waits for the window system. Seven methods
return a `Result` and can say "not supported" on the spot; the backend
decides from `wguiHasFeature`.

| winit | Served by | |
|---|---|---|
| `id` | the `WGUIWindow` | yes |
| `scale_factor`, `inner_size`, `outer_size`, `is_visible`, `is_minimized`, `is_maximized`, `is_resizable`, `is_decorated`, `has_focus`, `fullscreen`, `theme`, `current_monitor` | `wguiWindowGetState` | yes |
| `inner_position`, `outer_position` | `wguiWindowGetState`; `NotSupported` when the position is `WGUI_POSITION_UNDEFINED` | yes |
| `request_inner_size` | `wguiWindowSetSize`; returns `None`, and `Resized` follows | yes |
| `set_min_inner_size`, `set_max_inner_size` | `wguiWindowSetSizeLimits` | yes |
| `set_outer_position` | `wguiWindowSetPosition` | yes, where `WindowPosition` |
| `set_title` | `wguiWindowSetTitle` | yes |
| `title` | what the backend last set | guest side |
| `set_visible`, `set_resizable`, `set_minimized`, `set_maximized`, `set_decorations`, `set_window_level` | `wguiWindowSetFlags` | yes, where `WindowStates` |
| `set_fullscreen(Borderless)` | `wguiWindowSetFullscreen` | yes |
| `set_fullscreen(Exclusive)` | borderless on the mode's display: the host has one mode a display | switching modes: later |
| `focus_window` | `wguiWindowFocus` | yes |
| `request_user_attention` | `wguiWindowRequestAttention` | yes |
| `set_cursor` | `wguiWindowSetCursorShape`, `wguiWindowSetCustomCursor` | yes |
| `set_cursor_visible` | `wguiWindowSetCursorVisible` | yes |
| `set_cursor_position` | `wguiWindowSetCursorPosition`; `NotSupported` without `CursorWarp` | yes |
| `set_cursor_grab` | `wguiWindowSetCursorGrab`; `NotSupported` without the feature for that mode, a later refusal as `WindowCursorGrabChanged` | yes |
| `set_ime_allowed`, `set_ime_cursor_area`, `set_ime_purpose` | `wguiWindowSetTextInput` | yes |
| `pre_present_notify`, `reset_dead_keys` | nothing to do | yes |
| `set_cursor_hittest`, `drag_window`, `drag_resize_window`, `show_window_menu` | `NotSupported` | later |
| `set_transparent` | `wguiWindowSetFlags` (`Transparent`) | yes, where the host has such windows |
| `set_window_icon`, `set_theme`, `set_resize_increments`, `set_enabled_buttons`, `set_content_protected`, `set_blur` | no-ops, as on other platforms | later |
| `window_handle`, `display_handle` | `RawWindowHandle::Web` carrying the `WGUIWindow`, until raw-window-handle has a variant for it | see below |

## Events

| winit | From |
|---|---|
| `Resized`, `ScaleFactorChanged` | `WindowResized`; a size written to `InnerSizeWriter` becomes `wguiWindowSetSize` |
| `Moved` | `WindowMoved` |
| `CloseRequested` | `WindowCloseRequested`, `WindowLost`; `AppQuitRequested` for every window |
| `Destroyed` | the backend, when it destroys the window |
| `Focused` | `WindowFocusGained`, `WindowFocusLost` |
| `Occluded`, `ThemeChanged` | `WindowFlagsChanged`, `WindowThemeChanged` |
| `RedrawRequested` | `WindowRedrawRequested`, and after every `Resized`: a window of another size has nothing drawn in it yet |
| `KeyboardInput` | `KeyDown`, `KeyUp`; `is_synthetic` is the record's `Synthetic` flag |
| `ModifiersChanged` | `ModifiersChanged`, with the keys held on each side |
| `Ime::Enabled`, `Ime::Disabled` | `TextInputState` |
| `Ime::Preedit` | `TextComposition` (already in bytes) |
| `Ime::Commit` | `TextCommit` without `FromKey`, preceded by an empty preedit |
| `CursorMoved`, `CursorEntered`, `CursorLeft`, `MouseInput` | the mouse's pointer records; where the cursor is, is said once |
| `Touch` | pointer records of kind `Touch` |
| `MouseWheel` | `Wheel` (winit's signs are the opposite) |
| `DeviceEvent::MouseMotion`, `MouseWheel`, `Button`, `Key` | the raw records; on a host without them, the motion of a locked pointer |
| `PinchGesture`, `PanGesture`, `RotationGesture`, `DoubleTapGesture`, `TouchpadPressure` | later |
| `DroppedFile`, `HoveredFile`, `HoveredFileCancelled` | later |
| `ActivationTokenDone`, `AxisMotion` | no |

## Keys

winit's `KeyCode` is the W3C `code` vocabulary, as `WGUIPhysicalKey` is. The
differences are winit's own names for three keys (`SuperLeft` and `SuperRight`
for `MetaLeft` and `MetaRight`, `Meta` for the legacy `Super`) and `F25` to
`F35`, which have numbers of their own here. `NamedKey` is the W3C `key`
vocabulary, as the named `WGUIKey` values are, except that winit names the
space bar.

| `KeyEvent` field | From |
|---|---|
| `physical_key` | `physicalKey`, or `Unidentified` |
| `logical_key` | `key`: character, named, or dead, by its tag; `keyString` for a sequence |
| `text` | `text` (winit's backends differ on whether a release carries it; here it does not) |
| `location`, `repeat`, `state` | `location`, `WGUIKeyFlags_Repeat`, the record's type |
| `key_without_modifiers` (`KeyEventExtModifierSupplement`) | `keyUnmodified` |
| `text_with_all_modifiers` | `textWithAllModifiers` |

## Text

winit never says in so many words that one input does not arrive as both
`KeyEvent.text` and `Ime::Commit`; each backend keeps to it. Here the two are
told apart by the record: with text input on, a typed key is a `KeyDown` with
its text and then a `TextCommit` flagged `FromKey`. The backend drops commits
with that flag, and delivers the others as `Ime::Commit`. A key an input
method took for its composition arrives flagged `Composing`, and is not
delivered.

## Window handles

raw-window-handle 0.6 has no variant for a WASIX window, and its enums are
non-exhaustive, so one can be added upstream. Until then the handle is
`RawWindowHandle::Web(WebWindowHandle::new(id))` with the `WGUIWindow` as the
id (never 0) and `RawDisplayHandle::Web`. On a WASIX target the only code that
reads that variant is code that takes WASIX for the Web, which has to be
patched anyway. A graphics crate turns the id into a surface selector with
`wguiWindowGetSurfaceSelector`.

## softbuffer

One backend file. `present_with_damage` becomes the pixel-buffer call of
phase 4: window, pointer, width, height, format, stride and damage
rectangles. The frame already lives in guest memory, so presenting is one
copy on the host's side. softbuffer's next release changes its pixel format
on wasm targets from `0RGB` words to RGBA bytes; the call takes a format so
that either works.
