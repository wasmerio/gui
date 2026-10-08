//! winit's events, as what a [`WindowSink`](crate::system::WindowSink) is told.

use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, TouchPhase};
use winit::keyboard::{
    Key, KeyLocation as WinitLocation, ModifiersKeyState, PhysicalKey as Physical,
};

use super::keys;
use crate::abi::enums::{KEY_DEAD, KEY_SEQUENCE};
use crate::system::*;

/// A `WGUIKey` for a winit key, and the text of it a key record carries.
fn logical_key(logical: &Key) -> (u32, String) {
    match logical {
        Key::Named(named) => match keys::named_key(*named) {
            0x20 => (0x20, " ".to_owned()),
            value => (value, String::new()),
        },
        Key::Character(text) => {
            let mut scalars = text.chars();
            match (scalars.next(), scalars.next()) {
                (Some(only), None) => (u32::from(only), text.to_string()),
                (Some(first), Some(_)) => (KEY_SEQUENCE | u32::from(first), text.to_string()),
                (None, _) => (key::UNIDENTIFIED, String::new()),
            }
        }
        Key::Dead(adds) => (KEY_DEAD | adds.map_or(0, u32::from), String::new()),
        Key::Unidentified(_) => (key::UNIDENTIFIED, String::new()),
    }
}

/// The modifier a physical key is, as (in effect, held on this side, held on
/// the other side).
fn modifier_of(physical: PhysicalKey) -> Option<(u32, u32, u32)> {
    use modifiers::*;
    Some(match physical {
        PhysicalKey::ShiftLeft => (SHIFT, LEFT_SHIFT, RIGHT_SHIFT),
        PhysicalKey::ShiftRight => (SHIFT, RIGHT_SHIFT, LEFT_SHIFT),
        PhysicalKey::ControlLeft => (CONTROL, LEFT_CONTROL, RIGHT_CONTROL),
        PhysicalKey::ControlRight => (CONTROL, RIGHT_CONTROL, LEFT_CONTROL),
        PhysicalKey::AltLeft => (ALT, LEFT_ALT, RIGHT_ALT),
        PhysicalKey::AltRight => (ALT, RIGHT_ALT, LEFT_ALT),
        PhysicalKey::MetaLeft => (SUPER, LEFT_SUPER, RIGHT_SUPER),
        PhysicalKey::MetaRight => (SUPER, RIGHT_SUPER, LEFT_SUPER),
        _ => return None,
    })
}

/// A key event. `current` are the modifiers before it: when the key is a
/// modifier itself, the record should carry them as they are after, whether
/// winit announces the change before the key or after it.
pub(super) fn key_input(event: &KeyEvent, current: u32) -> KeyInput {
    let pressed = event.state == ElementState::Pressed;
    let (physical_key, native_scancode) = match event.physical_key {
        Physical::Code(code) => (keys::physical_key(code), scancode(event.physical_key)),
        Physical::Unidentified(_) => (PhysicalKey::Unidentified, scancode(event.physical_key)),
    };
    let (key, key_string) = logical_key(&event.logical_key);
    let (key_unmodified, text_with_all_modifiers) = supplement(event);
    let modifiers = modifier_of(physical_key).map(|(effect, side, other_side)| {
        if pressed {
            current | effect | side
        } else if current & other_side != 0 {
            current & !side
        } else {
            current & !(effect | side)
        }
    });
    KeyInput {
        physical_key,
        native_scancode,
        key,
        key_unmodified,
        key_string,
        location: match event.location {
            WinitLocation::Standard => KeyLocation::Standard,
            WinitLocation::Left => KeyLocation::Left,
            WinitLocation::Right => KeyLocation::Right,
            WinitLocation::Numpad => KeyLocation::Numpad,
        },
        pressed,
        composing: false,
        text: match (&event.text, pressed) {
            (Some(text), true) => text.to_string(),
            _ => String::new(),
        },
        text_with_all_modifiers: if pressed {
            text_with_all_modifiers
        } else {
            String::new()
        },
        modifiers,
        device: 0,
    }
}

#[cfg(any(
    target_os = "windows",
    target_os = "macos",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd"
))]
fn scancode(physical: Physical) -> u32 {
    use winit::platform::scancode::PhysicalKeyExtScancode;
    physical.to_scancode().unwrap_or(0)
}

#[cfg(not(any(
    target_os = "windows",
    target_os = "macos",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
fn scancode(_physical: Physical) -> u32 {
    0
}

/// The key without modifiers, and the text with all of them, where winit
/// knows these.
#[cfg(any(
    target_os = "windows",
    target_os = "macos",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd"
))]
fn supplement(event: &KeyEvent) -> (u32, String) {
    use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
    (
        logical_key(&event.key_without_modifiers()).0,
        event
            .text_with_all_modifiers()
            .map(str::to_owned)
            .unwrap_or_default(),
    )
}

#[cfg(not(any(
    target_os = "windows",
    target_os = "macos",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
fn supplement(_event: &KeyEvent) -> (u32, String) {
    (0, String::new())
}

/// winit's modifiers. It knows of no locks, and of the side a modifier is
/// held on only where the platform tells it.
pub(super) fn modifier_bits(state: &Modifiers) -> u32 {
    use modifiers::*;
    let held = |side: ModifiersKeyState, bit: u32| {
        if side == ModifiersKeyState::Pressed {
            bit
        } else {
            0
        }
    };
    let effect = state.state();
    let mut bits = 0;
    if effect.shift_key() {
        bits |= SHIFT;
    }
    if effect.control_key() {
        bits |= CONTROL;
    }
    if effect.alt_key() {
        bits |= ALT;
    }
    if effect.super_key() {
        bits |= SUPER;
    }
    bits | held(state.lshift_state(), LEFT_SHIFT)
        | held(state.rshift_state(), RIGHT_SHIFT)
        | held(state.lcontrol_state(), LEFT_CONTROL)
        | held(state.rcontrol_state(), RIGHT_CONTROL)
        | held(state.lalt_state(), LEFT_ALT)
        | held(state.ralt_state(), RIGHT_ALT)
        | held(state.lsuper_state(), LEFT_SUPER)
        | held(state.rsuper_state(), RIGHT_SUPER)
}

/// A `WGUIPointerButton`.
pub(super) fn button(button: MouseButton) -> u32 {
    match button {
        MouseButton::Left => PointerButton::Primary as u32,
        MouseButton::Right => PointerButton::Secondary as u32,
        MouseButton::Middle => PointerButton::Auxiliary as u32,
        MouseButton::Back => PointerButton::Back as u32,
        MouseButton::Forward => PointerButton::Forward as u32,
        // Further buttons follow the named ones.
        MouseButton::Other(extra) => (PointerButton::Eraser as u32 + 1 + u32::from(extra)).min(32),
    }
}

/// A wheel turn or a scroll. winit's deltas say which way the content moves;
/// this API's, like the DOM's, which way the view does.
pub(super) fn wheel(delta: MouseScrollDelta, phase: TouchPhase, at: (f64, f64)) -> WheelInput {
    let (unit, delta_x, delta_y) = match delta {
        MouseScrollDelta::LineDelta(x, y) => (WheelUnit::Line, -f64::from(x), -f64::from(y)),
        MouseScrollDelta::PixelDelta(by) => (WheelUnit::Pixel, -by.x, -by.y),
    };
    let phase = match (phase, unit) {
        (TouchPhase::Started, _) => WheelPhase::Began,
        (TouchPhase::Ended, _) => WheelPhase::Ended,
        (TouchPhase::Cancelled, _) => WheelPhase::Cancelled,
        // winit says "moved" for a plain wheel's notches too.
        (TouchPhase::Moved, WheelUnit::Line) => WheelPhase::None,
        (TouchPhase::Moved, WheelUnit::Pixel) => WheelPhase::Changed,
    };
    WheelInput {
        unit,
        phase,
        flags: 0,
        delta_x,
        delta_y,
        x: at.0,
        y: at.1,
        device: 0,
    }
}

/// What a finger did.
pub(super) fn touch_phase(phase: TouchPhase) -> PointerPhase {
    match phase {
        TouchPhase::Started => PointerPhase::Down,
        TouchPhase::Moved => PointerPhase::Motion,
        TouchPhase::Ended => PointerPhase::Up,
        TouchPhase::Cancelled => PointerPhase::Cancel,
    }
}

/// The system's cursor of a shape. Both lists are CSS's.
pub(super) fn cursor_icon(shape: CursorShape) -> winit::window::CursorIcon {
    use winit::window::CursorIcon::*;
    match shape {
        CursorShape::Default => Default,
        CursorShape::ContextMenu => ContextMenu,
        CursorShape::Help => Help,
        CursorShape::Pointer => Pointer,
        CursorShape::Progress => Progress,
        CursorShape::Wait => Wait,
        CursorShape::Cell => Cell,
        CursorShape::Crosshair => Crosshair,
        CursorShape::Text => Text,
        CursorShape::VerticalText => VerticalText,
        CursorShape::Alias => Alias,
        CursorShape::Copy => Copy,
        CursorShape::Move => Move,
        CursorShape::NoDrop => NoDrop,
        CursorShape::NotAllowed => NotAllowed,
        CursorShape::Grab => Grab,
        CursorShape::Grabbing => Grabbing,
        CursorShape::EResize => EResize,
        CursorShape::NResize => NResize,
        CursorShape::NeResize => NeResize,
        CursorShape::NwResize => NwResize,
        CursorShape::SResize => SResize,
        CursorShape::SeResize => SeResize,
        CursorShape::SwResize => SwResize,
        CursorShape::WResize => WResize,
        CursorShape::EwResize => EwResize,
        CursorShape::NsResize => NsResize,
        CursorShape::NeswResize => NeswResize,
        CursorShape::NwseResize => NwseResize,
        CursorShape::ColResize => ColResize,
        CursorShape::RowResize => RowResize,
        CursorShape::AllScroll => AllScroll,
        CursorShape::ZoomIn => ZoomIn,
        CursorShape::ZoomOut => ZoomOut,
    }
}

pub(super) fn theme(theme: Option<winit::window::Theme>) -> Theme {
    match theme {
        Some(winit::window::Theme::Light) => Theme::Light,
        Some(winit::window::Theme::Dark) => Theme::Dark,
        None => Theme::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::{NamedKey, SmolStr};

    #[test]
    fn keys_are_characters_names_dead_or_sequences() {
        assert_eq!(
            logical_key(&Key::Character(SmolStr::new("é"))),
            (0xE9, "é".to_owned())
        );
        assert_eq!(
            logical_key(&Key::Named(NamedKey::Enter)),
            (key::ENTER, String::new())
        );
        // winit's one named key that is a character.
        assert_eq!(
            logical_key(&Key::Named(NamedKey::Space)),
            (0x20, " ".to_owned())
        );
        assert_eq!(
            logical_key(&Key::Dead(Some('^'))),
            (KEY_DEAD | 0x5E, String::new())
        );
        assert_eq!(logical_key(&Key::Dead(None)), (KEY_DEAD, String::new()));
        let (sequence, text) = logical_key(&Key::Character(SmolStr::new("e\u{301}")));
        assert_eq!(sequence, KEY_SEQUENCE | 0x65);
        assert_eq!(text, "e\u{301}");
        // winit's name for the Windows and Command keys is the W3C's Meta.
        assert_eq!(logical_key(&Key::Named(NamedKey::Super)).0, key::META);
    }

    #[test]
    fn wheels_scroll_the_way_the_view_moves() {
        let notch = wheel(
            MouseScrollDelta::LineDelta(0.0, 1.0),
            TouchPhase::Moved,
            (3.0, 4.0),
        );
        assert_eq!(
            (notch.unit, notch.phase, notch.delta_y, notch.x),
            (WheelUnit::Line, WheelPhase::None, -1.0, 3.0)
        );
        let swipe = wheel(
            MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(2.0, -8.0)),
            TouchPhase::Moved,
            (0.0, 0.0),
        );
        assert_eq!(
            (swipe.unit, swipe.phase, swipe.delta_x, swipe.delta_y),
            (WheelUnit::Pixel, WheelPhase::Changed, -2.0, 8.0)
        );
    }

    #[test]
    fn buttons_are_numbered_from_the_primary() {
        assert_eq!(button(MouseButton::Left), 1);
        assert_eq!(button(MouseButton::Right), 2);
        assert_eq!(button(MouseButton::Middle), 3);
        assert_eq!(button(MouseButton::Forward), 5);
        assert_eq!(button(MouseButton::Other(0)), 7);
        assert_eq!(button(MouseButton::Other(u16::MAX)), 32);
    }
}
