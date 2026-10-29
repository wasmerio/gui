//! What the producers of input and a guest's threads share.
//!
//! Input is produced by whatever thread the window system lives on (a native
//! event loop's, the page's in a browser) and consumed by the guest's threads
//! when they poll. The producer may not wait for a lock (see [`super::wait`]),
//! so everything it touches here is an atomic or a lock-free queue. The
//! mutexes in this file are only ever taken by guest threads.
//!
//! Three kinds of records travel from producer to guest:
//!
//! - most go through a bounded queue, in order;
//! - a few kinds of which only the latest matters (pointer motion, wheel
//!   deltas, the window's size) sit in one-element mailboxes and are merged
//!   there, so a guest that falls behind is not buried under them;
//! - completions of requests, each of which the guest is owed exactly once,
//!   travel beside the queue so that a flood of input cannot push them out.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_queue::{ArrayQueue, SegQueue};

use super::clipboard::Written;
use super::lock;
use super::wait::WaitSet;
use crate::abi::enums::*;
use crate::abi::events::*;
use crate::abi::{AbiEnum, Handle};
use crate::system::WindowSystem;

/// Pointers of one window whose motion is merged while unread: the mouse, a
/// pen, and a hand of fingers. Further pointers still work, unmerged.
pub(crate) const MOTION_SLOTS: usize = 12;

/// Changes an atomic to what `f` makes of it, unless `f` declines. `true`
/// when it was changed. (What the standard library offers for this goes by
/// different names from one toolchain to the next.)
pub(crate) fn update(value: &AtomicU32, f: impl Fn(u32) -> Option<u32>) -> bool {
    let mut current = value.load(Ordering::Acquire);
    loop {
        let Some(next) = f(current) else {
            return false;
        };
        match value.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return true,
            Err(actual) => current = actual,
        }
    }
}

/// Completions a guest may leave unread before further requests are refused.
pub(crate) const MAX_OWED_COMPLETIONS: u32 = 256;

/// One event on its way to a guest.
#[derive(Clone, Debug)]
pub(crate) struct Queued {
    pub window: Handle,
    pub device: u32,
    /// Nanoseconds on the window system's clock.
    pub time: u64,
    /// `WGUIEventFlags`.
    pub flags: u32,
    pub event: Event,
}

/// A mailbox for the latest value of something.
pub(crate) struct Slot(ArrayQueue<Queued>);

impl Slot {
    fn new() -> Self {
        Self(ArrayQueue::new(1))
    }

    pub(crate) fn take(&self) -> Option<Queued> {
        self.0.pop()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Puts `new` in. If the guest has not collected what was there, the two
    /// are merged by `merge(old, new)` first.
    ///
    /// Only the one producer of a slot calls this. The consumer may take the
    /// old value at any moment; then there is nothing to merge with.
    pub(crate) fn put(&self, new: Queued, merge: impl FnOnce(Queued, Queued) -> Queued) {
        let value = match self.0.pop() {
            Some(old) => merge(old, new),
            None => new,
        };
        self.0.force_push(value);
    }
}

/// What the host knows of a window, as the window system last reported it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WindowData {
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
    pub x: i32,
    pub y: i32,
    /// Left, top, right, bottom.
    pub frame: [u32; 4],
    pub safe_area: [u32; 4],
    /// `WGUIWindowFlags`.
    pub flags: u64,
    /// The window system's id of the display the window is on, or 0.
    pub display: u64,
    pub fullscreen: FullscreenMode,
    pub cursor_grab: CursorGrab,
    pub theme: Theme,
    pub modifiers: u32,
    /// Goes up with every change.
    pub serial: u32,
}

impl Default for WindowData {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            scale_factor: 1.0,
            x: POSITION_UNDEFINED,
            y: POSITION_UNDEFINED,
            frame: [0; 4],
            safe_area: [0; 4],
            flags: 0,
            display: 0,
            fullscreen: FullscreenMode::None,
            cursor_grab: CursorGrab::None,
            theme: Theme::Unknown,
            modifiers: 0,
            serial: 0,
        }
    }
}

const SNAPSHOT_WORDS: usize = 12;

fn pair(low: u32, high: u32) -> u64 {
    u64::from(low) | (u64::from(high) << 32)
}

fn unpair(word: u64) -> (u32, u32) {
    (word as u32, (word >> 32) as u32)
}

/// A [`WindowData`] that its one writer updates and anyone reads, neither
/// ever waiting: a sequence lock over atomic words.
pub(crate) struct Snapshot {
    /// Odd while a write is in progress.
    sequence: AtomicU32,
    words: [AtomicU64; SNAPSHOT_WORDS],
}

impl Default for Snapshot {
    fn default() -> Self {
        let snapshot = Self {
            sequence: AtomicU32::new(0),
            words: std::array::from_fn(|_| AtomicU64::new(0)),
        };
        snapshot.store(&WindowData::default());
        snapshot
    }
}

impl Snapshot {
    fn store(&self, data: &WindowData) {
        let words = [
            pair(data.width, data.height),
            data.scale_factor.to_bits(),
            pair(data.x as u32, data.y as u32),
            pair(data.frame[0], data.frame[1]),
            pair(data.frame[2], data.frame[3]),
            pair(data.safe_area[0], data.safe_area[1]),
            pair(data.safe_area[2], data.safe_area[3]),
            data.flags,
            data.display,
            pair(data.fullscreen as u32, data.cursor_grab as u32),
            pair(data.theme as u32, data.modifiers),
            u64::from(data.serial),
        ];
        self.sequence.fetch_add(1, Ordering::AcqRel);
        for (slot, word) in self.words.iter().zip(words) {
            slot.store(word, Ordering::Release);
        }
        self.sequence.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn read(&self) -> WindowData {
        loop {
            let before = self.sequence.load(Ordering::Acquire);
            if !before.is_multiple_of(2) {
                std::hint::spin_loop();
                continue;
            }
            let words: [u64; SNAPSHOT_WORDS] =
                std::array::from_fn(|index| self.words[index].load(Ordering::Acquire));
            if self.sequence.load(Ordering::Acquire) != before {
                continue;
            }
            let (width, height) = unpair(words[0]);
            let (x, y) = unpair(words[2]);
            let (frame_left, frame_top) = unpair(words[3]);
            let (frame_right, frame_bottom) = unpair(words[4]);
            let (safe_left, safe_top) = unpair(words[5]);
            let (safe_right, safe_bottom) = unpair(words[6]);
            let (fullscreen, cursor_grab) = unpair(words[9]);
            let (theme, modifiers) = unpair(words[10]);
            return WindowData {
                width,
                height,
                scale_factor: f64::from_bits(words[1]),
                x: x as i32,
                y: y as i32,
                frame: [frame_left, frame_top, frame_right, frame_bottom],
                safe_area: [safe_left, safe_top, safe_right, safe_bottom],
                flags: words[7],
                display: words[8],
                fullscreen: FullscreenMode::from_u32(fullscreen).unwrap_or_default(),
                cursor_grab: CursorGrab::from_u32(cursor_grab).unwrap_or_default(),
                theme: Theme::from_u32(theme).unwrap_or_default(),
                modifiers,
                serial: words[11] as u32,
            };
        }
    }

    /// Changes the data, and bumps its serial if anything is different.
    /// Only the window's one producer calls this. Returns (before, after).
    pub(crate) fn update(&self, change: impl FnOnce(&mut WindowData)) -> (WindowData, WindowData) {
        let before = self.read();
        let mut after = before;
        change(&mut after);
        if after != before {
            after.serial = before.serial.wrapping_add(1);
            self.store(&after);
        }
        (before, after)
    }
}

/// What the producer and the guest share about one window.
pub(crate) struct WindowShared {
    /// The guest's handle for the window.
    pub(crate) handle: AtomicU32,
    pub(crate) snapshot: Snapshot,
    /// The guest destroyed the window: nothing more is reported for it.
    pub(crate) closed: AtomicBool,
    /// A text entry session is on.
    pub(crate) text_input: AtomicBool,
    /// [`InstanceShared::waits`] when the guest was last handed a
    /// `WindowRedrawRequested` of the window; `u64::MAX` for never.
    pub(crate) redrawn_at_wait: AtomicU64,
    /// Nothing the guest drew is in the window at its present size: it is
    /// new, or its size changed (a canvas is cleared then). The next redraw
    /// the guest is handed is an exposure, whenever it was queued.
    pub(crate) exposed: AtomicBool,
    resize: Slot,
    moved: Slot,
    wheel: Slot,
    redraw: Slot,
    motion: [Slot; MOTION_SLOTS],
}

impl Default for WindowShared {
    fn default() -> Self {
        Self {
            handle: AtomicU32::new(0),
            snapshot: Snapshot::default(),
            closed: AtomicBool::new(false),
            text_input: AtomicBool::new(false),
            redrawn_at_wait: AtomicU64::new(u64::MAX),
            exposed: AtomicBool::new(true),
            resize: Slot::new(),
            moved: Slot::new(),
            wheel: Slot::new(),
            redraw: Slot::new(),
            motion: std::array::from_fn(|_| Slot::new()),
        }
    }
}

impl WindowShared {
    pub(crate) fn handle(&self) -> Handle {
        self.handle.load(Ordering::Acquire)
    }

    pub(crate) fn resize_slot(&self) -> &Slot {
        &self.resize
    }

    pub(crate) fn moved_slot(&self) -> &Slot {
        &self.moved
    }

    pub(crate) fn wheel_slot(&self) -> &Slot {
        &self.wheel
    }

    pub(crate) fn redraw_slot(&self) -> &Slot {
        &self.redraw
    }

    pub(crate) fn motion_slot(&self, index: usize) -> &Slot {
        &self.motion[index]
    }

    /// The mailboxes whose content has to reach the guest before any later
    /// record of this window, in the order they are delivered.
    fn ordered_slots(&self) -> impl Iterator<Item = &Slot> {
        [&self.resize, &self.moved]
            .into_iter()
            .chain(self.motion.iter())
            .chain([&self.wheel])
    }

    /// Moves what the mailboxes hold into `out`. Returns whether there was
    /// anything.
    /// With `redraw_reasons`, a redraw handed over now is an exposure if the
    /// window is (`exposed`), or if a resize goes with it: the sink marks the
    /// window only after it queued the resize.
    fn take_slots(&self, out: &mut VecDeque<Queued>, redraw_reasons: bool) -> bool {
        let before = out.len();
        let mut resized = false;
        for slot in self.ordered_slots() {
            if let Some(queued) = slot.take() {
                resized |= std::ptr::eq(slot, &self.resize);
                out.push_back(queued);
            }
        }
        if let Some(mut redraw) = self.redraw.take() {
            if redraw_reasons && (self.exposed.swap(false, Ordering::AcqRel) || resized) {
                redraw.flags |= event_flags::EXPOSURE;
            }
            out.push_back(redraw);
        }
        out.len() != before
    }

    fn slots_empty(&self) -> bool {
        self.ordered_slots().all(Slot::is_empty) && self.redraw.is_empty()
    }
}

/// A key the guest has been told is down.
#[derive(Clone, Debug)]
struct HeldKey {
    window: Handle,
    device: u32,
    record: KeyEvent,
}

/// A pointer the guest has been told about.
#[derive(Clone, Debug)]
struct SeenPointer {
    window: Handle,
    device: u32,
    record: PointerEvent,
}

/// What only guest threads touch: the state of delivery.
#[derive(Default)]
struct Consumer {
    sequence: u32,
    last_time: u64,
    /// Records ready to be delivered ahead of everything else: ones that did
    /// not fit the guest's buffer, a resynchronisation, emptied mailboxes.
    pending: VecDeque<Queued>,
    /// What the guest currently believes about keys and pointers, so that it
    /// can be corrected after records were dropped.
    keys: Vec<HeldKey>,
    pointers: Vec<SeenPointer>,
}

impl Consumer {
    /// Notes what a record about to be delivered tells the guest.
    fn observe(&mut self, queued: &Queued) {
        match &queued.event {
            Event::KeyDown(key) => {
                let known = self.keys.iter().any(|held| {
                    held.window == queued.window
                        && held.record.physical_key == key.physical_key
                        && held.record.native_scancode == key.native_scancode
                });
                if !known {
                    self.keys.push(HeldKey {
                        window: queued.window,
                        device: queued.device,
                        record: key.clone(),
                    });
                }
            }
            Event::KeyUp(key) => self.keys.retain(|held| {
                held.window != queued.window
                    || held.record.physical_key != key.physical_key
                    || held.record.native_scancode != key.native_scancode
            }),
            Event::PointerDown(pointer) | Event::PointerUp(pointer) => {
                self.pointers.retain(|seen| {
                    seen.window != queued.window || seen.record.pointer_id != pointer.pointer_id
                });
                if pointer.buttons != 0 {
                    self.pointers.push(SeenPointer {
                        window: queued.window,
                        device: queued.device,
                        record: pointer.clone(),
                    });
                }
            }
            Event::PointerCancel(pointer) | Event::PointerLeave(pointer) => {
                self.pointers.retain(|seen| {
                    seen.window != queued.window || seen.record.pointer_id != pointer.pointer_id
                });
            }
            _ => {}
        }
    }

    fn forget_window(&mut self, window: Handle) {
        self.keys.retain(|held| held.window != window);
        self.pointers.retain(|seen| seen.window != window);
    }

    /// Queues what tells the guest that records were dropped, and what makes
    /// its view right again: releases for what it believes held, and each
    /// window's present geometry, flags and modifiers.
    fn resynchronise(&mut self, time: u64, windows: &[Arc<WindowShared>], lost: u32, classes: u64) {
        let synthetic = event_flags::SYNTHETIC;
        self.pending.push_back(Queued {
            window: 0,
            device: 0,
            time,
            flags: 0,
            event: Event::EventsLost(EventsLostEvent {
                count: lost,
                classes,
            }),
        });
        for held in self.keys.drain(..) {
            self.pending.push_back(Queued {
                window: held.window,
                device: held.device,
                time,
                flags: synthetic,
                event: Event::KeyUp(KeyEvent {
                    key_flags: 0,
                    text: String::new(),
                    text_with_all_modifiers: String::new(),
                    ..held.record
                }),
            });
        }
        for seen in self.pointers.drain(..) {
            self.pending.push_back(Queued {
                window: seen.window,
                device: seen.device,
                time,
                flags: synthetic,
                event: Event::PointerCancel(PointerEvent {
                    button: 0,
                    buttons: 0,
                    pointer_flags: seen.record.pointer_flags & !pointer_flags::IN_CONTACT,
                    delta_x: 0.0,
                    delta_y: 0.0,
                    pressure: 0.0,
                    click_count: 0,
                    ..seen.record
                }),
            });
        }
        for window in windows {
            let handle = window.handle();
            let data = window.snapshot.read();
            let mut push = |event| {
                self.pending.push_back(Queued {
                    window: handle,
                    device: 0,
                    time,
                    flags: synthetic,
                    event,
                });
            };
            push(Event::WindowResized(WindowResizedEvent {
                width: data.width,
                height: data.height,
                scale_factor: data.scale_factor,
                changed: resize_flags::SIZE | resize_flags::SCALE_FACTOR,
            }));
            push(Event::WindowFlagsChanged(WindowFlagsChangedEvent {
                flags: data.flags,
                changed: 0,
            }));
            if data.flags & window_flags::FOCUSED != 0 {
                push(Event::ModifiersChanged(ModifiersChangedEvent {
                    modifiers: data.modifiers,
                }));
            }
        }
    }
}

/// What the producers and the guest share about one instance.
pub(crate) struct InstanceShared {
    pub(crate) system: Arc<dyn WindowSystem>,
    /// The window system tells why a window is redrawn
    /// (`WGUIFeatureName_RedrawReasons`): only then do redraws carry a reason.
    pub(crate) redraw_reasons: bool,
    queue: ArrayQueue<Queued>,
    completions: SegQueue<Queued>,
    /// Requests accepted whose completion the guest has not read yet.
    owed: AtomicU32,
    raw_motion: Slot,
    /// The `WGUIEventClass` bits the guest wants queued.
    pub(crate) event_mask: AtomicU64,
    /// Records dropped since the guest was last told, and their classes.
    lost: AtomicU32,
    lost_classes: AtomicU64,
    /// The queue filled up, and nothing is queued until the guest has read
    /// what is in it and been told what it missed.
    overflowed: AtomicBool,
    /// Goes up whenever records are dropped. A producer that sees it change
    /// forgets which keys and buttons it believes the guest holds.
    pub(crate) resync_epoch: AtomicU32,
    pub(crate) wait: WaitSet,
    /// The guest destroyed the instance.
    pub(crate) closed: AtomicBool,
    /// When a paste chord was last pressed in a focused window of the
    /// instance, on the window system's clock; `u64::MAX` for never.
    pub(crate) paste_stamp: AtomicU64,
    /// How many of the instance's windows have keyboard focus.
    pub(crate) focused_windows: AtomicU32,
    /// Threads inside `wguiInstanceWaitEvents`.
    pub(crate) waiting: AtomicU32,
    /// Calls of `wguiInstanceWaitEvents` so far.
    pub(crate) waits: AtomicU64,
    /// Records delivered so far.
    pub(crate) delivered: AtomicU64,
    consumer: Mutex<Consumer>,
    windows: Mutex<Vec<Arc<WindowShared>>>,
    /// (window system display id, guest handle) of the displays the guest
    /// has been given handles for.
    pub(crate) displays: Mutex<Vec<(u64, Handle)>>,
    /// What the instance wrote to the clipboards and can read back.
    pub(crate) written: Mutex<Written>,
    /// The embedder lets the guest read nothing others put on a clipboard,
    /// so it is not told what is there either.
    clipboard_hidden: bool,
}

impl InstanceShared {
    pub(crate) fn new(
        system: Arc<dyn WindowSystem>,
        capacity: usize,
        event_mask: u64,
        clipboard_hidden: bool,
    ) -> Self {
        let redraw_reasons = system.info().features.contains(&FeatureName::RedrawReasons);
        Self {
            system,
            redraw_reasons,
            queue: ArrayQueue::new(capacity.max(16)),
            completions: SegQueue::new(),
            owed: AtomicU32::new(0),
            raw_motion: Slot::new(),
            event_mask: AtomicU64::new(event_mask | event_class::SYSTEM),
            lost: AtomicU32::new(0),
            lost_classes: AtomicU64::new(0),
            overflowed: AtomicBool::new(false),
            resync_epoch: AtomicU32::new(0),
            wait: WaitSet::default(),
            closed: AtomicBool::new(false),
            paste_stamp: AtomicU64::new(u64::MAX),
            focused_windows: AtomicU32::new(0),
            waiting: AtomicU32::new(0),
            waits: AtomicU64::new(0),
            delivered: AtomicU64::new(0),
            consumer: Mutex::new(Consumer::default()),
            windows: Mutex::new(Vec::new()),
            displays: Mutex::new(Vec::new()),
            written: Mutex::new(Written::default()),
            clipboard_hidden,
        }
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Whether the guest wants events of this class.
    pub(crate) fn wants(&self, class: u64) -> bool {
        self.event_mask.load(Ordering::Acquire) & class != 0
    }

    // -- producing (lock-free) ------------------------------------------------

    /// Queues a record. Returns whether it was queued: not when the guest
    /// does not want its class, has gone, or has let the queue fill up.
    pub(crate) fn push(&self, queued: Queued) -> bool {
        let class = queued.event.class();
        if self.is_closed() || !self.wants(class) {
            return false;
        }
        let dropped = self.overflowed.load(Ordering::Acquire) || self.queue.push(queued).is_err();
        if dropped {
            // Set before the count, which is what the consumer goes by: it
            // may clear the flag only once it has seen every drop counted.
            self.overflowed.store(true, Ordering::Release);
            self.lost_classes.fetch_or(class, Ordering::AcqRel);
            self.lost.fetch_add(1, Ordering::AcqRel);
            self.resync_epoch.fetch_add(1, Ordering::AcqRel);
        }
        self.wait.notify();
        !dropped
    }

    /// Reserves the completion a request about to be accepted will need.
    /// `false` when the guest has too many unread: the request is refused.
    pub(crate) fn owe_completion(&self) -> bool {
        update(&self.owed, |owed| {
            (owed < MAX_OWED_COMPLETIONS).then_some(owed + 1)
        })
    }

    /// Queues the completion of a request. Never dropped while the instance
    /// lives: it goes in with everything else, in order, and beside the
    /// queue when that is full.
    pub(crate) fn push_completion(&self, queued: Queued) {
        if self.is_closed() {
            return;
        }
        let queued = if self.overflowed.load(Ordering::Acquire) {
            Some(queued)
        } else {
            self.queue.push(queued).err()
        };
        if let Some(queued) = queued {
            self.completions.push(queued);
        }
        self.wait.notify();
    }

    /// Adds to the device motion the guest has not collected yet.
    pub(crate) fn put_raw_motion(&self, queued: Queued) {
        if self.is_closed() || !self.wants(event_class::RAW) {
            return;
        }
        self.raw_motion.put(queued, |old, new| {
            let (Event::RawMotion(old), Event::RawMotion(delta)) = (&old.event, &new.event) else {
                return new;
            };
            Queued {
                flags: new.flags | event_flags::COALESCED,
                event: Event::RawMotion(RawMotionEvent {
                    delta_x: old.delta_x + delta.delta_x,
                    delta_y: old.delta_y + delta.delta_y,
                }),
                ..new
            }
        });
        self.wait.notify();
    }

    // -- consuming (guest threads) ---------------------------------------------

    pub(crate) fn add_window(&self, window: &Arc<WindowShared>) {
        lock(&self.windows).push(Arc::clone(window));
    }

    pub(crate) fn remove_window(&self, window: &Arc<WindowShared>) {
        lock(&self.windows).retain(|other| !Arc::ptr_eq(other, window));
        lock(&self.consumer).forget_window(window.handle());
    }

    /// The guest's handle for a display of the window system, or 0 if it
    /// has none.
    pub(crate) fn display_handle(&self, id: u64) -> Handle {
        lock(&self.displays)
            .iter()
            .find_map(|(known, handle)| (*known == id).then_some(*handle))
            .unwrap_or(0)
    }

    /// Whether a poll would return anything.
    pub(crate) fn has_events(&self) -> bool {
        if !self.queue.is_empty()
            || !self.completions.is_empty()
            || self.lost.load(Ordering::Acquire) != 0
            || !self.raw_motion.is_empty()
        {
            return true;
        }
        if !lock(&self.consumer).pending.is_empty() {
            return true;
        }
        lock(&self.windows)
            .iter()
            .any(|window| !window.slots_empty())
    }

    /// The next record to deliver, if there is any.
    fn next(&self, consumer: &mut Consumer) -> Option<Queued> {
        loop {
            if let Some(queued) = consumer.pending.pop_front() {
                return Some(queued);
            }
            if let Some(queued) = self.completions.pop() {
                return Some(queued);
            }
            if let Some(queued) = self.queue.pop() {
                return Some(queued);
            }
            // The queue is read to its end. If records were dropped, this is
            // where they are missing.
            let lost = self.lost.swap(0, Ordering::AcqRel);
            if lost != 0 {
                let classes = self.lost_classes.swap(0, Ordering::AcqRel);
                let windows = lock(&self.windows).clone();
                consumer.resynchronise(self.system.now(), &windows, lost, classes);
                self.overflowed.store(false, Ordering::Release);
                continue;
            }
            // Then come the latest values of what is only kept as that.
            let windows = lock(&self.windows).clone();
            let mut any = false;
            for window in &windows {
                any |= window.take_slots(&mut consumer.pending, self.redraw_reasons);
            }
            if let Some(queued) = self.raw_motion.take() {
                consumer.pending.push_back(queued);
                any = true;
            }
            if !any {
                return None;
            }
        }
    }

    /// Notes that the guest was handed a redraw of a window: until it next
    /// waits for events, it is drawing that window's frame.
    fn handed_redraw(&self, window: Handle) {
        let waits = self.waits.load(Ordering::Acquire);
        if let Some(window) = lock(&self.windows)
            .iter()
            .find(|shared| shared.handle() == window)
        {
            window.redrawn_at_wait.store(waits, Ordering::Release);
        }
    }

    /// Whether the guest is still drawing the frame it was last told to draw
    /// of a window: it has not waited for events since it was handed it.
    pub(crate) fn drawing(&self, window: &WindowShared) -> bool {
        window.redrawn_at_wait.load(Ordering::Acquire) == self.waits.load(Ordering::Acquire)
    }

    /// Encodes as many whole records as fit `capacity` bytes, oldest first.
    pub(crate) fn poll(&self, capacity: usize) -> Vec<u8> {
        let mut consumer = lock(&self.consumer);
        let mut out = Vec::new();
        while let Some(mut queued) = self.next(&mut consumer) {
            if let Event::WindowDisplayChanged(change) = &mut queued.event {
                // The producer knows the display by the window system's id;
                // the guest knows it by a handle, if it has asked for one.
                let id = lock(&self.windows)
                    .iter()
                    .find(|window| window.handle() == queued.window)
                    .map_or(0, |window| window.snapshot.read().display);
                change.display = self.display_handle(id);
            }
            if let Event::ClipboardChanged(change) = &mut queued.event {
                if lock(&self.written).owns(change.selection, change.clipboard_sequence) {
                    change.clipboard_flags |= clipboard_flags::OWNED;
                } else if self.clipboard_hidden {
                    // What the guest may never read, it is not told about.
                    change.clipboard_flags &= !clipboard_flags::HAS_TEXT;
                }
            }
            let header = EventHeader {
                flags: queued.flags,
                window: queued.window,
                timestamp: queued.time.max(consumer.last_time),
                device: queued.device,
                sequence: consumer.sequence,
            };
            let before = out.len();
            queued.event.encode(&header, &mut out);
            if out.len() > capacity {
                out.truncate(before);
                consumer.pending.push_front(queued);
                break;
            }
            consumer.observe(&queued);
            consumer.sequence = consumer.sequence.wrapping_add(1);
            consumer.last_time = header.timestamp;
            if matches!(queued.event, Event::RequestCompleted(_)) {
                update(&self.owed, |owed| owed.checked_sub(1));
            }
            if matches!(queued.event, Event::WindowRedrawRequested) {
                self.handed_redraw(queued.window);
            }
            self.delivered.fetch_add(1, Ordering::AcqRel);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_reads_back_what_was_stored() {
        let snapshot = Snapshot::default();
        assert_eq!(snapshot.read(), WindowData::default());
        let (before, after) = snapshot.update(|data| {
            data.width = 800;
            data.height = 600;
            data.scale_factor = 1.5;
            data.x = -40;
            data.y = i32::MIN + 1;
            data.frame = [1, 2, 3, 4];
            data.safe_area = [5, 6, 7, 8];
            data.flags = window_flags::VISIBLE | window_flags::FOCUSED;
            data.display = u64::MAX - 1;
            data.fullscreen = FullscreenMode::Borderless;
            data.cursor_grab = CursorGrab::Locked;
            data.theme = Theme::Dark;
            data.modifiers = modifiers::SHIFT | modifiers::LEFT_SHIFT;
        });
        assert_eq!(before.serial, 0);
        assert_eq!(after.serial, 1);
        assert_eq!(snapshot.read(), after);
        // Nothing changes, nothing is bumped.
        let (_, same) = snapshot.update(|data| data.width = 800);
        assert_eq!(same.serial, 1);
    }

    #[test]
    fn a_snapshot_is_never_read_half_written() {
        let snapshot = Arc::new(Snapshot::default());
        let done = Arc::new(AtomicBool::new(false));
        let reader = {
            let (snapshot, done) = (Arc::clone(&snapshot), Arc::clone(&done));
            std::thread::spawn(move || {
                while !done.load(Ordering::Acquire) {
                    let data = snapshot.read();
                    assert_eq!(data.width, data.height, "torn read");
                    assert_eq!(u64::from(data.width), data.flags, "torn read");
                }
            })
        };
        for value in 1..20_000u32 {
            snapshot.update(|data| {
                data.width = value;
                data.height = value;
                data.flags = u64::from(value);
            });
        }
        done.store(true, Ordering::Release);
        reader.join().expect("reads were consistent");
    }
}
