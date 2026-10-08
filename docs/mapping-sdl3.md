# SDL3 on wasmer-gui

What an SDL3 video driver for WASIX needs, hook by hook, and which call of
`<wasmer/gui.h>` serves it. The hook list is complete: all 98 function
pointers of `struct SDL_VideoDevice` and the 11 of the mouse table, read at
SDL `release-3.4.18` (`src/video/SDL_sysvideo.h`, `src/events/SDL_mouse_c.h`).
The notes behind this table, with the line of every call site and what SDL
does when a hook is missing, are in
[`integrations/sdl3/NOTES.md`](../integrations/sdl3/NOTES.md).

Verdicts: **yes** is served by the API and done by the driver; **later**
needs a call the API has reserved room for; **no** is something the API does
not offer, and the hook stays empty.

## State

The driver exists: [`integrations/sdl3/driver`](../integrations/sdl3/driver),
built into SDL 3.4.18 for WASIX by `integrations/sdl3/build.sh`. Its
[README](../integrations/sdl3/README.md) says how to build and use it, what
it does on each host, and what was checked: SDL's own test suite, a program
that prints every SDL event under scripted input, and Dear ImGui's SDL3
example, unchanged, on the headless host, in native windows and in Chrome.

Two things that run decided the order of work here:

- Without `CreateWindowFramebuffer`, and with no GPU backend SDL knows,
  `SDL_CreateRenderer` fails. SDL has no WebGPU renderer, so an SDL program
  had nothing to draw with until `wguiWindowPresentPixels` existed.
- Without `WaitEventTimeout` and `SendWakeupEvent`, `SDL_WaitEvent` polls
  every millisecond: about 450 wake-ups a second while idle. With them, a
  program that waits for its user uses no CPU to speak of.

## The video device

| # | Hook | Served by | |
|---|---|---|---|
| 1 | `VideoInit` | `wguiCreateInstance`, `wguiInstanceGetInfo`, `wguiInstanceGetDisplays`, `wguiDisplayGetInfo` (it must add a display) | yes |
| 2 | `VideoQuit` | `wguiInstanceDestroy` | yes |
| 3 | `ResetTouch` | nothing to reset: fingers are the host's to end | no |
| 4 | `RefreshDisplays` | the hook stays empty: the driver brings SDL's displays up to date itself on `WGUIEventType_DisplaysChanged` | yes |
| 5 | `GetDisplayBounds` | `wguiDisplayGetInfo` | yes |
| 6 | `GetDisplayUsableBounds` | `wguiDisplayGetInfo` (work area) | yes |
| 7 | `GetDisplayModes` | the current mode, from `wguiDisplayGetInfo` | later (mode lists) |
| 8 | `SetDisplayMode` | | later (exclusive fullscreen) |
| 9 | `CreateSDLWindow` | `wguiInstanceCreateWindow` with `WGUIWindowOptions_LogicalUnits` | yes |
| 10 | `SetWindowTitle` | `wguiWindowSetTitle` | yes |
| 11 | `SetWindowIcon` | | later |
| 12 | `SetWindowPosition` | `wguiWindowSetPosition` | yes, where `WindowPosition` |
| 13 | `SetWindowSize` | `wguiWindowSetSize`; SDL is told by the `WindowResized` that follows | yes |
| 14, 15 | `SetWindowMinimumSize`, `SetWindowMaximumSize` | `wguiWindowSetSizeLimits` | yes |
| 16 | `SetWindowAspectRatio` | | later |
| 17 | `GetWindowBordersSize` | `wguiWindowGetState` (frame) | yes |
| 18 | `GetWindowContentScale` | the hook stays empty: SDL works the scale out from the window's pixel density, as on macOS | yes |
| 19 | `GetWindowSizeInPixels` | the size of the last `WindowResized`, for a window made with `SDL_WINDOW_HIGH_PIXEL_DENSITY`; the window's own size otherwise | yes |
| 20 | `SetWindowOpacity` | | later |
| 21, 22 | `SetWindowParent`, `SetWindowModal` | | later |
| 23, 24 | `ShowWindow`, `HideWindow` | `wguiWindowSetFlags` (`Visible`) | yes, where `WindowStates` |
| 25 | `RaiseWindow` | `wguiWindowFocus` | yes |
| 26, 27, 28 | `MaximizeWindow`, `MinimizeWindow`, `RestoreWindow` | `wguiWindowSetFlags` | yes, where `WindowStates` |
| 29, 30, 31 | `SetWindowBordered`, `SetWindowResizable`, `SetWindowAlwaysOnTop` | `wguiWindowSetFlags` | yes, where `WindowStates` |
| 32 | `SetWindowFullscreen` | `wguiWindowSetFullscreen`; the hook answers `SDL_FULLSCREEN_PENDING`, and `WindowFlagsChanged` sends enter and leave | yes (borderless) |
| 33 | `GetWindowICCProfile` | | no |
| 34 | `GetDisplayForWindow` | `wguiWindowGetState` (display) | yes |
| 35 | `SetWindowMouseRect` | | later |
| 36 | `SetWindowMouseGrab` | `wguiWindowSetCursorGrab(Confined)`; SDL also asks this for relative mode, which is `Locked` | yes, where `CursorConfine` |
| 37 | `SetWindowKeyboardGrab` | | later |
| 38 | `DestroyWindow` | `wguiWindowDestroy` | yes |
| 39, 42, 43 | `CreateWindowFramebuffer`, `UpdateWindowFramebuffer`, `DestroyWindowFramebuffer` | a surface of the driver's, shown with `wguiWindowPresentPixels` | yes, where `PixelPresent` |
| 40, 41 | `SetWindowFramebufferVSync`, `GetWindowFramebufferVSync` | after each frame, `wguiWindowRequestRedraw` and a wait for `WindowRedrawRequested` | yes |
| 44 | `OnWindowEnter` | nothing to do | no |
| 45 | `UpdateWindowShape` | | no |
| 46 | `FlashWindow` | `wguiWindowRequestAttention` | yes, where `AttentionRequest` |
| 47 | `ApplyWindowProgress` | | no |
| 48 | `SetWindowFocusable` | | later |
| 49 | `SetWindowFillDocument` | the embedder decides how big an adopted target is | no |
| 50 | `SyncWindow` | pump until the completions of the requests made so far have arrived | yes |
| 51 | `ReconfigureWindow` | no graphics flags to change | no |
| 52–62 | `GL_*` (11) | | no: WebGPU is the GPU |
| 63–68 | `Vulkan_*` (6) | | no |
| 69–71 | `Metal_*` (3) | | no |
| 72 | `WaitEventTimeout` | `wguiInstanceWaitEvents` | yes |
| 73 | `SendWakeupEvent` | `wguiInstanceWake` (any thread) | yes |
| 74 | `PumpEvents` | `wguiInstancePollEvents` | yes |
| 75 | `SuspendScreenSaver` | | later |
| 76, 77, 78, 83 | `StartTextInput`, `StopTextInput`, `UpdateTextInputArea`, `SetTextInputProperties` | `wguiWindowSetTextInput`; the properties are read when text input starts, and the fourth hook stays empty | yes |
| 79 | `ClearComposition` | `wguiWindowResetTextInput` | yes |
| 80 | `HasScreenKeyboardSupport` | `wguiHasFeature(WGUIFeatureName_ScreenKeyboard)` | yes |
| 81, 82 | `ShowScreenKeyboard`, `HideScreenKeyboard` | the descriptor's `screenKeyboard` | yes |
| 84 | `GetTextMimeTypes` | SDL's default list | yes |
| 85, 86, 87 | `SetClipboardData`, `GetClipboardData`, `HasClipboardData` | empty: SDL then goes through the three text hooks below | other types than text: later |
| 88, 89, 90 | `SetClipboardText`, `GetClipboardText`, `HasClipboardText` | the same three calls | yes |
| 91, 92, 93 | `SetPrimarySelectionText`, `GetPrimarySelectionText`, `HasPrimarySelectionText` | the same calls with `WGUIClipboardSelection_Primary` | later |
| 94 | `ShowMessageBox` | | later |
| 95 | `SetWindowHitTest` | | later |
| 96 | `AcceptDragAndDrop` | | later |
| 97 | `ShowWindowSystemMenu` | | later |
| 98 | `free` | the driver's own memory | yes |

## The mouse

| Hook | Served by | |
|---|---|---|
| `CreateCursor` | `wguiInstanceCreateCursor` | yes, where `CustomCursors` |
| `CreateAnimatedCursor` | SDL animates frame by frame through `CreateCursor` | yes |
| `CreateSystemCursor` | a driver object holding a `WGUICursorShape` | yes |
| `ShowCursor` | `wguiWindowSetCursorShape`, `wguiWindowSetCustomCursor`, `wguiWindowSetCursorVisible` | yes |
| `MoveCursor` | for drivers that draw the cursor themselves | no |
| `FreeCursor` | `wguiCursorDestroy` | yes |
| `WarpMouse` | `wguiWindowSetCursorPosition` | yes, where `CursorWarp` |
| `WarpMouseGlobal` | there is no global coordinate space | no |
| `SetRelativeMouseMode` | `wguiWindowSetCursorGrab(Locked)`; motion is the pointer records' deltas | yes, where `CursorLock` |
| `CaptureMouse` | a pointer with a button down is captured as it is | implicit |
| `GetGlobalMouseState` | SDL falls back to the position in the window | no |

## Events the driver sends

| SDL call | From |
|---|---|
| `SDL_SendWindowEvent` (`RESIZED`, `PIXEL_SIZE_CHANGED`, `DISPLAY_SCALE_CHANGED`) | `WindowResized` |
| `SDL_SendWindowEvent` (`MOVED`) | `WindowMoved` |
| `SDL_SendWindowEvent` (`SHOWN`, `HIDDEN`, `MINIMIZED`, `MAXIMIZED`, `RESTORED`, `ENTER_FULLSCREEN`, `LEAVE_FULLSCREEN`, `OCCLUDED`, `EXPOSED`) | `WindowFlagsChanged`, `WindowRedrawRequested` |
| `SDL_SetKeyboardFocus` | `WindowFocusGained`, `WindowFocusLost` |
| `SDL_SetMouseFocus` | the mouse's `PointerEnter`, `PointerLeave` |
| `SDL_SendWindowEvent` (`CLOSE_REQUESTED`) | `WindowCloseRequested`, `WindowLost` |
| `SDL_SendKeyboardKeyAndKeycode` | `KeyDown`, `KeyUp`: the scancode is the low 16 bits of `physicalKey` when its page is 7; the keycode comes from `keyUnmodified` |
| `SDL_SendKeyboardText` | `TextCommit`, every one of them |
| `SDL_SendEditingText` | `TextComposition` (byte offsets become characters) |
| `SDL_SendMouseMotion` | the mouse's `PointerMotion`; relative while the cursor is locked |
| `SDL_SendMouseButton` | the mouse's `PointerDown`, `PointerUp`, and `PointerCancel` as a release of every button |
| `SDL_SendMouseWheel` | `Wheel` (SDL's vertical sign is the opposite) |
| `SDL_SendTouch`, `SDL_SendTouchMotion` | pointer records of kind `Touch` |
| `SDL_SendClipboardUpdate` | `ClipboardChanged` |
| `SDL_AddVideoDisplay`, `SDL_DelVideoDisplay`, `SDL_SetDesktopDisplayMode` | `DisplaysChanged` |
| `SDL_SendWindowEvent` (`DISPLAY_CHANGED`) | `WindowDisplayChanged` |
| `SDL_ToggleModState` (Caps, Num and Scroll Lock) | `ModifiersChanged` |
| `SDL_SendScreenKeyboardShown`, `SDL_SendScreenKeyboardHidden` | `TextInputState` |
| `SDL_SetWindowSafeAreaInsets` | the safe insets of `wguiWindowGetState` |
| `SDL_SendAppEvent`, `SDL_SendQuit` | the application records |

SDL scancodes are USB HID keyboard-page usages, which is what a
`WGUIPhysicalKey` of page 7 is: no table is needed for those. The handful of
SDL scancodes past 255 (media and application keys) map from the consumer
page by a short table.

## Text input

SDL wants every insertion as `SDL_EVENT_TEXT_INPUT` while text input is
started, and key events besides. That is the API's rule: while a window has
text input on, every insertion is a `TextCommit`, typed keys included, so the
driver forwards all commits and ignores the text on key records. A key the
input method took (`WGUIKeyFlags_Composing`) is not a key press to SDL.

Programs take an empty `SDL_EVENT_TEXT_EDITING` for the end of a composition.
Not every host sends an empty `TextComposition` before the commit, so the
driver sends the empty one itself when a commit ends what was being composed.

## Clipboard

`SDL_GetClipboardText` is synchronous. `wguiInstanceReadClipboard` answers at
once on hosts with `ClipboardSyncRead`, and always for what the program wrote
itself. Where it answers `Pending` (a browser), the driver makes the request,
pumps for up to half a second, and returns the text when the completion
arrives, or the empty string. `SDL_HasClipboardText` is true where the host
says `WGUIClipboardFlags_NeedsRequest`: there may be text.

## Frames

`SDL_RenderPresent` with vsync has to wait for the display, and the API's
`wguiWindowPresentPixels` does not wait. The driver asks for a redraw after
the frame and pumps until `WindowRedrawRequested` comes, which every host
sends once per refresh of the display; what else arrives meanwhile goes to
SDL's queue. That record is not passed on as `SDL_EVENT_WINDOW_EXPOSED`: a
program that draws when exposed would otherwise draw forever.

SDL's software renderer simulates vsync by sleeping. A 12-line change to
it (`sdl3-wasmer.patch`) lets a video driver that can pace its framebuffer do
so instead.

## Outside video

Found by the build spike, to be dealt with by the port and not by this API:
`SDL_GetBasePath()` returns NULL, `SDL_GetPrefPath()` needs `HOME`, and audio
is the dummy driver until there is an audio API.
