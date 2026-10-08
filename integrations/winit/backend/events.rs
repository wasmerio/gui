//! The host's event records, as winit's events.

use std::sync::{Arc, Mutex};

use smol_str::SmolStr;
use wasmer_gui_sys::*;

use crate::dpi::{PhysicalPosition, PhysicalSize};
use crate::event::{
    DeviceEvent, DeviceId as RootDeviceId, ElementState, Event, Force, Ime, InnerSizeWriter,
    KeyEvent, Modifiers, MouseButton, MouseScrollDelta, RawKeyEvent, Touch, TouchPhase,
    WindowEvent,
};
use crate::keyboard::{
    Key, KeyLocation, ModifiersKeys, ModifiersState, NamedKey, NativeKey, NativeKeyCode,
    PhysicalKey,
};
use crate::window::{Theme, WindowId as RootWindowId};

use super::event_loop::Shared;
use super::{has, keys, DeviceId, KeyEventExtra, WindowId};

pub(super) fn theme(theme: WGUITheme) -> Option<Theme> {
    match theme {
        WGUITheme_Light => Some(Theme::Light),
        WGUITheme_Dark => Some(Theme::Dark),
        _ => None,
    }
}

/// A string of a record. Records are only read once, so this is its own.
fn string(record: *const WGUIEventHeader, string: WGUIEventString) -> String {
    let view = unsafe { wguiEventGetString(record, string) };
    if view.data.is_null() || view.length == 0 {
        return String::new();
    }
    let bytes = unsafe { std::slice::from_raw_parts(view.data, view.length) };
    String::from_utf8_lossy(bytes).into_owned()
}

fn physical_key(key: WGUIPhysicalKey) -> PhysicalKey {
    match keys::key_code(key) {
        Some(code) => PhysicalKey::Code(code),
        None => PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
    }
}

/// What a key means, as winit says it. `text` is what the record has for a
/// key that stands for more than one character.
fn logical_key(key: WGUIKey, text: &str) -> Key {
    let value = WGUI_KEY_VALUE(key);
    match WGUI_KEY_TAG(key) {
        // The space bar types a character, and is a named key to winit.
        WGUI_KEY_CHARACTER if value == 0x20 => Key::Named(NamedKey::Space),
        WGUI_KEY_CHARACTER => match char::from_u32(value) {
            Some(character) if value != 0 => {
                Key::Character(SmolStr::new_inline(character.encode_utf8(&mut [0; 4])))
            },
            _ => Key::Unidentified(NativeKey::Unidentified),
        },
        WGUI_KEY_NAMED => match keys::named_key(key) {
            Some(named) => Key::Named(named),
            None => Key::Unidentified(NativeKey::Unidentified),
        },
        WGUI_KEY_DEAD => Key::Dead(char::from_u32(value).filter(|adds| *adds != '\0')),
        WGUI_KEY_SEQUENCE if !text.is_empty() => Key::Character(SmolStr::new(text)),
        WGUI_KEY_SEQUENCE => match char::from_u32(value) {
            Some(first) => Key::Character(SmolStr::new_inline(first.encode_utf8(&mut [0; 4]))),
            None => Key::Unidentified(NativeKey::Unidentified),
        },
        _ => Key::Unidentified(NativeKey::Unidentified),
    }
}

fn key_event(record: *const WGUIEventHeader, pressed: bool) -> KeyEvent {
    let key = unsafe { &*record.cast::<WGUIKeyEvent>() };
    let key_string = string(record, key.keyString);
    let text = string(record, key.text);
    let with_all_modifiers = string(record, key.textWithAllModifiers);
    // The key without modifiers, where the host can tell; the key otherwise.
    let unmodified = if key.keyUnmodified != 0 { key.keyUnmodified } else { key.key };
    KeyEvent {
        physical_key: physical_key(key.physicalKey),
        logical_key: logical_key(key.key, &key_string),
        text: (pressed && !text.is_empty()).then(|| SmolStr::new(&text)),
        location: match key.location {
            WGUIKeyLocation_Left => KeyLocation::Left,
            WGUIKeyLocation_Right => KeyLocation::Right,
            WGUIKeyLocation_Numpad => KeyLocation::Numpad,
            _ => KeyLocation::Standard,
        },
        state: if pressed { ElementState::Pressed } else { ElementState::Released },
        repeat: key.keyFlags & WGUIKeyFlags_Repeat != 0,
        platform_specific: KeyEventExtra {
            key_without_modifiers: logical_key(unmodified, &key_string),
            text_with_all_modifiers: if !pressed {
                None
            } else if !with_all_modifiers.is_empty() {
                Some(SmolStr::new(&with_all_modifiers))
            } else if !text.is_empty() {
                Some(SmolStr::new(&text))
            } else {
                None
            },
        },
    }
}

fn modifiers(bits: WGUIModifiers) -> Modifiers {
    let mut state = ModifiersState::empty();
    state.set(ModifiersState::SHIFT, bits & WGUIModifiers_Shift != 0);
    state.set(ModifiersState::CONTROL, bits & WGUIModifiers_Control != 0);
    state.set(ModifiersState::ALT, bits & WGUIModifiers_Alt != 0);
    state.set(ModifiersState::SUPER, bits & WGUIModifiers_Super != 0);
    let mut pressed = ModifiersKeys::empty();
    pressed.set(ModifiersKeys::LSHIFT, bits & WGUIModifiers_LeftShift != 0);
    pressed.set(ModifiersKeys::RSHIFT, bits & WGUIModifiers_RightShift != 0);
    pressed.set(ModifiersKeys::LCONTROL, bits & WGUIModifiers_LeftControl != 0);
    pressed.set(ModifiersKeys::RCONTROL, bits & WGUIModifiers_RightControl != 0);
    pressed.set(ModifiersKeys::LALT, bits & WGUIModifiers_LeftAlt != 0);
    pressed.set(ModifiersKeys::RALT, bits & WGUIModifiers_RightAlt != 0);
    pressed.set(ModifiersKeys::LSUPER, bits & WGUIModifiers_LeftSuper != 0);
    pressed.set(ModifiersKeys::RSUPER, bits & WGUIModifiers_RightSuper != 0);
    Modifiers { state, pressed_mods: pressed }
}

fn mouse_button(button: WGUIPointerButton) -> MouseButton {
    match button {
        WGUIPointerButton_Primary => MouseButton::Left,
        WGUIPointerButton_Secondary => MouseButton::Right,
        WGUIPointerButton_Auxiliary => MouseButton::Middle,
        WGUIPointerButton_Back => MouseButton::Back,
        WGUIPointerButton_Forward => MouseButton::Forward,
        other => MouseButton::Other(other as u16),
    }
}

/// A wheel turn or a scroll. The host's deltas, like the DOM's, say which
/// way the view moves; winit's which way the content does.
fn scroll(unit: WGUIWheelUnit, x: f64, y: f64) -> MouseScrollDelta {
    // The other way, and nothing stays nothing: not a zero with a sign.
    let (x, y) = (0.0 - x, 0.0 - y);
    if unit == WGUIWheelUnit_Pixel {
        MouseScrollDelta::PixelDelta(PhysicalPosition::new(x, y))
    } else {
        MouseScrollDelta::LineDelta(x as f32, y as f32)
    }
}

/// Turns one record into the events it stands for, in order. Redraws are
/// only noted in `redraws`: the loop sends them after everything else.
pub(super) fn dispatch<T: 'static>(
    shared: &Arc<Shared>,
    record: *const WGUIEventHeader,
    redraws: &mut Vec<WGUIWindow>,
    handler: &mut dyn FnMut(Event<T>),
) {
    let header = unsafe { &*record };
    let device_id = RootDeviceId(DeviceId(header.device));

    // What is for no window in particular.
    match header.r#type {
        WGUIEventType_AppSuspended => return handler(Event::Suspended),
        WGUIEventType_AppResumed => return handler(Event::Resumed),
        WGUIEventType_AppQuitRequested => {
            // winit has no word for it: every window is asked to close.
            let windows: Vec<WGUIWindow> = shared.windows.lock().unwrap().keys().copied().collect();
            for window in windows {
                handler(Event::WindowEvent {
                    window_id: RootWindowId(WindowId(window)),
                    event: WindowEvent::CloseRequested,
                });
            }
            return;
        },
        WGUIEventType_RawMotion => {
            let motion = unsafe { &*record.cast::<WGUIRawMotionEvent>() };
            return handler(Event::DeviceEvent {
                device_id,
                event: DeviceEvent::MouseMotion { delta: (motion.deltaX, motion.deltaY) },
            });
        },
        WGUIEventType_RawButton => {
            let button = unsafe { &*record.cast::<WGUIRawButtonEvent>() };
            return handler(Event::DeviceEvent {
                device_id,
                event: DeviceEvent::Button {
                    button: button.button,
                    state: if button.pressed != 0 {
                        ElementState::Pressed
                    } else {
                        ElementState::Released
                    },
                },
            });
        },
        WGUIEventType_RawWheel => {
            let wheel = unsafe { &*record.cast::<WGUIRawWheelEvent>() };
            return handler(Event::DeviceEvent {
                device_id,
                event: DeviceEvent::MouseWheel {
                    delta: scroll(wheel.unit, wheel.deltaX, wheel.deltaY),
                },
            });
        },
        WGUIEventType_RawKey => {
            let key = unsafe { &*record.cast::<WGUIRawKeyEvent>() };
            return handler(Event::DeviceEvent {
                device_id,
                event: DeviceEvent::Key(RawKeyEvent {
                    physical_key: physical_key(key.physicalKey),
                    state: if key.pressed != 0 {
                        ElementState::Pressed
                    } else {
                        ElementState::Released
                    },
                }),
            });
        },
        _ => {},
    }

    // The rest is for a window, which may have been dropped since.
    let handle = header.window;
    let Some(window) = shared.windows.lock().unwrap().get(&handle).cloned() else {
        return;
    };
    let window_id = RootWindowId(WindowId(handle));
    let mut send = |event: WindowEvent| handler(Event::WindowEvent { window_id, event });

    match header.r#type {
        WGUIEventType_WindowResized => {
            let resized = unsafe { &*record.cast::<WGUIWindowResizedEvent>() };
            let size = PhysicalSize::new(resized.width, resized.height);
            let scale = if resized.scaleFactor > 0.0 { resized.scaleFactor } else { 1.0 };
            let scale_changed = {
                let mut state = window.state.lock().unwrap();
                let changed = state.scale_factor != scale;
                state.scale_factor = scale;
                changed
            };
            if scale_changed {
                // The program may want another size at the new scale. The
                // host is asked for it, and says what came of that.
                let wanted = Arc::new(Mutex::new(size));
                send(WindowEvent::ScaleFactorChanged {
                    scale_factor: scale,
                    inner_size_writer: InnerSizeWriter::new(Arc::downgrade(&wanted)),
                });
                let wanted = *wanted.lock().unwrap();
                if wanted != size {
                    unsafe { wguiWindowSetSize(handle, wanted.width, wanted.height) };
                }
            }
            send(WindowEvent::Resized(size));
            // A window that is new or has another size has nothing drawn in
            // it yet.
            note_redraw(redraws, handle);
        },
        WGUIEventType_WindowMoved => {
            let moved = unsafe { &*record.cast::<WGUIWindowMovedEvent>() };
            send(WindowEvent::Moved(PhysicalPosition::new(moved.x, moved.y)));
        },
        WGUIEventType_WindowFlagsChanged => {
            let flags = unsafe { &*record.cast::<WGUIWindowFlagsChangedEvent>() };
            if flags.changed & WGUIWindowFlags_Occluded != 0 {
                send(WindowEvent::Occluded(flags.flags & WGUIWindowFlags_Occluded != 0));
            }
        },
        WGUIEventType_WindowFocusGained => send(WindowEvent::Focused(true)),
        WGUIEventType_WindowFocusLost => send(WindowEvent::Focused(false)),
        // A window the host took away is one the program can only close.
        WGUIEventType_WindowCloseRequested | WGUIEventType_WindowLost => {
            send(WindowEvent::CloseRequested)
        },
        WGUIEventType_WindowRedrawRequested => note_redraw(redraws, handle),
        WGUIEventType_WindowThemeChanged => {
            let changed = unsafe { &*record.cast::<WGUIThemeEvent>() };
            if let Some(theme) = theme(changed.theme) {
                send(WindowEvent::ThemeChanged(theme));
            }
        },
        WGUIEventType_WindowCursorGrabChanged => {
            let grab = unsafe { &*record.cast::<WGUIWindowCursorGrabChangedEvent>() };
            window.state.lock().unwrap().locked = grab.grab == WGUICursorGrab_Locked;
        },
        WGUIEventType_KeyDown | WGUIEventType_KeyUp => {
            let pressed = header.r#type == WGUIEventType_KeyDown;
            send(WindowEvent::KeyboardInput {
                device_id,
                event: key_event(record, pressed),
                is_synthetic: header.flags & WGUIEventFlags_Synthetic != 0,
            });
        },
        WGUIEventType_ModifiersChanged => {
            let changed = unsafe { &*record.cast::<WGUIModifiersChangedEvent>() };
            send(WindowEvent::ModifiersChanged(modifiers(changed.modifiers)));
        },
        WGUIEventType_TextInputState => {
            let input = unsafe { &*record.cast::<WGUITextInputStateEvent>() };
            let active = input.stateFlags & WGUITextInputStateFlags_Active != 0;
            let was = std::mem::replace(&mut window.state.lock().unwrap().ime_enabled, active);
            if active != was {
                send(WindowEvent::Ime(if active { Ime::Enabled } else { Ime::Disabled }));
            }
        },
        WGUIEventType_TextComposition => {
            let composition = unsafe { &*record.cast::<WGUITextCompositionEvent>() };
            let text = string(record, composition.text);
            // Offsets are bytes of UTF-8, as winit's are.
            let cursor = (composition.selectionStart != WGUI_NO_TEXT_CURSOR && !text.is_empty())
                .then(|| {
                    let start = (composition.selectionStart as usize).min(text.len());
                    let end = (composition.selectionEnd as usize).clamp(start, text.len());
                    (start, end)
                });
            window.state.lock().unwrap().composing = !text.is_empty();
            send(WindowEvent::Ime(Ime::Preedit(text, cursor)));
        },
        WGUIEventType_TextCommit => {
            let commit = unsafe { &*record.cast::<WGUITextCommitEvent>() };
            // What a key typed directly is on its key event, which is where
            // winit's programs look for it.
            if commit.textFlags & WGUITextFlags_FromKey != 0 {
                return;
            }
            let text = string(record, commit.text);
            // Programs take an empty preedit for the end of a composition.
            if std::mem::take(&mut window.state.lock().unwrap().composing) {
                send(WindowEvent::Ime(Ime::Preedit(String::new(), None)));
            }
            if !text.is_empty() {
                send(WindowEvent::Ime(Ime::Commit(text)));
            }
        },
        WGUIEventType_PointerEnter
        | WGUIEventType_PointerLeave
        | WGUIEventType_PointerMotion
        | WGUIEventType_PointerDown
        | WGUIEventType_PointerUp
        | WGUIEventType_PointerCancel => {
            let pointer = unsafe { &*record.cast::<WGUIPointerEvent>() };
            let position = PhysicalPosition::new(pointer.x, pointer.y);
            if pointer.pointerKind == WGUIPointerKind_Touch {
                let phase = match header.r#type {
                    WGUIEventType_PointerDown => TouchPhase::Started,
                    WGUIEventType_PointerUp => TouchPhase::Ended,
                    WGUIEventType_PointerCancel => TouchPhase::Cancelled,
                    WGUIEventType_PointerMotion
                        if pointer.pointerFlags & WGUIPointerFlags_InContact != 0 =>
                    {
                        TouchPhase::Moved
                    },
                    // A finger that hovers, comes or goes is no touch.
                    _ => return,
                };
                send(WindowEvent::Touch(Touch {
                    device_id,
                    phase,
                    location: position,
                    force: (pointer.pointerFlags & WGUIPointerFlags_HasPressure != 0)
                        .then(|| Force::Normalized(pointer.pressure.into())),
                    id: pointer.pointerId.into(),
                }));
                return;
            }
            // The mouse, and a pen, which is one to winit for now.
            let locked = window.state.lock().unwrap().locked;
            // Where the cursor is, said once: a click where it already was
            // told to be moves nothing.
            let moved = |send: &mut dyn FnMut(WindowEvent)| {
                let at = Some((pointer.x, pointer.y));
                let was = std::mem::replace(&mut window.state.lock().unwrap().cursor, at);
                if was != at {
                    send(WindowEvent::CursorMoved { device_id, position });
                }
            };
            match header.r#type {
                WGUIEventType_PointerEnter => {
                    send(WindowEvent::CursorEntered { device_id });
                    moved(&mut send);
                },
                WGUIEventType_PointerLeave => {
                    window.state.lock().unwrap().cursor = None;
                    send(WindowEvent::CursorLeft { device_id });
                },
                WGUIEventType_PointerMotion if locked => {
                    // A locked cursor does not move. How far the mouse did
                    // is what devices report, where the host passes that
                    // on; where it does not, this is all there is of it.
                    if !has(WGUIFeatureName_RawInput)
                        && (pointer.deltaX != 0.0 || pointer.deltaY != 0.0)
                    {
                        handler(Event::DeviceEvent {
                            device_id,
                            event: DeviceEvent::MouseMotion {
                                delta: (pointer.deltaX, pointer.deltaY),
                            },
                        });
                    }
                },
                WGUIEventType_PointerMotion => moved(&mut send),
                WGUIEventType_PointerDown | WGUIEventType_PointerUp => {
                    if !locked {
                        moved(&mut send);
                    }
                    if pointer.button != WGUIPointerButton_None {
                        send(WindowEvent::MouseInput {
                            device_id,
                            state: if header.r#type == WGUIEventType_PointerDown {
                                ElementState::Pressed
                            } else {
                                ElementState::Released
                            },
                            button: mouse_button(pointer.button),
                        });
                    }
                },
                _ => {},
            }
        },
        WGUIEventType_Wheel => {
            let wheel = unsafe { &*record.cast::<WGUIWheelEvent>() };
            send(WindowEvent::MouseWheel {
                device_id,
                delta: scroll(wheel.unit, wheel.deltaX, wheel.deltaY),
                phase: match wheel.phase {
                    WGUIWheelPhase_Began => TouchPhase::Started,
                    WGUIWheelPhase_Ended => TouchPhase::Ended,
                    WGUIWheelPhase_Cancelled => TouchPhase::Cancelled,
                    _ => TouchPhase::Moved,
                },
            });
        },
        // Records of kinds this backend has no event for, or does not know
        // yet, are stepped over.
        _ => {},
    }
}

fn note_redraw(redraws: &mut Vec<WGUIWindow>, window: WGUIWindow) {
    if !redraws.contains(&window) {
        redraws.push(window);
    }
}
