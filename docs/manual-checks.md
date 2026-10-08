# Checks that need a person

The test suite plays the user on the headless window system, and opens real
windows only for what needs nobody at the keyboard. What a real keyboard,
mouse, input method and clipboard do in a real window has to be looked at.
This is the list, for each desktop the native window system is released on.

Build first:

```bash
make programs gpu-programs examples
cargo build --features cli,winit,clipboard,webgpu,pixels --bin gui_wasmer
```

## 1. Events, as the guest sees them

```bash
./target/debug/gui_wasmer --window tests/programs/out/events.wasm
```

The program prints one line for every record it reads. In its window:

| Do | Look for |
|---|---|
| Nothing yet | `WindowResized` first, with the display's scale factor; then `WindowFocusGained` |
| Type `a`, hold it | `KeyDown KeyA key='a' text="a"`, repeats flagged `Repeat`, one `KeyUp` |
| Shift+`a` | `ModifiersChanged mods=Shift` right after the Shift key's `KeyDown`; `key='A' base='a'` |
| A dead key, then a letter (Option+`e`, `e` on a US Mac layout) | a `Dead(...)` key, then a key whose text is `é` |
| Move, click, drag, right-click | `PointerMotion` with positions in physical pixels; `PointerDown`/`PointerUp` per button with `buttons=` what is held |
| Scroll with a wheel, then with a trackpad | `Wheel Line` for the wheel; `Wheel Pixel` with `phase=` for the trackpad |
| Click another window, come back | synthetic `KeyUp`s for what was held, `WindowFocusLost`, then `WindowFocusGained` |
| `F1`, then `F1` again | `RequestCompleted kind=Fullscreen status=Success`, `WindowFlagsChanged` with `Fullscreen`, a `WindowResized`; and back |
| `F2`, move the mouse, `F3` | the cursor disappears; `WindowCursorGrabChanged grab=Locked`; `PointerMotion` with deltas and a position that stays put; `F3` gives it back |
| `F2`, then click another application | `WindowCursorGrabChanged grab=None reason=FocusLost`; coming back locks it again |
| `F4`, then type with an input method (Japanese, or Option+`e` `e`) | `TextInputState state=Active`; `TextComposition` while composing; `TextCommit` with the result; keys typed directly commit with `flags=FromKey` |
| `F5`, then paste in a text editor | `RequestCompleted kind=ClipboardWrite status=Success`; the editor gets `copied by the guest` |
| Copy text elsewhere, come back, press the paste shortcut | `ClipboardChanged` on return; `pasted "..."` with what was copied |
| The paste shortcut without having copied anything new | the guest's own text is readable at once |
| `F7`, `F8` | `WindowResized` to 640x480; then `WindowFlagsChanged` with `Maximized` and a resize |
| Drag the window to a display with another scale factor | `WindowResized` with `changed=...ScaleFactor`, `WindowDisplayChanged` |
| Cover the window completely, or minimise it | `WindowFlagsChanged` with `Occluded` (or `Minimized`); uncovering it clears the flag and asks for a redraw |
| The close button | `WindowCloseRequested`; the program ends |

`F9` quits. Run it once more with `--raw` and check that `RawMotion` records
arrive only while the window has focus.

## 2. Drawing, with input

```bash
./target/debug/gui_wasmer --window --webgpu examples/out/triangle.wasm
```

| Do | Look for |
|---|---|
| Nothing | a triangle turning smoothly, at the display's refresh rate |
| Drag; arrow keys; the wheel | it turns with the mouse, and with left and right; up, down and the wheel scale it |
| Resize the window, also across displays | the triangle keeps its shape; no stretched or stale frames |
| `F` | fullscreen and back, without a wrong-sized frame |
| `L`, move the mouse, `Escape` | the cursor is gone and the mouse turns the triangle, which glows; `Escape` gives the cursor back, a second one quits |
| Cover the window, wait, uncover it | CPU use near zero while covered; it goes on turning when seen again |
| Lock the screen, unlock it | the same |
| The close button | the program prints `presented N frames at WxH` and ends |

## 3. Drawing without a GPU

```bash
./target/debug/gui_wasmer --window tests/programs/out/pixels.wasm
```

The window closes by itself after a moment: run it and watch. It shows red
growing to the right and green growing downward over a blue tint, first at
320 by 200 and then at 400 by 250, with a white square in the top-left corner
at the end. No stripes (the padding between its rows must not show), nothing
transparent, and nothing upside down.

## 4. A guest that does not answer

```bash
./target/debug/gui_wasmer --window tests/programs/out/killed.wasm
```

This one kills itself after a moment, while waiting for input: the command
returns promptly, with a status that is not 0.

## 5. SDL and Dear ImGui

With SDL built (`make sdl-programs imgui-demo`):

```bash
./target/debug/gui_wasmer --window integrations/sdl3/tests/out/sdl_events.wasm
./target/debug/gui_wasmer --window integrations/sdl3/imgui/out/imgui_demo.wasm
```

The first prints every SDL event; its function keys are listed at the top of
`integrations/sdl3/tests/sdl_events.c`. Go through the rows of section 1
again, in SDL's words: `KEY_DOWN` and `KEY_UP` with SDL's names for keys and
modifiers, `MOUSE_MOTION` in window coordinates (points, on a display of
scale factor 2), `TEXT_EDITING` while an input method composes and
`TEXT_INPUT` for what it commits, relative motion after `F2`, and the paste
shortcut printing what was copied elsewhere.

The second is Dear ImGui's own example:

| Do | Look for |
|---|---|
| Nothing | two windows of the demo, drawn smoothly; the frame rate it shows is the display's |
| Drag a window by its title bar; resize one by its corner | it follows the mouse; the cursor changes shape over the corner |
| Click in a text field (Widgets, Text Input) and type, also with an input method | the text appears; the input method's window is next to the caret |
| Copy from one field and paste into another, and paste something copied in another program | both arrive |
| Scroll the demo window with the wheel and with a trackpad | it scrolls in the direction the system is set to |
| Resize the system's window; move it to a display with another scale factor | the interface keeps its size in points and stays sharp |
| Cover the window, wait, uncover it | CPU use drops while covered; it draws again when seen |
| The close button | the program ends |

## 6. The same through the Wasmer CLI

With the patches of `integrations/` applied to a Wasmer checkout:

```bash
wasmer run --experimental-gui tests/programs/out/events.wasm
wasmer run --experimental-gui --experimental-webgpu examples/out/triangle.wasm
wasmer run --experimental-gui integrations/sdl3/imgui/out/imgui_demo.wasm
```

Check the first rows of sections 1, 2 and 5 again, and that closing the
window ends the command.
