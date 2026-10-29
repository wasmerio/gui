//! What a window system reports, turned into the records guests read.
//!
//! A window system holds one [`WindowSink`] per window and tells it what
//! happens, as it happens, in the plainest terms it has. The sink keeps the
//! promises the API makes about the result, so that they hold the same way
//! on every window system:
//!
//! - a key's presses and releases pair up, a held key repeats as flagged
//!   presses, and a release nobody saw the press of is not passed on;
//! - a window about to lose focus first sees its keys released and its
//!   touches cancelled;
//! - a change of modifiers is announced right after the key that made it;
//! - every button of a pointer goes down and up on its own;
//! - motion, wheel deltas and resizes a guest has not collected yet are
//!   merged instead of piling up, with nothing of the movement lost;
//! - text too long for one record is split, never cut.
//!
//! A sink belongs to the one thread that reports, and nothing in it waits.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::shared::{InstanceShared, MOTION_SLOTS, Queued, WindowShared, update};
use crate::abi::Handle;
use crate::abi::enums::*;
use crate::abi::events::*;
use crate::abi::structs::Rect;
use crate::system::ClipboardState;

/// The longest text one record carries. Longer commits are split.
const MAX_TEXT_BYTES: usize = MAX_EVENT_SIZE as usize - 128;
/// The longest of the short strings a key record carries.
const MAX_KEY_TEXT_BYTES: usize = 256;

const LOCK_MODIFIERS: u32 = modifiers::CAPS_LOCK | modifiers::NUM_LOCK | modifiers::SCROLL_LOCK;

/// The longest prefix of `text` that is at most `max` bytes and ends between
/// characters.
fn prefix(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// A key going down or up.
#[derive(Clone, Debug, Default)]
pub struct KeyInput {
    pub physical_key: PhysicalKey,
    /// The platform's own code for the key. 0 if unknown.
    pub native_scancode: u32,
    /// What the key means with the modifiers held: a `WGUIKey`.
    pub key: u32,
    /// What it means with none held. 0 if unknown.
    pub key_unmodified: u32,
    /// `key` as text, when it is a character, a dead key or a sequence.
    pub key_string: String,
    pub location: KeyLocation,
    pub pressed: bool,
    /// An input method took the key for what it is composing.
    pub composing: bool,
    /// What the press types through the layout. Empty for a release, and for
    /// keys that type nothing.
    pub text: String,
    /// The same with Control applied too.
    pub text_with_all_modifiers: String,
    /// The [`modifiers`] after this event, when the window system learns them
    /// with the key. Otherwise report them with [`WindowSink::modifiers`].
    pub modifiers: Option<u32>,
    /// Which keyboard, where they can be told apart. 0 otherwise.
    pub device: u32,
}

/// What a pointer did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PointerPhase {
    Enter,
    Leave,
    #[default]
    Motion,
    Down,
    Up,
    Cancel,
}

/// A mouse cursor, a finger or a pen doing something over a window.
#[derive(Clone, Debug)]
pub struct PointerInput {
    pub kind: PointerKind,
    /// The window system's own name for this finger or pen. Ignored for the
    /// mouse, of which there is one.
    pub source: u64,
    pub phase: PointerPhase,
    /// The `WGUIPointerButton` that went down or up. For a touch, primary.
    pub button: u32,
    /// Position in the window, in physical pixels.
    pub x: f64,
    pub y: f64,
    /// The movement, when the window system measures it itself (as it must
    /// while the cursor is locked). Otherwise it is worked out from positions.
    pub delta: Option<(f64, f64)>,
    /// 0 to 1, where the hardware measures it.
    pub pressure: Option<f32>,
    pub tangential_pressure: f32,
    /// Degrees from upright, where the hardware measures it.
    pub tilt: Option<(f32, f32)>,
    pub twist: f32,
    /// The size of the contact in physical pixels. (0, 0) if unknown.
    pub contact_size: (f32, f32),
    pub tool: PointerTool,
    /// For a press or release: 2 for a double click. 0 if unknown.
    pub click_count: u32,
    pub device: u32,
}

impl Default for PointerInput {
    fn default() -> Self {
        Self {
            kind: PointerKind::Mouse,
            source: 0,
            phase: PointerPhase::Motion,
            button: 0,
            x: 0.0,
            y: 0.0,
            delta: None,
            pressure: None,
            tangential_pressure: 0.0,
            tilt: None,
            twist: 0.0,
            contact_size: (0.0, 0.0),
            tool: PointerTool::Unknown,
            click_count: 0,
            device: 0,
        }
    }
}

/// A wheel turning or a touchpad scrolling.
#[derive(Clone, Debug)]
pub struct WheelInput {
    pub unit: WheelUnit,
    pub phase: WheelPhase,
    /// [`wheel_flags`].
    pub flags: u32,
    /// Positive `delta_y` shows what is below, positive `delta_x` what is to
    /// the right.
    pub delta_x: f64,
    pub delta_y: f64,
    /// Where the cursor is.
    pub x: f64,
    pub y: f64,
    pub device: u32,
}

/// Where a window system reports what concerns no window in particular.
#[derive(Clone)]
pub struct InstanceSink {
    instance: Arc<InstanceShared>,
    raw_input_unfocused: bool,
}

impl InstanceSink {
    pub(crate) fn new(instance: &Arc<InstanceShared>, raw_input_unfocused: bool) -> Self {
        Self {
            instance: Arc::clone(instance),
            raw_input_unfocused,
        }
    }

    /// The guest has gone: nothing reported from now on reaches anyone.
    pub fn is_closed(&self) -> bool {
        self.instance.is_closed()
    }

    /// Whether the guest has read everything reported so far.
    ///
    /// For tests and tools that feed a guest input step by step. It looks at
    /// what guest threads lock, so it is not for a thread that may not wait.
    pub fn is_drained(&self) -> bool {
        !self.instance.has_events()
    }

    /// Whether the guest sits in `wguiInstanceWaitEvents` with nothing to
    /// read: it has dealt with everything and is waiting for more. The same
    /// caveat as [`InstanceSink::is_drained`] applies.
    pub fn is_idle(&self) -> bool {
        self.instance.waiting.load(Ordering::Acquire) != 0 && !self.instance.has_events()
    }

    fn emit(&self, device: u32, event: Event) {
        self.instance.push(Queued {
            window: 0,
            device,
            time: self.instance.system.now(),
            flags: 0,
            event,
        });
    }

    pub fn suspended(&self) {
        self.emit(0, Event::AppSuspended);
    }

    pub fn resumed(&self) {
        self.emit(0, Event::AppResumed);
    }

    pub fn quit_requested(&self) {
        self.emit(0, Event::AppQuitRequested);
    }

    pub fn terminating(&self) {
        self.emit(0, Event::AppTerminating);
    }

    pub fn theme_changed(&self, theme: Theme) {
        self.emit(0, Event::AppThemeChanged(ThemeEvent { theme }));
    }

    pub fn displays_changed(&self) {
        self.emit(0, Event::DisplaysChanged);
    }

    pub fn clipboard_changed(&self, selection: ClipboardSelection, state: ClipboardState) {
        self.emit(
            0,
            Event::ClipboardChanged(ClipboardChangedEvent {
                selection,
                clipboard_flags: state.flags,
                clipboard_sequence: state.sequence,
            }),
        );
    }

    /// Device input reaches a guest only while one of its windows has focus,
    /// unless the embedder said otherwise.
    fn raw_allowed(&self) -> bool {
        self.raw_input_unfocused || self.instance.focused_windows.load(Ordering::Acquire) != 0
    }

    /// A mouse moved, as the device reports it.
    pub fn raw_motion(&self, device: u32, delta_x: f64, delta_y: f64) {
        if !self.raw_allowed() {
            return;
        }
        self.instance.put_raw_motion(Queued {
            window: 0,
            device,
            time: self.instance.system.now(),
            flags: 0,
            event: Event::RawMotion(RawMotionEvent { delta_x, delta_y }),
        });
    }

    pub fn raw_button(&self, device: u32, button: u32, pressed: bool) {
        if self.raw_allowed() {
            self.emit(
                device,
                Event::RawButton(RawButtonEvent {
                    button,
                    pressed: u32::from(pressed),
                }),
            );
        }
    }

    pub fn raw_wheel(&self, device: u32, unit: WheelUnit, delta_x: f64, delta_y: f64) {
        if self.raw_allowed() {
            self.emit(
                device,
                Event::RawWheel(RawWheelEvent {
                    unit,
                    delta_x,
                    delta_y,
                }),
            );
        }
    }

    pub fn raw_key(
        &self,
        device: u32,
        physical_key: PhysicalKey,
        native_scancode: u32,
        pressed: bool,
    ) {
        if self.raw_allowed() {
            self.emit(
                device,
                Event::RawKey(RawKeyEvent {
                    physical_key,
                    native_scancode,
                    pressed: u32::from(pressed),
                }),
            );
        }
    }
}

/// A pointer the sink is following.
#[derive(Clone, Debug)]
struct Pointer {
    kind: PointerKind,
    source: u64,
    id: u32,
    /// `WGUIPointerButtons` held.
    buttons: u32,
    x: f64,
    y: f64,
    /// Whether `x` and `y` are a position it has been at.
    placed: bool,
    /// Its motion mailbox, if it has one.
    slot: Option<usize>,
}

/// Where a window system reports what happens to one window.
pub struct WindowSink {
    instance: Arc<InstanceShared>,
    window: Arc<WindowShared>,
    /// The modifier the platform's shortcuts use, for spotting a paste.
    shortcut_modifier: u32,
    /// The resync epoch this sink has caught up with.
    epoch: u32,
    /// The window's size has been reported at least once.
    sized: bool,
    focused: bool,
    modifiers: u32,
    /// The keys the guest was told are down.
    keys: Vec<KeyInput>,
    mouse: Pointer,
    /// Fingers and pens.
    pointers: Vec<Pointer>,
    next_pointer_id: u32,
    /// The finger that came down while no other was: the primary touch.
    primary_touch: Option<u32>,
}

impl WindowSink {
    pub(crate) fn new(
        instance: &Arc<InstanceShared>,
        window: &Arc<WindowShared>,
        shortcut_modifier: u32,
    ) -> Self {
        Self {
            instance: Arc::clone(instance),
            window: Arc::clone(window),
            shortcut_modifier,
            epoch: instance.resync_epoch.load(Ordering::Acquire),
            sized: false,
            focused: false,
            modifiers: 0,
            keys: Vec::new(),
            mouse: Pointer {
                kind: PointerKind::Mouse,
                source: 0,
                id: POINTER_ID_MOUSE,
                buttons: 0,
                x: 0.0,
                y: 0.0,
                placed: false,
                slot: Some(0),
            },
            pointers: Vec::new(),
            next_pointer_id: POINTER_ID_MOUSE + 1,
            primary_touch: None,
        }
    }

    /// The guest's handle for the window, for a window system's own logs.
    pub fn handle(&self) -> Handle {
        self.window.handle()
    }

    /// The guest destroyed the window, or has gone altogether.
    pub fn is_closed(&self) -> bool {
        self.window.closed.load(Ordering::Acquire) || self.instance.is_closed()
    }

    /// A way to go on asking that after the sink has been handed to another
    /// thread.
    pub fn watch(&self) -> SinkWatch {
        SinkWatch {
            instance: Arc::clone(&self.instance),
            window: Arc::clone(&self.window),
        }
    }

    /// How many records the guest has read so far, of all its windows'. A
    /// number that stands still while records wait is a guest that is not
    /// reading.
    pub fn delivered(&self) -> u64 {
        self.instance.delivered.load(Ordering::Acquire)
    }

    // -- plumbing ---------------------------------------------------------------

    /// Catches up with records having been dropped: the guest was told that
    /// everything it held is released, so this sink must believe that too,
    /// or it would pass on releases the guest has no press for.
    fn sync(&mut self) {
        let epoch = self.instance.resync_epoch.load(Ordering::Acquire);
        if epoch != self.epoch {
            self.epoch = epoch;
            self.keys.clear();
            self.mouse.buttons = 0;
            self.pointers.clear();
            self.primary_touch = None;
        }
    }

    fn queued(&self, device: u32, flags: u32, event: Event) -> Queued {
        Queued {
            window: self.window.handle(),
            device,
            time: self.instance.system.now(),
            flags,
            event,
        }
    }

    /// Moves what the window's mailboxes hold into the queue, so that what is
    /// queued next comes after it for the guest too.
    fn flush(&self) {
        for slot in [self.window.resize_slot(), self.window.moved_slot()]
            .into_iter()
            .chain((0..MOTION_SLOTS).map(|index| self.window.motion_slot(index)))
            .chain([self.window.wheel_slot()])
        {
            if let Some(queued) = slot.take() {
                self.instance.push(queued);
            }
        }
    }

    /// Queues a record, in order.
    ///
    /// Nothing is queued before the window's size was first reported: that
    /// report is how the guest learns the window exists, and what was known
    /// before it the guest reads from the window's state.
    fn emit(&self, device: u32, flags: u32, event: Event) {
        if !self.sized || self.window.closed.load(Ordering::Acquire) {
            return;
        }
        self.flush();
        self.instance.push(self.queued(device, flags, event));
    }

    /// Whether a record of this class would reach anyone.
    fn wanted(&self, class: u64) -> bool {
        self.sized && !self.is_closed() && self.instance.wants(class)
    }

    // -- the window ---------------------------------------------------------------

    /// The content area's size in physical pixels, and the scale factor. The
    /// first report is the window's creation, as far as the guest can tell.
    pub fn resized(&mut self, width: u32, height: u32, scale_factor: f64) {
        self.sync();
        let (before, _) = self.window.snapshot.update(|data| {
            data.width = width;
            data.height = height;
            data.scale_factor = scale_factor;
        });
        if !self.sized {
            self.sized = true;
            self.emit(
                0,
                event_flags::SYNTHETIC,
                Event::WindowResized(WindowResizedEvent {
                    width,
                    height,
                    scale_factor,
                    changed: resize_flags::SIZE | resize_flags::SCALE_FACTOR,
                }),
            );
            return;
        }
        let mut changed = 0;
        if (before.width, before.height) != (width, height) {
            changed |= resize_flags::SIZE;
        }
        if before.scale_factor != scale_factor {
            changed |= resize_flags::SCALE_FACTOR;
        }
        if changed == 0 {
            return;
        }
        if self.wanted(event_class::WINDOW) {
            self.queue_resize(width, height, scale_factor, changed);
        }
        // Only now: a guest that takes the resize with a redraw already
        // waiting sees that redraw as an exposure (`WindowShared::exposed`).
        self.window.exposed.store(true, Ordering::Release);
    }

    fn queue_resize(&mut self, width: u32, height: u32, scale_factor: f64, changed: u32) {
        let resized = self.queued(
            0,
            0,
            Event::WindowResized(WindowResizedEvent {
                width,
                height,
                scale_factor,
                changed,
            }),
        );
        self.window.resize_slot().put(resized, |old, new| {
            let (Event::WindowResized(old), Event::WindowResized(latest)) =
                (&old.event, &new.event)
            else {
                return new;
            };
            Queued {
                flags: new.flags | event_flags::COALESCED,
                event: Event::WindowResized(WindowResizedEvent {
                    changed: old.changed | latest.changed,
                    ..latest.clone()
                }),
                ..new
            }
        });
        self.instance.wait.notify();
    }

    /// The content area's position on the desktop.
    pub fn moved(&mut self, x: i32, y: i32) {
        self.sync();
        let (before, _) = self.window.snapshot.update(|data| {
            data.x = x;
            data.y = y;
        });
        if (before.x, before.y) == (x, y) || !self.wanted(event_class::WINDOW) {
            return;
        }
        let moved = self.queued(0, 0, Event::WindowMoved(WindowMovedEvent { x, y }));
        self.window.moved_slot().put(moved, |_, new| Queued {
            flags: new.flags | event_flags::COALESCED,
            ..new
        });
        self.instance.wait.notify();
    }

    /// How far the window's decorations reach beyond the content area:
    /// left, top, right, bottom.
    pub fn frame(&mut self, frame: [u32; 4]) {
        self.window.snapshot.update(|data| data.frame = frame);
    }

    /// How much of the content area's edges may be covered.
    pub fn safe_area(&mut self, safe_area: [u32; 4]) {
        self.window
            .snapshot
            .update(|data| data.safe_area = safe_area);
    }

    /// The [`window_flags`] in `mask` are now as in `values`. Focus has its
    /// own report, [`WindowSink::focused`].
    pub fn flags(&mut self, mask: u64, values: u64) {
        self.sync();
        let mask = mask & !window_flags::FOCUSED;
        let (before, after) = self
            .window
            .snapshot
            .update(|data| data.flags = (data.flags & !mask) | (values & mask));
        // Hover is announced by the mouse entering and leaving.
        let changed = (before.flags ^ after.flags) & !window_flags::HOVERED;
        if changed != 0 {
            self.emit(
                0,
                0,
                Event::WindowFlagsChanged(WindowFlagsChangedEvent {
                    flags: after.flags,
                    changed,
                }),
            );
        }
    }

    /// The window entered or left fullscreen, whoever caused it.
    pub fn fullscreen(&mut self, mode: FullscreenMode) {
        self.window.snapshot.update(|data| data.fullscreen = mode);
        let on = mode != FullscreenMode::None;
        self.flags(
            window_flags::FULLSCREEN,
            if on { window_flags::FULLSCREEN } else { 0 },
        );
    }

    /// The window gained or lost keyboard focus.
    pub fn focused(&mut self, focused: bool) {
        self.sync();
        if focused == self.focused {
            return;
        }
        if focused {
            self.focused = true;
            self.instance.focused_windows.fetch_add(1, Ordering::AcqRel);
            self.window
                .snapshot
                .update(|data| data.flags |= window_flags::FOCUSED);
            self.emit(0, 0, Event::WindowFocusGained);
            if self.modifiers != 0 {
                self.emit(
                    0,
                    0,
                    Event::ModifiersChanged(ModifiersChangedEvent {
                        modifiers: self.modifiers,
                    }),
                );
            }
            return;
        }
        // What the guest holds is released before it hears of the loss:
        // its keys, then the modifiers they made, then its touches.
        let modifiers = self.modifiers & LOCK_MODIFIERS;
        for key in std::mem::take(&mut self.keys) {
            let record = Self::key_record(&key, modifiers, 0, false);
            self.emit(key.device, event_flags::SYNTHETIC, Event::KeyUp(record));
        }
        self.set_modifiers(modifiers);
        for pointer in std::mem::take(&mut self.pointers) {
            if pointer.kind == PointerKind::Touch {
                let record = self.cancel_record(&pointer);
                self.emit(0, event_flags::SYNTHETIC, Event::PointerCancel(record));
            } else {
                self.pointers.push(pointer);
            }
        }
        self.primary_touch = None;
        self.focused = false;
        self.window
            .snapshot
            .update(|data| data.flags &= !window_flags::FOCUSED);
        self.emit(0, 0, Event::WindowFocusLost);
        update(&self.instance.focused_windows, |count| count.checked_sub(1));
    }

    /// The user asked to close the window.
    pub fn close_requested(&mut self) {
        self.emit(0, 0, Event::WindowCloseRequested);
    }

    /// The window has to be shown whole again: an exposure. A redraw whose
    /// cause the window system cannot tell is one too.
    pub fn redraw_requested(&mut self) {
        self.redraw_with_reason(event_flags::EXPOSURE);
    }

    /// Answers the guest's `wguiWindowRequestRedraw`; on its own, what the
    /// window showed is still there. `exposure`: the window also has to be
    /// shown whole again, told in the same redraw. Only a window system that
    /// knows this, and says so with `WGUIFeatureName_RedrawReasons`, calls it.
    pub fn frame_callback_requested(&mut self, exposure: bool) {
        let exposure = if exposure { event_flags::EXPOSURE } else { 0 };
        self.redraw_with_reason(event_flags::FRAME_CALLBACK | exposure);
    }

    fn redraw_with_reason(&mut self, reason: u32) {
        if !self.wanted(event_class::WINDOW) {
            return;
        }
        // A new or resized window's exposure is added as the guest takes the
        // redraw (`WindowShared::exposed`).
        let reason = if self.instance.redraw_reasons {
            reason
        } else {
            0
        };
        let redraw = self.queued(0, reason, Event::WindowRedrawRequested);
        self.window.redraw_slot().put(redraw, |old, mut new| {
            // A merged redraw keeps every reason: an exposure still waiting
            // is not lost to the frame callback that follows it.
            new.flags |= old.flags | event_flags::COALESCED;
            new
        });
        self.instance.wait.notify();
    }

    /// The window is now mostly on the display with this
    /// [`DisplayData::id`](crate::system::DisplayData::id).
    pub fn display_changed(&mut self, display: u64) {
        let (before, _) = self.window.snapshot.update(|data| data.display = display);
        if before.display != display {
            // The guest knows displays by handle; the record gets this
            // window's one when it is delivered.
            self.emit(
                0,
                0,
                Event::WindowDisplayChanged(WindowDisplayChangedEvent { display: 0 }),
            );
        }
    }

    pub fn theme_changed(&mut self, theme: Theme) {
        let (before, _) = self.window.snapshot.update(|data| data.theme = theme);
        if before.theme != theme {
            self.emit(0, 0, Event::WindowThemeChanged(ThemeEvent { theme }));
        }
    }

    /// The cursor grab in effect changed, whoever caused it.
    pub fn cursor_grab_changed(&mut self, grab: CursorGrab, reason: CursorGrabReason) {
        let (before, _) = self.window.snapshot.update(|data| data.cursor_grab = grab);
        if before.cursor_grab != grab {
            self.emit(
                0,
                0,
                Event::WindowCursorGrabChanged(WindowCursorGrabChangedEvent { grab, reason }),
            );
        }
    }

    /// The window's own side is gone: its target was taken away.
    pub fn lost(&mut self) {
        self.emit(0, 0, Event::WindowLost);
    }

    // -- the keyboard ---------------------------------------------------------------

    fn key_record(key: &KeyInput, modifiers: u32, key_flags: u32, with_text: bool) -> KeyEvent {
        let text = |text: &str| {
            if with_text {
                prefix(text, MAX_KEY_TEXT_BYTES).to_owned()
            } else {
                String::new()
            }
        };
        KeyEvent {
            physical_key: key.physical_key,
            native_scancode: key.native_scancode,
            key: key.key,
            key_unmodified: key.key_unmodified,
            modifiers,
            key_flags,
            location: key.location,
            key_string: prefix(&key.key_string, MAX_KEY_TEXT_BYTES).to_owned(),
            text: text(&key.text),
            text_with_all_modifiers: text(&key.text_with_all_modifiers),
        }
    }

    fn set_modifiers(&mut self, modifiers: u32) {
        if modifiers == self.modifiers {
            return;
        }
        self.modifiers = modifiers;
        self.window
            .snapshot
            .update(|data| data.modifiers = modifiers);
        if self.focused {
            self.emit(
                0,
                0,
                Event::ModifiersChanged(ModifiersChangedEvent { modifiers }),
            );
        }
    }

    /// The modifiers in effect changed without a key the window saw (they
    /// were pressed elsewhere, or a lock was toggled).
    pub fn modifiers(&mut self, modifiers: u32) {
        self.sync();
        self.set_modifiers(modifiers);
    }

    /// A key went down or up. Report repeats as further presses.
    pub fn key(&mut self, input: KeyInput) {
        self.sync();
        let modifiers = input.modifiers.unwrap_or(self.modifiers);
        let held = self.keys.iter().position(|key| {
            key.physical_key == input.physical_key && key.native_scancode == input.native_scancode
        });
        let mut key_flags = 0;
        if input.pressed {
            if input.composing {
                key_flags |= key_flags::COMPOSING;
            }
            match held {
                Some(_) => key_flags |= key_flags::REPEAT,
                None => self.keys.push(KeyInput {
                    text: String::new(),
                    text_with_all_modifiers: String::new(),
                    ..input.clone()
                }),
            }
        } else {
            match held {
                Some(index) => {
                    self.keys.swap_remove(index);
                }
                None => {
                    // The press went to another window, or was dropped: the
                    // guest must not see a release without it.
                    self.set_modifiers(modifiers);
                    return;
                }
            }
        }
        let record = Self::key_record(&input, modifiers, key_flags, input.pressed);
        let text = record.text.clone();
        self.emit(
            input.device,
            0,
            if input.pressed {
                Event::KeyDown(record)
            } else {
                Event::KeyUp(record)
            },
        );
        self.set_modifiers(modifiers);
        if !input.pressed {
            return;
        }
        let shortcut = modifiers & self.shortcut_modifier != 0;
        let paste = match input.physical_key {
            PhysicalKey::KeyV => shortcut,
            PhysicalKey::Insert => modifiers & modifiers::SHIFT != 0,
            PhysicalKey::Paste => true,
            _ => false,
        };
        if paste && self.focused {
            self.instance
                .paste_stamp
                .store(self.instance.system.now(), Ordering::Release);
        }
        // While text is being entered, what a plain key types is also a
        // commit, so that a guest can take all its text from commits.
        let types = !text.is_empty()
            && !input.composing
            && modifiers & (modifiers::CONTROL | modifiers::SUPER) == 0
            && !text.chars().any(char::is_control);
        if types && self.window.text_input.load(Ordering::Acquire) {
            self.commit(&text, text_flags::FROM_KEY);
        }
    }

    // -- text ---------------------------------------------------------------------

    /// Whether an input method is attached, and what an on-screen keyboard
    /// covers of the window.
    pub fn text_input_state(&mut self, active: bool, keyboard_visible: bool, keyboard_area: Rect) {
        let mut state_flags = 0;
        if active {
            state_flags |= text_input_state_flags::ACTIVE;
        }
        if keyboard_visible {
            state_flags |= text_input_state_flags::SCREEN_KEYBOARD_VISIBLE;
        }
        self.emit(
            0,
            0,
            Event::TextInputState(TextInputStateEvent {
                state_flags,
                keyboard_area,
            }),
        );
    }

    /// The text an input method is composing; it replaces what was shown.
    /// `selection` is its cursor or selection inside the text, in bytes.
    pub fn text_composition(&mut self, text: &str, selection: Option<(u32, u32)>) {
        let text = prefix(text, MAX_TEXT_BYTES);
        let clamp = |offset: u32| offset.min(text.len() as u32);
        let (selection_start, selection_end) = match selection {
            Some((start, end)) => (clamp(start), clamp(end)),
            None => (NO_TEXT_CURSOR, NO_TEXT_CURSOR),
        };
        self.emit(
            0,
            0,
            Event::TextComposition(TextCompositionEvent {
                selection_start,
                selection_end,
                text_flags: 0,
                text: text.to_owned(),
            }),
        );
    }

    /// Text to insert, from an input method or an on-screen keyboard.
    pub fn text_commit(&mut self, text: &str) {
        self.commit(text, 0);
    }

    fn commit(&mut self, text: &str, flags: u32) {
        let mut rest = text;
        while !rest.is_empty() {
            let chunk = prefix(rest, MAX_TEXT_BYTES);
            rest = &rest[chunk.len()..];
            let continues = if rest.is_empty() {
                0
            } else {
                text_flags::CONTINUES
            };
            self.emit(
                0,
                0,
                Event::TextCommit(TextCommitEvent {
                    text_flags: flags | continues,
                    text: chunk.to_owned(),
                }),
            );
        }
    }

    // -- pointers -------------------------------------------------------------------

    fn pointer_index(&self, input: &PointerInput) -> Option<usize> {
        self.pointers
            .iter()
            .position(|pointer| pointer.kind == input.kind && pointer.source == input.source)
    }

    /// Starts following a finger or a pen.
    fn adopt(&mut self, input: &PointerInput) -> usize {
        let used: Vec<usize> = self.pointers.iter().filter_map(|p| p.slot).collect();
        let slot = (1..MOTION_SLOTS).find(|slot| !used.contains(slot));
        let id = self.next_pointer_id;
        self.next_pointer_id = self
            .next_pointer_id
            .checked_add(1)
            .unwrap_or(POINTER_ID_MOUSE + 1);
        if input.kind == PointerKind::Touch
            && !self.pointers.iter().any(|p| p.kind == PointerKind::Touch)
        {
            self.primary_touch = Some(id);
        }
        self.pointers.push(Pointer {
            kind: input.kind,
            source: input.source,
            id,
            buttons: 0,
            x: input.x,
            y: input.y,
            placed: false,
            slot,
        });
        self.pointers.len() - 1
    }

    fn pointer_flags(&self, pointer: &Pointer) -> u32 {
        let primary = match pointer.kind {
            PointerKind::Touch => self.primary_touch == Some(pointer.id),
            _ => true,
        };
        let mut flags = 0;
        if primary {
            flags |= pointer_flags::PRIMARY;
        }
        if pointer.buttons != 0 {
            flags |= pointer_flags::IN_CONTACT;
        }
        flags
    }

    fn cancel_record(&self, pointer: &Pointer) -> PointerEvent {
        PointerEvent {
            pointer_id: pointer.id,
            pointer_kind: pointer.kind,
            pointer_flags: self.pointer_flags(pointer) & !pointer_flags::IN_CONTACT,
            modifiers: self.modifiers,
            x: pointer.x,
            y: pointer.y,
            ..PointerEvent::default()
        }
    }

    /// A mouse cursor, a finger or a pen did something.
    pub fn pointer(&mut self, input: PointerInput) {
        self.sync();
        let mouse = input.kind == PointerKind::Mouse;
        let index = if mouse {
            None
        } else {
            match (self.pointer_index(&input), input.phase) {
                (Some(index), _) => Some(index),
                // A finger exists from its press, a pen from wherever it is
                // first seen. What comes of one never seen begin is dropped.
                (None, PointerPhase::Down | PointerPhase::Enter) => Some(self.adopt(&input)),
                (None, PointerPhase::Motion) if input.kind == PointerKind::Pen => {
                    Some(self.adopt(&input))
                }
                (None, _) => return,
            }
        };
        let mut pointer = match index {
            Some(index) => self.pointers[index].clone(),
            None => self.mouse.clone(),
        };

        let bit = match input.button {
            0 => 0,
            button @ 1..=32 => 1u32 << (button - 1),
            _ => 0,
        };
        let touch = input.kind == PointerKind::Touch;
        let (button, bit) = if touch && bit == 0 {
            (PointerButton::Primary as u32, pointer_buttons::PRIMARY)
        } else {
            (input.button, bit)
        };
        match input.phase {
            PointerPhase::Down => {
                if bit == 0 || pointer.buttons & bit != 0 {
                    return;
                }
                pointer.buttons |= bit;
            }
            PointerPhase::Up => {
                if pointer.buttons & bit == 0 {
                    return;
                }
                pointer.buttons &= !bit;
            }
            PointerPhase::Cancel => pointer.buttons = 0,
            _ => {}
        }

        // A locked cursor stays where it was; only its movement is real.
        let locked = mouse && self.window.snapshot.read().cursor_grab == CursorGrab::Locked;
        let (x, y) = if locked && pointer.placed {
            (pointer.x, pointer.y)
        } else {
            (input.x, input.y)
        };
        let (delta_x, delta_y) = match input.delta {
            Some(delta) => delta,
            None if pointer.placed => (x - pointer.x, y - pointer.y),
            None => (0.0, 0.0),
        };
        pointer.x = x;
        pointer.y = y;
        pointer.placed = true;

        let in_contact = pointer.buttons != 0;
        let mut flags = self.pointer_flags(&pointer);
        if input.pressure.is_some() {
            flags |= pointer_flags::HAS_PRESSURE;
        }
        if input.tilt.is_some() {
            flags |= pointer_flags::HAS_TILT;
        }
        let changes_button = matches!(input.phase, PointerPhase::Down | PointerPhase::Up);
        let record = PointerEvent {
            pointer_id: pointer.id,
            pointer_kind: pointer.kind,
            pointer_flags: flags,
            button: if changes_button { button } else { 0 },
            buttons: pointer.buttons,
            modifiers: self.modifiers,
            x,
            y,
            delta_x,
            delta_y,
            pressure: input.pressure.unwrap_or(if in_contact { 0.5 } else { 0.0 }),
            tangential_pressure: input.tangential_pressure,
            tilt_x: input.tilt.map_or(0.0, |tilt| tilt.0),
            tilt_y: input.tilt.map_or(0.0, |tilt| tilt.1),
            twist: input.twist,
            contact_width: input.contact_size.0,
            contact_height: input.contact_size.1,
            tool: input.tool,
            click_count: if changes_button { input.click_count } else { 0 },
        };

        // Does the pointer go on existing after this?
        let ends = match (input.kind, input.phase) {
            (PointerKind::Mouse, _) => false,
            (PointerKind::Touch, PointerPhase::Up) => pointer.buttons == 0,
            (_, PointerPhase::Cancel) => input.kind == PointerKind::Touch,
            (_, PointerPhase::Leave) => true,
            _ => false,
        };
        let slot = pointer.slot;
        match index {
            Some(index) if ends => {
                self.pointers.swap_remove(index);
                if self.primary_touch == Some(pointer.id) {
                    self.primary_touch = None;
                }
            }
            Some(index) => self.pointers[index] = pointer,
            None => self.mouse = pointer,
        }

        if mouse && matches!(input.phase, PointerPhase::Enter | PointerPhase::Leave) {
            let inside = input.phase == PointerPhase::Enter;
            self.window.snapshot.update(|data| {
                data.flags = (data.flags & !window_flags::HOVERED)
                    | if inside { window_flags::HOVERED } else { 0 };
            });
        }

        let event = match input.phase {
            PointerPhase::Enter => Event::PointerEnter(record),
            PointerPhase::Leave => Event::PointerLeave(record),
            PointerPhase::Down => Event::PointerDown(record),
            PointerPhase::Up => Event::PointerUp(record),
            PointerPhase::Cancel => Event::PointerCancel(record),
            PointerPhase::Motion => {
                let Some(slot) = slot else {
                    return self.emit(input.device, 0, Event::PointerMotion(record));
                };
                if !self.wanted(event_class::POINTER) {
                    return;
                }
                let motion = self.queued(input.device, 0, Event::PointerMotion(record));
                // What the guest has not collected is merged: the position is
                // the latest, and the movement adds up.
                self.window.motion_slot(slot).put(motion, |old, new| {
                    let (Event::PointerMotion(old), Event::PointerMotion(latest)) =
                        (&old.event, &new.event)
                    else {
                        return new;
                    };
                    if old.pointer_id != latest.pointer_id {
                        return new;
                    }
                    Queued {
                        flags: new.flags | event_flags::COALESCED,
                        event: Event::PointerMotion(PointerEvent {
                            delta_x: old.delta_x + latest.delta_x,
                            delta_y: old.delta_y + latest.delta_y,
                            ..latest.clone()
                        }),
                        ..new
                    }
                });
                self.instance.wait.notify();
                return;
            }
        };
        self.emit(input.device, 0, event);
    }

    /// A wheel turned or a touchpad scrolled.
    pub fn wheel(&mut self, input: WheelInput) {
        self.sync();
        let record = WheelEvent {
            unit: input.unit,
            phase: input.phase,
            wheel_flags: input.flags,
            modifiers: self.modifiers,
            delta_x: input.delta_x,
            delta_y: input.delta_y,
            x: input.x,
            y: input.y,
        };
        // The ends of a gesture are records of their own; what lies between,
        // and a plain wheel's notches, add up while they wait.
        if !matches!(input.phase, WheelPhase::None | WheelPhase::Changed) {
            return self.emit(input.device, 0, Event::Wheel(record));
        }
        if !self.wanted(event_class::WHEEL) {
            return;
        }
        let slot = self.window.wheel_slot();
        let waiting = slot.take();
        let wheel = match waiting {
            Some(Queued {
                event: Event::Wheel(old),
                ..
            }) if old.unit == record.unit
                && old.phase == record.phase
                && old.wheel_flags == record.wheel_flags
                && old.modifiers == record.modifiers =>
            {
                self.queued(
                    input.device,
                    event_flags::COALESCED,
                    Event::Wheel(WheelEvent {
                        delta_x: old.delta_x + record.delta_x,
                        delta_y: old.delta_y + record.delta_y,
                        ..record
                    }),
                )
            }
            other => {
                // Of another kind: it goes out as it is, ahead of this one.
                if let Some(other) = other {
                    self.flush();
                    self.instance.push(other);
                }
                self.queued(input.device, 0, Event::Wheel(record))
            }
        };
        slot.put(wheel, |_, new| new);
        self.instance.wait.notify();
    }
}

/// Tells whether the guest a [`WindowSink`] reports to is still there. Unlike
/// the sink, it can be asked from any thread.
#[derive(Clone)]
pub struct SinkWatch {
    instance: Arc<InstanceShared>,
    window: Arc<WindowShared>,
}

impl SinkWatch {
    /// The guest destroyed the window, or has gone altogether.
    pub fn is_closed(&self) -> bool {
        self.window.closed.load(Ordering::Acquire) || self.instance.is_closed()
    }
}

impl Drop for WindowSink {
    /// A window that goes away while it has focus no longer counts as a
    /// focused window of its instance.
    fn drop(&mut self) {
        if self.focused {
            update(&self.instance.focused_windows, |count| count.checked_sub(1));
        }
    }
}
