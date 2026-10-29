//! How events come out, checked on the headless window system: the promises
//! of `gui.h` about order, pairing, merging and loss, one test each.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use wasmer::{Memory, MemoryType, Store};

use super::shared::InstanceShared;
use super::{Host, lock};
use crate::abi::api::{InstanceApi, WindowApi};
use crate::abi::enums::*;
use crate::abi::events::*;
use crate::abi::structs::*;
use crate::abi::{AbiEnum, Cx, GuestMemory, Handle, OutPtr};
use crate::headless::{HeadlessClock, HeadlessOptions, HeadlessSystem, HeadlessWindow};
use crate::system::{
    HostWindow, KeyInput, PendingRequest, PointerInput, PointerPhase, SystemInfo, WheelInput,
    WindowAttributes, WindowChange, WindowSink, WindowSystem,
};
use crate::{ClipboardRead, GuiCtx, GuiLimits, GuiPolicy, GuiScope};

/// A guest's side of things without a guest: a scope on a headless system,
/// one instance, and a page of memory to pass pointers into.
pub(crate) struct Rig {
    pub(crate) system: HeadlessSystem,
    pub(crate) scope: GuiScope,
    pub(crate) instance: Handle,
    store: Store,
    memory: Memory,
}

fn manual() -> HeadlessOptions {
    HeadlessOptions {
        clock: HeadlessClock::Manual,
        ..HeadlessOptions::default()
    }
}

impl Rig {
    fn new() -> Self {
        Self::with(manual(), GuiPolicy::default(), GuiLimits::default())
    }

    fn with(options: HeadlessOptions, policy: GuiPolicy, limits: GuiLimits) -> Self {
        let system = HeadlessSystem::new(options);
        Self::on(Arc::new(system.clone()), system, policy, limits)
    }

    /// A rig whose guests live on `backend`; `system` is only kept for its
    /// clock and clipboard.
    pub(crate) fn on(
        backend: Arc<dyn WindowSystem>,
        system: HeadlessSystem,
        policy: GuiPolicy,
        limits: GuiLimits,
    ) -> Self {
        let ctx = GuiCtx::builder()
            .window_system(backend)
            .policy(policy)
            .limits(limits)
            .build();
        let scope = ctx.scope();
        let instance = scope
            .host()
            .open_instance(&InstanceDescriptor {
                abi_version: ABI_VERSION,
                event_mask: event_class::DEFAULT,
                ..InstanceDescriptor::default()
            })
            .expect("an instance");
        let mut store = Store::default();
        let memory = Memory::new(&mut store, MemoryType::new(1, None, false)).expect("a page");
        Self {
            system,
            scope,
            instance,
            store,
            memory,
        }
    }

    pub(crate) fn host(&self) -> &Arc<Host> {
        self.scope.host()
    }

    fn shared(&self) -> Arc<InstanceShared> {
        Arc::clone(&self.host().instance(self.instance).expect("alive").shared)
    }

    /// Runs a function of the API as a guest would call it.
    pub(crate) fn call<R>(&self, f: impl FnOnce(&Host, &mut Cx<'_>) -> R) -> R {
        let mut owned = Vec::new();
        let stopped = || false;
        let mut cx = Cx {
            mem: GuestMemory::new(self.memory.view(&self.store)),
            owned: &mut owned,
            stopped: &stopped,
        };
        f(self.host(), &mut cx)
    }

    pub(crate) fn write(&self, ptr: u32, bytes: &[u8]) {
        self.memory
            .view(&self.store)
            .write(u64::from(ptr), bytes)
            .expect("in bounds");
    }

    pub(crate) fn read(&self, ptr: u32, len: usize) -> Vec<u8> {
        let mut bytes = vec![0; len];
        self.memory
            .view(&self.store)
            .read(u64::from(ptr), &mut bytes)
            .expect("in bounds");
        bytes
    }

    pub(crate) fn read_u32(&self, ptr: u32) -> u32 {
        u32::from_le_bytes(self.read(ptr, 4).try_into().expect("4 bytes"))
    }

    /// Opens a window and reads the record that announces it.
    fn window(&self) -> (Handle, HeadlessWindow) {
        let handle = self
            .host()
            .create_window(self.instance, None)
            .expect("a window");
        let user = self
            .system
            .windows()
            .into_iter()
            .find(|window| window.handle() == handle)
            .expect("the system has it");
        let announced = self.events();
        assert!(
            matches!(announced[..], [(_, Event::WindowResized(_))]),
            "{announced:?}"
        );
        (handle, user)
    }

    /// Everything the guest would read now.
    fn records(&self) -> Vec<(EventHeader, Event)> {
        let bytes = self.shared().poll(1 << 20);
        let mut at = 0;
        let mut out = Vec::new();
        while at < bytes.len() {
            let (header, event, size) = Event::decode(&bytes[at..]).expect("a valid record");
            out.push((header, event));
            at += size;
        }
        out
    }

    pub(crate) fn events(&self) -> Vec<(EventHeader, Event)> {
        self.records()
    }

    /// The same, as short names: `KeyDown`, `WindowFocusLost`.
    pub(crate) fn names(&self) -> Vec<String> {
        self.records()
            .iter()
            .map(|(_, event)| event.ty().name().to_owned())
            .collect()
    }
}

pub(crate) fn key(physical_key: PhysicalKey, pressed: bool) -> KeyInput {
    KeyInput {
        physical_key,
        pressed,
        ..KeyInput::default()
    }
}

fn typed(physical_key: PhysicalKey, text: &str) -> KeyInput {
    KeyInput {
        physical_key,
        key: u32::from(text.chars().next().unwrap_or('\0')),
        pressed: true,
        text: text.to_owned(),
        ..KeyInput::default()
    }
}

fn pointer(phase: PointerPhase, x: f64, y: f64) -> PointerInput {
    PointerInput {
        phase,
        x,
        y,
        ..PointerInput::default()
    }
}

fn button(phase: PointerPhase, button: u32, x: f64, y: f64) -> PointerInput {
    PointerInput {
        phase,
        button,
        x,
        y,
        ..PointerInput::default()
    }
}

fn touch(source: u64, phase: PointerPhase, x: f64, y: f64) -> PointerInput {
    PointerInput {
        kind: PointerKind::Touch,
        source,
        phase,
        x,
        y,
        ..PointerInput::default()
    }
}

fn notch(delta_y: f64) -> WheelInput {
    WheelInput {
        unit: WheelUnit::Line,
        phase: WheelPhase::None,
        flags: 0,
        delta_x: 0.0,
        delta_y,
        x: 0.0,
        y: 0.0,
        device: 0,
    }
}

// -- windows -------------------------------------------------------------------

#[test]
fn a_window_is_announced_by_its_size_and_by_nothing_before() {
    let rig = Rig::new();
    let handle = rig
        .host()
        .create_window(rig.instance, None)
        .expect("a window");
    let events = rig.events();
    assert_eq!(events.len(), 1, "{events:?}");
    let (header, event) = &events[0];
    assert_eq!(header.window, handle);
    assert_eq!(header.flags, event_flags::SYNTHETIC);
    assert_eq!(header.sequence, 0);
    assert_eq!(
        *event,
        Event::WindowResized(WindowResizedEvent {
            width: 800,
            height: 600,
            scale_factor: 1.0,
            changed: resize_flags::SIZE | resize_flags::SCALE_FACTOR,
        })
    );
    // What the window system said before that is in the window's state.
    let state = rig
        .host()
        .window(handle)
        .expect("alive")
        .shared
        .snapshot
        .read();
    assert_eq!(
        state.flags,
        window_flags::VISIBLE | window_flags::RESIZABLE | window_flags::DECORATED
    );
    assert_eq!(state.theme, Theme::Light);
    assert_ne!(state.display, 0);
}

#[test]
fn resizes_nobody_read_become_the_latest_one() {
    let rig = Rig::new();
    let (_, user) = rig.window();
    user.resize(801, 600);
    user.resize(802, 601);
    user.resize(803, 602);
    let events = rig.events();
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].0.flags, event_flags::COALESCED);
    assert_eq!(
        events[0].1,
        Event::WindowResized(WindowResizedEvent {
            width: 803,
            height: 602,
            scale_factor: 1.0,
            changed: resize_flags::SIZE,
        })
    );
    // Read one by one, each is its own and none is marked merged.
    user.resize(900, 700);
    assert_eq!(rig.events()[0].0.flags, 0);
    // The same size again is no event.
    user.resize(900, 700);
    assert!(rig.events().is_empty());
}

#[test]
fn flag_changes_say_what_changed() {
    let rig = Rig::new();
    let (handle, _user) = rig.window();
    let status = rig.call(|host, cx| {
        host.window_set_flags(
            cx,
            handle,
            window_flags::MAXIMIZED | window_flags::FOCUSED | window_flags::TRANSPARENT,
            window_flags::MAXIMIZED,
            OutPtr::new(64),
        )
    });
    assert_eq!(status, Ok(Status::Success));
    // Focus cannot be asked for this way, nor transparency after creation.
    let refused = u64::from_le_bytes(rig.read(64, 8).try_into().expect("8 bytes"));
    assert_eq!(refused, window_flags::FOCUSED | window_flags::TRANSPARENT);
    let events = rig.events();
    assert_eq!(events.len(), 1, "{events:?}");
    let Event::WindowFlagsChanged(change) = &events[0].1 else {
        panic!("{events:?}");
    };
    assert_eq!(change.changed, window_flags::MAXIMIZED);
    assert_ne!(change.flags & window_flags::MAXIMIZED, 0);
    assert_ne!(change.flags & window_flags::VISIBLE, 0);
}

#[test]
fn a_destroyed_window_is_gone_for_the_window_system_too() {
    let rig = Rig::new();
    let (handle, user) = rig.window();
    assert!(user.is_alive());
    rig.call(|host, cx| host.window_destroy(cx, handle));
    assert!(!user.is_alive());
    assert!(rig.system.windows().is_empty());
    // What the window system still reports reaches nobody.
    user.key(key(PhysicalKey::KeyA, true));
    user.resize(10, 10);
    assert!(rig.events().is_empty());
    assert_eq!(
        rig.call(|host, cx| host.window_focus(cx, handle)),
        Ok(Status::InvalidHandle)
    );
}

/// Presents `pixels`, laid out as `frame` says, with `damage`.
fn present(
    rig: &Rig,
    window: Handle,
    mut frame: crate::abi::structs::PixelFrame,
    pixels: &[u8],
    damage: &[Rect],
) -> Status {
    const AT: u32 = 4096;
    rig.write(AT, pixels);
    frame.pixels = AT;
    frame.pixels_size = pixels.len() as u32;
    rig.call(|host, cx| host.window_present_pixels(cx, window, &frame, damage))
        .expect("no fault")
}

fn frame(width: u32, height: u32) -> crate::abi::structs::PixelFrame {
    crate::abi::structs::PixelFrame {
        width,
        height,
        stride: 0,
        format: PixelFormat::Bgra8,
        alpha: PixelAlpha::Opaque,
        pixels: 0,
        pixels_size: 0,
    }
}

#[test]
fn a_frame_the_guest_drew_is_what_the_window_shows() {
    let rig = Rig::with(
        HeadlessOptions {
            capture_frames: true,
            ..manual()
        },
        GuiPolicy::default(),
        GuiLimits::default(),
    );
    let (window, user) = rig.window();
    // Two pixels by two, with two bytes of padding after each row: blue,
    // green, red and a grey, as blue, green, red, alpha in memory.
    let rows = [
        [255, 0, 0, 0, 0, 255, 0, 7, 0xAA, 0xAA],
        [0, 0, 255, 9, 10, 20, 30, 40, 0xBB, 0xBB],
    ];
    let pixels: Vec<u8> = rows.concat();
    let padded = crate::abi::structs::PixelFrame {
        stride: 10,
        ..frame(2, 2)
    };
    // The last row's padding need not be there.
    assert_eq!(
        present(&rig, window, padded.clone(), &pixels[..18], &[]),
        Status::Success
    );
    assert_eq!(user.take_requests(), ["pixels 2x2"]);
    assert_eq!(user.frames_presented(), 1);
    let shown = user.last_frame().expect("kept");
    assert_eq!((shown.width, shown.height), (2, 2));
    // Red, green, blue, alpha, and opaque whatever the fourth byte was.
    assert_eq!(
        shown.pixels,
        [
            [0, 0, 255, 255, 0, 255, 0, 255],
            [255, 0, 0, 255, 30, 20, 10, 255]
        ]
        .concat()
    );

    // The same bytes as red, green, blue, alpha, with alpha that counts.
    let straight = crate::abi::structs::PixelFrame {
        format: PixelFormat::Rgba8,
        alpha: PixelAlpha::Straight,
        ..padded
    };
    assert_eq!(
        present(&rig, window, straight, &pixels, &[]),
        Status::Success
    );
    assert_eq!(
        user.last_frame().expect("kept").pixels,
        [[255, 0, 0, 0, 0, 255, 0, 7], [0, 0, 255, 9, 10, 20, 30, 40]].concat()
    );
    // Presenting says nothing to the guest.
    assert!(rig.events().is_empty());
}

#[test]
fn a_frame_that_does_not_add_up_is_refused() {
    let rig = Rig::new();
    let (window, user) = rig.window();
    let pixels = [0_u8; 64];
    let refused = |frame: crate::abi::structs::PixelFrame, bytes: &[u8]| {
        present(&rig, window, frame, bytes, &[])
    };
    assert_eq!(refused(frame(0, 2), &pixels), Status::InvalidArgument);
    assert_eq!(refused(frame(2, 0), &pixels), Status::InvalidArgument);
    // Sixteen bytes are needed, and fifteen are there.
    assert_eq!(refused(frame(2, 2), &pixels[..15]), Status::InvalidArgument);
    // A row cannot be shorter than its pixels.
    let narrow = crate::abi::structs::PixelFrame {
        stride: 7,
        ..frame(2, 2)
    };
    assert_eq!(refused(narrow, &pixels), Status::InvalidArgument);
    assert_eq!(refused(frame(40_000, 1), &pixels), Status::InvalidArgument);
    let null = rig.call(|host, cx| host.window_present_pixels(cx, window, &frame(2, 2), &[]));
    assert_eq!(null, Ok(Status::InvalidArgument));
    assert_eq!(
        rig.call(|host, cx| host.window_present_pixels(cx, 0xDEAD, &frame(2, 2), &[])),
        Ok(Status::InvalidHandle)
    );
    assert_eq!(user.frames_presented(), 0);

    // Bytes the guest says it has and does not have are a fault, as always.
    let beyond = crate::abi::structs::PixelFrame {
        pixels: 65_536 - 8,
        pixels_size: 16,
        ..frame(2, 2)
    };
    assert!(
        rig.call(|host, cx| host.window_present_pixels(cx, window, &beyond, &[]))
            .is_err()
    );
}

#[test]
fn what_changed_is_cut_to_the_frame_and_nothing_changed_is_nothing_to_show() {
    let rig = Rig::new();
    let (window, user) = rig.window();
    let pixels = [0_u8; 4 * 4 * 4];
    let rect = |x, y, width, height| Rect {
        x,
        y,
        width,
        height,
    };
    // One rectangle inside, one hanging over two edges, one outside.
    let damage = [rect(1, 1, 2, 2), rect(-2, 3, 4, 9), rect(10, 10, 5, 5)];
    assert_eq!(
        present(&rig, window, frame(4, 4), &pixels, &damage),
        Status::Success
    );
    assert_eq!(user.frames_presented(), 1);
    // Only rectangles outside the frame: the window is as it was.
    assert_eq!(
        present(&rig, window, frame(4, 4), &pixels, &damage[2..]),
        Status::Success
    );
    assert_eq!(user.frames_presented(), 1);
}

#[test]
fn a_window_asked_to_redraw_is_told_when_the_display_refreshes() {
    let rig = Rig::with(
        HeadlessOptions {
            redraws: crate::headless::HeadlessRedraws::Paced,
            // 50 a second: a frame every 20 ms.
            refresh_rate: 50_000,
            ..manual()
        },
        GuiPolicy::default(),
        GuiLimits::default(),
    );
    let (window, user) = rig.window();
    rig.events();
    let redraws = |rig: &Rig| {
        rig.names()
            .iter()
            .filter(|name| name.as_str() == "WindowRedrawRequested")
            .count()
    };

    // Asked twice before the display refreshes: nothing yet, then once.
    for _ in 0..2 {
        assert_eq!(
            rig.call(|host, cx| host.window_request_redraw(cx, window)),
            Ok(Status::Success)
        );
    }
    assert_eq!(user.take_requests(), ["redraw", "redraw"]);
    assert_eq!(redraws(&rig), 0);
    rig.system.advance(19_000_000);
    assert_eq!(redraws(&rig), 0);
    rig.system.advance(1_000_000);
    assert_eq!(redraws(&rig), 1);

    // A refresh nobody asked for tells nobody.
    rig.system.advance(20_000_000);
    assert_eq!(redraws(&rig), 0);

    // Asked again: the next refresh, however long it has been.
    assert_eq!(
        rig.call(|host, cx| host.window_request_redraw(cx, window)),
        Ok(Status::Success)
    );
    rig.system.advance(100_000_000);
    assert_eq!(redraws(&rig), 1);
}

#[test]
fn a_frame_can_be_looked_at_as_a_picture() {
    let frame = crate::headless::PresentedFrame {
        width: 2,
        height: 1,
        bgra: true,
        // Blue first: a red pixel, then a half transparent green one.
        pixels: vec![0, 0, 255, 255, 0, 255, 0, 128],
    };
    let png = frame.to_png().expect("a picture");
    assert_eq!(
        png[..8],
        [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n']
    );
    // The header: 13 bytes, two pixels by one, eight bits of red, green,
    // blue and alpha.
    assert_eq!(png[8..16], [0, 0, 0, 13, b'I', b'H', b'D', b'R']);
    assert_eq!(png[16..29], [0, 0, 0, 2, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
    // Its checksum, worked out here the slow way.
    fn crc(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    assert_eq!(png[29..33], crc(&png[12..29]).to_be_bytes());
    // The pixels, not compressed: a zlib header, one stored block of nine
    // bytes (the row's filter, then red first), and the sum zlib checks
    // them by.
    let data = &png[33..];
    assert_eq!(data[..8], [0, 0, 0, 20, b'I', b'D', b'A', b'T']);
    assert_eq!(data[8..10], [0x78, 0x01]);
    assert_eq!(data[10..15], [1, 9, 0, 0xF6, 0xFF]);
    let row = [0, 255, 0, 0, 255, 0, 255, 0, 128];
    assert_eq!(data[15..24], row);
    let (mut a, mut b) = (1u32, 0u32);
    for byte in row {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    assert_eq!(data[24..28], ((b << 16) | a).to_be_bytes());
    assert_eq!(data[28..32], crc(&data[4..28]).to_be_bytes());
    assert_eq!(
        data[32..],
        [0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]
    );

    // A frame that was only counted has no pixels to make a picture of.
    let counted = crate::headless::PresentedFrame {
        pixels: Vec::new(),
        ..frame
    };
    assert_eq!(counted.to_png(), None);
}

#[test]
fn a_frame_has_a_most_it_can_weigh() {
    let rig = Rig::with(
        manual(),
        GuiPolicy::default(),
        GuiLimits {
            max_frame_bytes: Some(64),
            ..GuiLimits::default()
        },
    );
    let (window, user) = rig.window();
    let pixels = [0_u8; 4 * 5 * 4];
    assert_eq!(
        present(&rig, window, frame(4, 4), &pixels, &[]),
        Status::Success
    );
    assert_eq!(
        present(&rig, window, frame(4, 5), &pixels, &[]),
        Status::LimitExceeded
    );
    assert_eq!(user.frames_presented(), 1);
}

#[test]
fn whoever_watches_is_told_of_a_window_before_its_guest_is() {
    let rig = Rig::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    {
        let seen = Arc::clone(&seen);
        rig.system.on_window(move |window| {
            // The guest has heard nothing of the window yet: what is said of
            // it here comes first.
            window.present(crate::headless::PresentedFrame {
                width: 1,
                height: 1,
                bgra: false,
                pixels: Vec::new(),
            });
            lock(&seen).push(window.clone());
        });
    }
    let (handle, user) = rig.window();
    let seen = lock(&seen).clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].handle(), handle);
    // A handle outlives its window, and what was drawn on it.
    rig.call(|host, cx| host.window_destroy(cx, handle));
    assert!(!user.is_alive());
    assert_eq!(seen[0].frames_presented(), 1);
}

#[test]
fn destroying_an_instance_destroys_its_windows() {
    let rig = Rig::new();
    let (handle, user) = rig.window();
    let shared = rig.shared();
    rig.host().destroy_instance(rig.instance);
    assert!(!user.is_alive());
    assert!(shared.is_closed());
    assert!(rig.host().window(handle).is_none());
    assert_eq!(rig.host().usage(), (0, 0, 0));
}

#[test]
fn targets_are_adopted_once_and_named_by_their_own_name() {
    let options = HeadlessOptions {
        targets: vec!["canvas".to_owned(), "side".to_owned()],
        top_level: false,
        ..manual()
    };
    let rig = Rig::with(options, GuiPolicy::default(), GuiLimits::default());
    let create = |target: &str| {
        rig.host().create_window(
            rig.instance,
            Some(&WindowDescriptor {
                target: target.to_owned(),
                flags: window_flags::VISIBLE,
                ..WindowDescriptor::default()
            }),
        )
    };
    // The default target is the first.
    let first = create("").expect("the default target");
    let selector = rig
        .host()
        .window(first)
        .expect("alive")
        .selector
        .get()
        .cloned();
    assert_eq!(selector.as_deref(), Some("canvas"));
    assert_eq!(create("canvas"), Err(Status::Busy));
    assert_eq!(create("nowhere"), Err(Status::NotFound));
    let second = create("side").expect("the other target");
    assert_ne!(first, second);
    // Both are taken, and there are no windows beyond targets here.
    assert_eq!(create(""), Err(Status::Busy));
    rig.host().destroy_window(first);
    assert!(create("canvas").is_ok());
}

#[test]
fn windows_of_their_own_get_selectors_nobody_can_guess() {
    let rig = Rig::new();
    let (first, _) = rig.window();
    let (second, _) = rig.window();
    let selector = |handle| {
        rig.host()
            .window(handle)
            .expect("alive")
            .selector
            .get()
            .cloned()
            .expect("set")
    };
    let (a, b) = (selector(first), selector(second));
    assert!(a.starts_with("wgui:") && a.len() == 5 + 32, "{a}");
    assert_ne!(a, b);
}

#[test]
fn the_window_limit_holds() {
    let limits = GuiLimits {
        max_windows: Some(1),
        ..GuiLimits::default()
    };
    let rig = Rig::with(manual(), GuiPolicy::default(), limits);
    let (first, _) = rig.window();
    assert_eq!(
        rig.host().create_window(rig.instance, None),
        Err(Status::LimitExceeded)
    );
    rig.host().destroy_window(first);
    assert!(rig.host().create_window(rig.instance, None).is_ok());
}

// -- the keyboard ----------------------------------------------------------------

#[test]
fn presses_and_releases_pair_up() {
    let rig = Rig::new();
    let (_, user) = rig.window();
    user.focus(true);
    user.key(typed(PhysicalKey::KeyA, "a"));
    user.key(typed(PhysicalKey::KeyA, "a"));
    // A release the window never saw the press of.
    user.key(key(PhysicalKey::KeyB, false));
    user.key(key(PhysicalKey::KeyA, false));
    // And one more release of the same key.
    user.key(key(PhysicalKey::KeyA, false));
    let events = rig.events();
    let keys: Vec<_> = events
        .iter()
        .filter_map(|(_, event)| match event {
            Event::KeyDown(key) => Some(("down", key.clone())),
            Event::KeyUp(key) => Some(("up", key.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(keys.len(), 3, "{events:?}");
    assert_eq!(keys[0].0, "down");
    assert_eq!(keys[0].1.key_flags, 0);
    assert_eq!(keys[0].1.text, "a");
    assert_eq!(keys[0].1.key, u32::from('a'));
    assert_eq!(keys[1].0, "down");
    assert_eq!(keys[1].1.key_flags, key_flags::REPEAT);
    assert_eq!(keys[2].0, "up");
    assert_eq!(keys[2].1.physical_key, PhysicalKey::KeyA);
    assert_eq!(keys[2].1.text, "", "a release types nothing");
}

#[test]
fn modifiers_are_announced_right_after_the_key_that_made_them() {
    let rig = Rig::new();
    let (_, user) = rig.window();
    user.focus(true);
    rig.events();
    let shift = modifiers::SHIFT | modifiers::LEFT_SHIFT;
    user.key(KeyInput {
        modifiers: Some(shift),
        ..key(PhysicalKey::ShiftLeft, true)
    });
    user.key(typed(PhysicalKey::KeyA, "A"));
    user.key(KeyInput {
        modifiers: Some(0),
        ..key(PhysicalKey::ShiftLeft, false)
    });
    let events = rig.events();
    let names: Vec<_> = events.iter().map(|(_, e)| e.ty().name()).collect();
    assert_eq!(
        names,
        [
            "KeyDown",
            "ModifiersChanged",
            "KeyDown",
            "KeyUp",
            "ModifiersChanged"
        ]
    );
    // Each key record carries the modifiers as they are after it.
    let modifiers_of = |index: usize| match &events[index].1 {
        Event::KeyDown(key) | Event::KeyUp(key) => key.modifiers,
        Event::ModifiersChanged(change) => change.modifiers,
        other => panic!("{other:?}"),
    };
    assert_eq!(modifiers_of(0), shift);
    assert_eq!(modifiers_of(1), shift);
    assert_eq!(modifiers_of(2), shift);
    assert_eq!(modifiers_of(3), 0);
    assert_eq!(modifiers_of(4), 0);
}

#[test]
fn losing_focus_releases_what_was_held_first() {
    let rig = Rig::new();
    let (_, user) = rig.window();
    user.focus(true);
    let held = modifiers::CONTROL | modifiers::LEFT_CONTROL | modifiers::CAPS_LOCK;
    user.key(KeyInput {
        modifiers: Some(held),
        ..key(PhysicalKey::ControlLeft, true)
    });
    user.key(key(PhysicalKey::KeyQ, true));
    user.pointer(touch(7, PointerPhase::Down, 5.0, 5.0));
    rig.events();

    user.focus(false);
    let events = rig.events();
    let names: Vec<_> = events.iter().map(|(_, e)| e.ty().name()).collect();
    assert_eq!(
        names,
        [
            "KeyUp",
            "KeyUp",
            "ModifiersChanged",
            "PointerCancel",
            "WindowFocusLost"
        ]
    );
    for (header, _) in &events[..2] {
        assert_eq!(header.flags, event_flags::SYNTHETIC);
    }
    assert_eq!(events[3].0.flags, event_flags::SYNTHETIC);
    // A lock that is on stays on; what was held is not.
    assert_eq!(
        events[2].1,
        Event::ModifiersChanged(ModifiersChangedEvent {
            modifiers: modifiers::CAPS_LOCK
        })
    );
    // The real releases, arriving while the window is elsewhere, are dropped.
    user.key(key(PhysicalKey::KeyQ, false));
    user.key(key(PhysicalKey::ControlLeft, false));
    assert!(rig.events().is_empty());
    // Back in focus, the guest hears of the modifiers still in effect.
    user.focus(true);
    assert_eq!(rig.names(), ["WindowFocusGained", "ModifiersChanged"]);
}

// -- pointers --------------------------------------------------------------------

#[test]
fn motion_nobody_read_adds_up_and_stays_in_order() {
    let rig = Rig::new();
    let (_, user) = rig.window();
    user.pointer(pointer(PointerPhase::Enter, 10.0, 10.0));
    user.pointer(pointer(PointerPhase::Motion, 20.0, 10.0));
    user.pointer(pointer(PointerPhase::Motion, 30.0, 15.0));
    user.pointer(pointer(PointerPhase::Motion, 40.0, 20.0));
    user.pointer(button(PointerPhase::Down, 1, 40.0, 20.0));
    user.pointer(pointer(PointerPhase::Motion, 45.0, 20.0));
    user.pointer(button(PointerPhase::Up, 1, 45.0, 20.0));
    let events = rig.events();
    let names: Vec<_> = events.iter().map(|(_, e)| e.ty().name()).collect();
    assert_eq!(
        names,
        [
            "PointerEnter",
            "PointerMotion",
            "PointerDown",
            "PointerMotion",
            "PointerUp"
        ]
    );
    let record = |index: usize| match &events[index].1 {
        Event::PointerEnter(p)
        | Event::PointerMotion(p)
        | Event::PointerDown(p)
        | Event::PointerUp(p) => p.clone(),
        other => panic!("{other:?}"),
    };
    // Three moves became one: where the cursor ended up, and all the way.
    assert_eq!(events[1].0.flags, event_flags::COALESCED);
    assert_eq!((record(1).x, record(1).y), (40.0, 20.0));
    assert_eq!((record(1).delta_x, record(1).delta_y), (30.0, 10.0));
    assert_eq!(record(1).pointer_id, POINTER_ID_MOUSE);
    assert_eq!(record(1).pointer_flags, pointer_flags::PRIMARY);
    // The press names its button and the buttons held after it.
    assert_eq!(record(2).button, PointerButton::Primary as u32);
    assert_eq!(record(2).buttons, pointer_buttons::PRIMARY);
    assert_eq!(
        record(2).pointer_flags,
        pointer_flags::PRIMARY | pointer_flags::IN_CONTACT
    );
    assert_eq!(record(2).pressure, 0.5);
    // A single move is not marked merged.
    assert_eq!(events[3].0.flags, 0);
    assert_eq!(record(3).delta_x, 5.0);
    assert_eq!(record(4).buttons, 0);
    assert_eq!(record(4).pressure, 0.0);
}

#[test]
fn every_button_goes_down_and_up_on_its_own() {
    let rig = Rig::new();
    let (_, user) = rig.window();
    user.pointer(button(PointerPhase::Down, 1, 1.0, 1.0));
    user.pointer(button(PointerPhase::Down, 2, 1.0, 1.0));
    // Twice the same press, and a release of a button that is not down.
    user.pointer(button(PointerPhase::Down, 2, 1.0, 1.0));
    user.pointer(button(PointerPhase::Up, 3, 1.0, 1.0));
    user.pointer(button(PointerPhase::Up, 1, 1.0, 1.0));
    user.pointer(button(PointerPhase::Up, 2, 1.0, 1.0));
    let held: Vec<_> = rig
        .events()
        .iter()
        .map(|(_, event)| match event {
            Event::PointerDown(p) | Event::PointerUp(p) => (event.ty().name(), p.button, p.buttons),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        held,
        [
            ("PointerDown", 1, 0b01),
            ("PointerDown", 2, 0b11),
            ("PointerUp", 1, 0b10),
            ("PointerUp", 2, 0b00),
        ]
    );
}

#[test]
fn fingers_are_pointers_of_their_own() {
    let rig = Rig::new();
    let (_, user) = rig.window();
    user.pointer(touch(70, PointerPhase::Down, 5.0, 5.0));
    user.pointer(touch(71, PointerPhase::Down, 50.0, 50.0));
    user.pointer(touch(70, PointerPhase::Motion, 6.0, 7.0));
    // A finger nobody saw come down.
    user.pointer(touch(99, PointerPhase::Motion, 1.0, 1.0));
    user.pointer(touch(70, PointerPhase::Up, 6.0, 7.0));
    user.pointer(touch(70, PointerPhase::Motion, 6.0, 8.0));
    let events = rig.events();
    let seen: Vec<_> = events
        .iter()
        .map(|(_, event)| match event {
            Event::PointerDown(p) | Event::PointerUp(p) | Event::PointerMotion(p) => (
                event.ty().name(),
                p.pointer_id,
                p.pointer_flags & pointer_flags::PRIMARY != 0,
            ),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("PointerDown", 2, true),
            ("PointerDown", 3, false),
            ("PointerMotion", 2, true),
            ("PointerUp", 2, true),
        ]
    );
    let Event::PointerDown(first) = &events[0].1 else {
        panic!();
    };
    assert_eq!(first.pointer_kind, PointerKind::Touch);
    assert_eq!(first.button, PointerButton::Primary as u32);
    let Event::PointerMotion(moved) = &events[2].1 else {
        panic!();
    };
    assert_eq!((moved.delta_x, moved.delta_y), (1.0, 2.0));
}

#[test]
fn wheel_notches_add_up_and_gestures_keep_their_ends() {
    let rig = Rig::new();
    let (_, user) = rig.window();
    user.wheel(notch(1.0));
    user.wheel(notch(1.0));
    user.wheel(notch(-0.5));
    user.wheel(WheelInput {
        unit: WheelUnit::Pixel,
        phase: WheelPhase::Began,
        ..notch(0.0)
    });
    user.wheel(WheelInput {
        unit: WheelUnit::Pixel,
        phase: WheelPhase::Changed,
        ..notch(4.0)
    });
    user.wheel(WheelInput {
        unit: WheelUnit::Pixel,
        phase: WheelPhase::Changed,
        ..notch(6.0)
    });
    user.wheel(WheelInput {
        unit: WheelUnit::Pixel,
        phase: WheelPhase::Ended,
        ..notch(0.0)
    });
    let wheels: Vec<_> = rig
        .events()
        .iter()
        .map(|(header, event)| match event {
            Event::Wheel(wheel) => (wheel.unit, wheel.phase, wheel.delta_y, header.flags),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        wheels,
        [
            (
                WheelUnit::Line,
                WheelPhase::None,
                1.5,
                event_flags::COALESCED
            ),
            (WheelUnit::Pixel, WheelPhase::Began, 0.0, 0),
            (
                WheelUnit::Pixel,
                WheelPhase::Changed,
                10.0,
                event_flags::COALESCED
            ),
            (WheelUnit::Pixel, WheelPhase::Ended, 0.0, 0),
        ]
    );
}

// -- text ------------------------------------------------------------------------

fn text_input(rig: &Rig, window: Handle, on: bool) -> Status {
    let descriptor = TextInputDescriptor::default();
    rig.call(|host, cx| host.window_set_text_input(cx, window, on.then_some(&descriptor)))
        .expect("no fault")
}

#[test]
fn while_text_is_entered_what_keys_type_is_also_committed() {
    let rig = Rig::new();
    let (handle, user) = rig.window();
    user.focus(true);
    rig.events();

    // No session: the key says what it types, and that is all.
    user.key(typed(PhysicalKey::KeyA, "a"));
    user.key(key(PhysicalKey::KeyA, false));
    assert_eq!(rig.names(), ["KeyDown", "KeyUp"]);

    assert_eq!(text_input(&rig, handle, true), Status::Success);
    assert_eq!(rig.names(), ["TextInputState"]);
    user.key(typed(PhysicalKey::KeyA, "a"));
    let events = rig.events();
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(
        events[1].1,
        Event::TextCommit(TextCommitEvent {
            text_flags: text_flags::FROM_KEY,
            text: "a".to_owned(),
        })
    );
    // Keys that type nothing a text field would take are not commits.
    user.key(typed(PhysicalKey::Enter, "\r"));
    user.key(KeyInput {
        modifiers: Some(modifiers::CONTROL),
        ..typed(PhysicalKey::KeyC, "c")
    });
    user.key(KeyInput {
        composing: true,
        ..typed(PhysicalKey::KeyN, "n")
    });
    let names = rig.names();
    assert!(!names.iter().any(|name| name == "TextCommit"), "{names:?}");

    assert_eq!(text_input(&rig, handle, false), Status::Success);
    rig.events();
    user.key(typed(PhysicalKey::KeyZ, "z"));
    assert_eq!(rig.names(), ["KeyDown"]);
}

#[test]
fn composition_and_commits_from_an_input_method() {
    let rig = Rig::new();
    let (handle, user) = rig.window();
    text_input(&rig, handle, true);
    rig.events();
    user.report(|sink| sink.text_composition("にほ", Some((6, 6))));
    user.report(|sink| sink.text_composition("日本", None));
    user.report(|sink| sink.text_commit("日本"));
    let events = rig.events();
    assert_eq!(
        events.iter().map(|(_, e)| e.clone()).collect::<Vec<_>>(),
        [
            Event::TextComposition(TextCompositionEvent {
                selection_start: 6,
                selection_end: 6,
                text_flags: 0,
                text: "にほ".to_owned(),
            }),
            Event::TextComposition(TextCompositionEvent {
                selection_start: NO_TEXT_CURSOR,
                selection_end: NO_TEXT_CURSOR,
                text_flags: 0,
                text: "日本".to_owned(),
            }),
            Event::TextCommit(TextCommitEvent {
                text_flags: 0,
                text: "日本".to_owned(),
            }),
        ]
    );
}

#[test]
fn text_too_long_for_a_record_is_split_between_characters() {
    let rig = Rig::new();
    let (_, user) = rig.window();
    // Two bytes each, so that a naive cut would land inside one.
    let text = "é".repeat(9001);
    user.report(|sink| sink.text_commit(&text));
    let events = rig.events();
    assert!(events.len() >= 3, "{}", events.len());
    let mut whole = String::new();
    for (index, (_, event)) in events.iter().enumerate() {
        let Event::TextCommit(commit) = event else {
            panic!("{event:?}");
        };
        let last = index + 1 == events.len();
        assert_eq!(
            commit.text_flags & text_flags::CONTINUES != 0,
            !last,
            "only the last part does not continue"
        );
        whole.push_str(&commit.text);
    }
    assert_eq!(whole, text);
    // And every record fits the size the header promises.
    let bytes = {
        user.report(|sink| sink.text_commit(&text));
        rig.shared().poll(1 << 20)
    };
    let mut at = 0;
    while at < bytes.len() {
        let (_, _, size) = Event::decode(&bytes[at..]).expect("valid");
        assert!(size <= MAX_EVENT_SIZE as usize);
        at += size;
    }
}

// -- requests --------------------------------------------------------------------

#[test]
fn fullscreen_completes_and_reports_what_changed() {
    let rig = Rig::new();
    let (handle, user) = rig.window();
    let options = FullscreenOptions {
        mode: FullscreenMode::Borderless,
        display: 0,
        flags: 0,
    };
    let status = rig.call(|host, cx| host.window_set_fullscreen(cx, handle, Some(&options), 42));
    assert_eq!(status, Ok(Status::Success));
    let events = rig.events();
    assert_eq!(
        events
            .iter()
            .map(|(_, e)| e.ty().name())
            .collect::<Vec<_>>(),
        ["WindowFlagsChanged", "RequestCompleted", "WindowResized"]
    );
    assert_eq!(
        events[1].1,
        Event::RequestCompleted(RequestCompletedEvent {
            tag: 42,
            kind: RequestKind::Fullscreen,
            status: Status::Success,
        })
    );
    assert_eq!(events[1].0.window, handle);
    let Event::WindowResized(resized) = &events[2].1 else {
        panic!();
    };
    assert_eq!((resized.width, resized.height), (1920, 1080));
    let state = rig
        .host()
        .window(handle)
        .expect("alive")
        .shared
        .snapshot
        .read();
    assert_eq!(state.fullscreen, FullscreenMode::Borderless);
    assert_ne!(state.flags & window_flags::FULLSCREEN, 0);

    // The user leaves it without the program asking.
    user.escape();
    let events = rig.events();
    assert_eq!(
        events
            .iter()
            .map(|(_, e)| e.ty().name())
            .collect::<Vec<_>>(),
        ["WindowFlagsChanged", "WindowResized"]
    );
    let Event::WindowResized(resized) = &events[1].1 else {
        panic!();
    };
    assert_eq!((resized.width, resized.height), (800, 600));
}

#[test]
fn a_grab_is_lent_while_the_window_has_focus() {
    let rig = Rig::new();
    let (handle, user) = rig.window();
    user.focus(true);
    rig.events();
    let status =
        rig.call(|host, cx| host.window_set_cursor_grab(cx, handle, CursorGrab::Locked, 7));
    assert_eq!(status, Ok(Status::Success));
    assert_eq!(rig.names(), ["WindowCursorGrabChanged", "RequestCompleted"]);
    // Locked: the cursor stays put and only its movement is reported.
    user.pointer(pointer(PointerPhase::Motion, 100.0, 100.0));
    rig.events();
    user.pointer(PointerInput {
        delta: Some((3.0, -2.0)),
        ..pointer(PointerPhase::Motion, 500.0, 500.0)
    });
    let events = rig.events();
    let Event::PointerMotion(moved) = &events[0].1 else {
        panic!("{events:?}");
    };
    assert_eq!((moved.x, moved.y), (100.0, 100.0));
    assert_eq!((moved.delta_x, moved.delta_y), (3.0, -2.0));

    user.focus(false);
    let events = rig.events();
    assert_eq!(
        events[0].1,
        Event::WindowCursorGrabChanged(WindowCursorGrabChangedEvent {
            grab: CursorGrab::None,
            reason: CursorGrabReason::FocusLost,
        })
    );
    user.focus(true);
    let events = rig.events();
    assert!(
        events.iter().any(|(_, event)| *event
            == Event::WindowCursorGrabChanged(WindowCursorGrabChangedEvent {
                grab: CursorGrab::Locked,
                reason: CursorGrabReason::FocusGained,
            })),
        "{events:?}"
    );
}

/// A window system that answers no request until it is told to.
#[derive(Default)]
struct Holding {
    held: Arc<Mutex<Vec<PendingRequest>>>,
    sinks: Arc<Mutex<Vec<WindowSink>>>,
}

struct HeldWindow(Arc<Mutex<Vec<PendingRequest>>>);

impl HostWindow for HeldWindow {
    fn surface_selector(&self) -> String {
        crate::system::new_surface_selector()
    }

    fn apply(&self, change: WindowChange) {
        if let WindowChange::Fullscreen { request, .. } | WindowChange::CursorGrab { request, .. } =
            change
        {
            // A later request of the same kind replaces the one still open.
            let mut held = lock(&self.0);
            held.retain(|other| other.kind() != request.kind());
            held.push(request);
        }
    }
}

impl WindowSystem for Holding {
    fn info(&self) -> SystemInfo {
        HeadlessSystem::default().info()
    }

    fn now(&self) -> u64 {
        0
    }

    fn displays(&self) -> Vec<crate::system::DisplayData> {
        Vec::new()
    }

    fn create_window(
        &self,
        _attributes: WindowAttributes,
        mut sink: WindowSink,
    ) -> Result<Box<dyn HostWindow>, Status> {
        sink.resized(100, 100, 1.0);
        lock(&self.sinks).push(sink);
        Ok(Box::new(HeldWindow(Arc::clone(&self.held))))
    }
}

fn holding() -> (Rig, Arc<Mutex<Vec<PendingRequest>>>) {
    let backend = Holding::default();
    let held = Arc::clone(&backend.held);
    let rig = Rig::on(
        Arc::new(backend),
        HeadlessSystem::default(),
        GuiPolicy::default(),
        GuiLimits::default(),
    );
    (rig, held)
}

pub(crate) fn completions(rig: &Rig) -> Vec<(u64, Status)> {
    rig.events()
        .into_iter()
        .filter_map(|(_, event)| match event {
            Event::RequestCompleted(done) => Some((done.tag, done.status)),
            _ => None,
        })
        .collect()
}

#[test]
fn every_request_is_answered_exactly_once() {
    let (rig, held) = holding();
    let window = rig
        .host()
        .create_window(rig.instance, None)
        .expect("a window");
    rig.events();
    let grab = |tag| {
        rig.call(|host, cx| host.window_set_cursor_grab(cx, window, CursorGrab::Locked, tag))
            .expect("no fault")
    };
    assert_eq!(grab(1), Status::Success);
    assert!(completions(&rig).is_empty(), "nothing is answered yet");
    // A later request replaces the one still open, which is told so.
    assert_eq!(grab(2), Status::Success);
    assert_eq!(completions(&rig), [(1, Status::Cancelled)]);
    // The window system answers the one it has.
    let request = lock(&held).pop().expect("held");
    request.complete(Status::Denied);
    assert_eq!(completions(&rig), [(2, Status::Denied)]);
    // One still open when its window goes is answered too.
    assert_eq!(grab(3), Status::Success);
    lock(&held).clear();
    assert_eq!(completions(&rig), [(3, Status::Cancelled)]);
    assert!(completions(&rig).is_empty());
}

#[test]
fn a_guest_that_reads_no_answers_is_refused_more_requests() {
    let (rig, _held) = holding();
    let window = rig
        .host()
        .create_window(rig.instance, None)
        .expect("a window");
    rig.events();
    let grab = |tag| {
        rig.call(|host, cx| host.window_set_cursor_grab(cx, window, CursorGrab::Locked, tag))
            .expect("no fault")
    };
    let limit = u64::from(super::shared::MAX_OWED_COMPLETIONS);
    for tag in 0..limit {
        assert_eq!(grab(tag), Status::Success, "request {tag}");
    }
    assert_eq!(grab(limit), Status::LimitExceeded);
    // Each of the refused one's predecessors was answered, in order...
    let answered = completions(&rig);
    assert_eq!(answered.len() as u64, limit - 1);
    assert!(
        answered
            .iter()
            .all(|(_, status)| *status == Status::Cancelled)
    );
    assert!(answered.iter().map(|(tag, _)| *tag).eq(0..limit - 1));
    // ...and having read them, the guest may ask again.
    assert_eq!(grab(limit), Status::Success);
}

// -- loss ------------------------------------------------------------------------

#[test]
fn a_full_queue_tells_what_was_lost_and_puts_the_guest_right() {
    let limits = GuiLimits {
        max_pending_events: Some(16),
        ..GuiLimits::default()
    };
    let rig = Rig::with(manual(), GuiPolicy::default(), limits);
    let (handle, user) = rig.window();
    user.focus(true);
    let shift = modifiers::SHIFT | modifiers::LEFT_SHIFT;
    user.key(KeyInput {
        modifiers: Some(shift),
        ..key(PhysicalKey::ShiftLeft, true)
    });
    user.key(key(PhysicalKey::KeyW, true));
    user.pointer(button(PointerPhase::Down, 1, 9.0, 9.0));
    assert_eq!(
        rig.names(),
        [
            "WindowFocusGained",
            "KeyDown",
            "ModifiersChanged",
            "KeyDown",
            "PointerDown"
        ]
    );

    // The guest stops reading while the user goes on typing.
    for _ in 0..40 {
        user.key(key(PhysicalKey::KeyX, true));
        user.key(key(PhysicalKey::KeyX, false));
    }
    // Among it all the user lets go of everything.
    user.key(key(PhysicalKey::KeyW, false));
    user.pointer(button(PointerPhase::Up, 1, 9.0, 9.0));

    let events = rig.events();
    let lost = events
        .iter()
        .position(|(_, event)| matches!(event, Event::EventsLost(_)))
        .expect("the guest is told");
    assert_eq!(lost, 16, "what was queued before the loss comes first");
    let Event::EventsLost(report) = &events[lost].1 else {
        unreachable!();
    };
    assert!(report.count > 0);
    assert_ne!(report.classes & event_class::KEYBOARD, 0);

    // Then the guest's view is put right: all it believes held is released,
    // and each window says what it is like now.
    let after: Vec<_> = events[lost + 1..]
        .iter()
        .map(|(header, event)| (event.ty().name(), header.flags))
        .collect();
    let synthetic = event_flags::SYNTHETIC;
    let releases = after
        .iter()
        .filter(|(name, flags)| *name == "KeyUp" && *flags == synthetic)
        .count();
    assert!(releases >= 2, "ShiftLeft and KeyW at least: {after:?}");
    assert!(after.contains(&("PointerCancel", synthetic)), "{after:?}");
    assert!(after.contains(&("WindowResized", synthetic)), "{after:?}");
    assert!(
        after.contains(&("WindowFlagsChanged", synthetic)),
        "{after:?}"
    );
    assert!(
        after.contains(&("ModifiersChanged", synthetic)),
        "{after:?}"
    );
    assert!(
        events[lost + 1..]
            .iter()
            .all(|(header, _)| header.window == handle),
        "{events:?}"
    );
    // Sequence numbers count what was delivered, without gaps.
    assert!(
        events
            .windows(2)
            .all(|pair| pair[1].0.sequence == pair[0].0.sequence + 1)
    );

    // From here on things pair up again: a release of what the guest was
    // told is up is not passed on, and a held key starts as a fresh press.
    user.key(KeyInput {
        modifiers: Some(0),
        ..key(PhysicalKey::ShiftLeft, false)
    });
    assert!(
        !rig.names().contains(&"KeyUp".to_owned()),
        "no release without a press"
    );
    user.key(key(PhysicalKey::KeyX, true));
    let events = rig.events();
    let Event::KeyDown(fresh) = &events[0].1 else {
        panic!("{events:?}");
    };
    assert_eq!(fresh.key_flags, 0);
}

#[test]
fn motion_does_not_fill_the_queue() {
    let limits = GuiLimits {
        max_pending_events: Some(16),
        ..GuiLimits::default()
    };
    let rig = Rig::with(manual(), GuiPolicy::default(), limits);
    let (_, user) = rig.window();
    for step in 0..10_000 {
        user.pointer(pointer(PointerPhase::Motion, f64::from(step), 0.0));
    }
    let events = rig.events();
    assert_eq!(events.len(), 1, "one record for all of it");
    let Event::PointerMotion(moved) = &events[0].1 else {
        panic!("{events:?}");
    };
    assert_eq!(moved.x, 9999.0);
    assert_eq!(moved.delta_x, 9999.0);
}

// -- what a guest asked for --------------------------------------------------------

#[test]
fn only_the_classes_asked_for_are_queued() {
    let rig = Rig::new();
    let (_, user) = rig.window();
    let mask = |mask| {
        rig.call(|host, cx| host.instance_set_event_mask(cx, rig.instance, mask))
            .expect("no fault")
    };
    assert_eq!(mask(event_class::WINDOW), Status::Success);
    user.focus(true);
    user.key(key(PhysicalKey::KeyA, true));
    user.pointer(pointer(PointerPhase::Motion, 1.0, 1.0));
    user.wheel(notch(1.0));
    assert_eq!(rig.names(), ["WindowFocusGained"]);
    // Asking for a class does not bring back what was not queued.
    assert_eq!(
        mask(event_class::WINDOW | event_class::KEYBOARD),
        Status::Success
    );
    assert!(rig.events().is_empty());
    user.key(key(PhysicalKey::KeyB, true));
    assert_eq!(rig.names(), ["KeyDown"]);
    // The system class cannot be turned off.
    assert_eq!(mask(0), Status::Success);
    assert_ne!(
        rig.shared().event_mask.load(Ordering::Acquire) & event_class::SYSTEM,
        0
    );
}

#[test]
fn device_input_needs_asking_for_and_a_focused_window() {
    let rig = Rig::new();
    let (_, user) = rig.window();
    let device = rig.system.instances().pop().expect("attached");
    device.raw_motion(0, 1.0, 1.0);
    user.focus(true);
    rig.events();
    device.raw_motion(0, 1.0, 1.0);
    assert!(rig.events().is_empty(), "not asked for");

    rig.call(|host, cx| {
        host.instance_set_event_mask(cx, rig.instance, event_class::DEFAULT | event_class::RAW)
    })
    .expect("no fault");
    device.raw_motion(0, 2.0, 1.0);
    device.raw_motion(0, 3.0, -4.0);
    let events = rig.events();
    assert_eq!(
        events.iter().map(|(_, e)| e.clone()).collect::<Vec<_>>(),
        [Event::RawMotion(RawMotionEvent {
            delta_x: 5.0,
            delta_y: -3.0,
        })]
    );
    user.focus(false);
    rig.events();
    device.raw_motion(0, 9.0, 9.0);
    device.raw_key(0, PhysicalKey::KeyA, 0, true);
    assert!(rig.events().is_empty(), "not while unfocused");
}

// -- the clipboard -----------------------------------------------------------------

const ITEM: u32 = 256;
const DATA: u32 = 512;
const SIZE: u32 = 1024;
const BUFFER: u32 = 2048;

pub(crate) fn copy(rig: &Rig, text: &str, tag: u64) -> Status {
    rig.write(DATA, text.as_bytes());
    let item = ClipboardItem {
        mime_type: "text/plain".to_owned(),
        data: DATA,
        size: text.len() as u32,
    };
    rig.call(|host, cx| {
        host.instance_write_clipboard(
            cx,
            rig.instance,
            ClipboardSelection::Clipboard,
            &[item],
            tag,
        )
    })
    .expect("no fault")
}

pub(crate) fn paste(rig: &Rig, capacity: u32) -> (Status, Option<String>) {
    let _ = ITEM;
    rig.write(SIZE, &u32::MAX.to_le_bytes());
    let status = rig
        .call(|host, cx| {
            host.instance_read_clipboard(
                cx,
                rig.instance,
                ClipboardSelection::Clipboard,
                "text/plain;charset=utf-8",
                BUFFER,
                capacity,
                OutPtr::new(SIZE),
            )
        })
        .expect("no fault");
    let text = (status == Status::Success)
        .then(|| String::from_utf8(rig.read(BUFFER, rig.read_u32(SIZE) as usize)).expect("UTF-8"));
    (status, text)
}

fn paste_chord(user: &HeadlessWindow) {
    user.key(KeyInput {
        modifiers: Some(modifiers::CONTROL),
        ..key(PhysicalKey::ControlLeft, true)
    });
    user.key(key(PhysicalKey::KeyV, true));
    user.key(key(PhysicalKey::KeyV, false));
    user.key(KeyInput {
        modifiers: Some(0),
        ..key(PhysicalKey::ControlLeft, false)
    });
}

#[test]
fn a_guest_reads_back_what_it_copied_at_once() {
    let rig = Rig::new();
    assert_eq!(copy(&rig, "héllo", 5), Status::Success);
    assert_eq!(rig.system.clipboard_text().as_deref(), Some("héllo"));
    assert_eq!(paste(&rig, 64), (Status::Success, Some("héllo".to_owned())));
    let events = rig.events();
    assert_eq!(
        events.iter().map(|(_, e)| e.clone()).collect::<Vec<_>>(),
        [
            Event::RequestCompleted(RequestCompletedEvent {
                tag: 5,
                kind: RequestKind::ClipboardWrite,
                status: Status::Success,
            }),
            Event::ClipboardChanged(ClipboardChangedEvent {
                selection: ClipboardSelection::Clipboard,
                clipboard_flags: clipboard_flags::OWNED | clipboard_flags::HAS_TEXT,
                clipboard_sequence: 1,
            }),
        ]
    );
    // Too small a buffer is told how much is needed, and gets nothing.
    assert_eq!(paste(&rig, 3).0, Status::BufferTooSmall);
    assert_eq!(rig.read_u32(SIZE), "héllo".len() as u32);
}

#[test]
fn what_others_copied_is_read_only_after_the_user_pastes() {
    let rig = Rig::new();
    let (_, user) = rig.window();
    user.focus(true);
    rig.system.set_clipboard_text("from elsewhere");
    let events = rig.events();
    assert!(
        events.iter().any(|(_, event)| *event
            == Event::ClipboardChanged(ClipboardChangedEvent {
                selection: ClipboardSelection::Clipboard,
                clipboard_flags: clipboard_flags::HAS_TEXT,
                clipboard_sequence: 1,
            })),
        "{events:?}"
    );
    assert_eq!(paste(&rig, 64).0, Status::Denied);

    paste_chord(&user);
    assert_eq!(
        paste(&rig, 64),
        (Status::Success, Some("from elsewhere".to_owned()))
    );
    // The permission is for the paste the user made, not for later.
    rig.system.advance(3_000_000_000);
    assert_eq!(paste(&rig, 64).0, Status::Denied);
}

#[test]
fn the_embedder_decides_whether_the_clipboard_can_be_read() {
    let deny = GuiPolicy {
        clipboard_read: ClipboardRead::Deny,
        ..GuiPolicy::default()
    };
    let rig = Rig::with(manual(), deny, GuiLimits::default());
    let (_, user) = rig.window();
    user.focus(true);
    rig.system.set_clipboard_text("secret");
    paste_chord(&user);
    assert_eq!(paste(&rig, 64).0, Status::Denied);
    // Not even that there is text.
    let status = rig
        .call(|host, cx| {
            host.instance_get_clipboard_info(
                cx,
                rig.instance,
                ClipboardSelection::Clipboard,
                OutPtr::new(SIZE),
            )
        })
        .expect("no fault");
    assert_eq!(status, Status::Success);
    assert_eq!(rig.read_u32(SIZE + 8), 0, "no flags");
    // Its own text is still its own.
    assert_eq!(copy(&rig, "mine", 1), Status::Success);
    assert_eq!(paste(&rig, 64), (Status::Success, Some("mine".to_owned())));

    let allow = GuiPolicy {
        clipboard_read: ClipboardRead::Allow,
        ..GuiPolicy::default()
    };
    let rig = Rig::with(manual(), allow, GuiLimits::default());
    rig.system.set_clipboard_text("public");
    assert_eq!(
        paste(&rig, 64),
        (Status::Success, Some("public".to_owned()))
    );
}

#[test]
fn where_reading_has_to_ask_first_a_request_makes_it_answer() {
    let options = HeadlessOptions {
        clipboard_sync: false,
        ..manual()
    };
    let allow = GuiPolicy {
        clipboard_read: ClipboardRead::Allow,
        ..GuiPolicy::default()
    };
    let rig = Rig::with(options, allow, GuiLimits::default());
    assert!(!rig.host().has(FeatureName::ClipboardSyncRead));
    rig.system.set_clipboard_text("later");
    rig.events();
    assert_eq!(paste(&rig, 64).0, Status::Pending);
    let status = rig.call(|host, cx| {
        host.instance_request_clipboard(
            cx,
            rig.instance,
            ClipboardSelection::Clipboard,
            "text/plain",
            9,
        )
    });
    assert_eq!(status, Ok(Status::Success));
    assert_eq!(completions(&rig), [(9, Status::Success)]);
    assert_eq!(paste(&rig, 64), (Status::Success, Some("later".to_owned())));
}

// -- waiting ---------------------------------------------------------------------

#[test]
fn a_wait_ends_for_input_a_wake_or_its_timeout() {
    let rig = Rig::new();
    let (_, user) = rig.window();
    let wait = |timeout: u64| {
        rig.write(SIZE, &u32::MAX.to_le_bytes());
        let status = rig
            .call(|host, cx| {
                host.instance_wait_events(cx, rig.instance, timeout, OutPtr::new(SIZE))
            })
            .expect("no fault");
        assert_eq!(status, Status::Success);
        rig.read_u32(SIZE)
    };
    // Nothing to read: a check says so at once, a short wait times out.
    assert_eq!(wait(0), 0);
    assert_eq!(wait(2_000_000), 0);
    // A wake that came first is not lost, and is used up by one wait.
    rig.call(|host, cx| host.instance_wake(cx, rig.instance))
        .expect("no fault");
    assert_eq!(wait(WAIT_FOREVER), wait_reasons::WOKEN);
    assert_eq!(wait(0), 0);
    // Input ends a wait, whatever kind of input: a merged move as well.
    user.pointer(pointer(PointerPhase::Motion, 1.0, 1.0));
    assert_eq!(wait(WAIT_FOREVER), wait_reasons::EVENTS);
    assert_eq!(wait(WAIT_FOREVER), wait_reasons::EVENTS, "until it is read");
    rig.events();
    // A long wait with nothing to end it hands the thread back after a while.
    assert_eq!(wait(WAIT_FOREVER), wait_reasons::INTERRUPTED);
}

#[test]
fn input_from_another_thread_ends_a_wait_promptly() {
    let rig = Rig::new();
    let (_, user) = rig.window();
    let typist = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(40));
        user.key(key(PhysicalKey::KeyA, true));
    });
    let started = std::time::Instant::now();
    rig.call(|host, cx| {
        host.instance_wait_events(cx, rig.instance, 5_000_000_000, OutPtr::new(SIZE))
    })
    .expect("no fault");
    assert_eq!(rig.read_u32(SIZE), wait_reasons::EVENTS);
    assert!(started.elapsed() < std::time::Duration::from_millis(240));
    typist.join().expect("joins");
}

#[test]
fn a_waiting_guest_is_stopped_when_its_scope_closes() {
    let rig = Rig::new();
    let scope = rig.scope.clone();
    let closer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(30));
        scope.close();
    });
    let host = Arc::clone(rig.host());
    let started = std::time::Instant::now();
    let mut owned = Vec::new();
    let stopped = || host.is_closed();
    let mut cx = Cx {
        mem: GuestMemory::new(rig.memory.view(&rig.store)),
        owned: &mut owned,
        stopped: &stopped,
    };
    let outcome = host.instance_wait_events(&mut cx, rig.instance, WAIT_FOREVER, OutPtr::new(SIZE));
    assert_eq!(outcome, Err(crate::abi::AbiError::Stopped));
    assert!(started.elapsed() < std::time::Duration::from_millis(200));
    closer.join().expect("joins");
}

#[test]
fn one_look_at_a_wait_says_why_it_is_over_or_how_long_to_sleep() {
    use super::instance::{WaitStep, wait_step};
    use crate::abi::AbiError;
    use std::time::Duration;
    let rig = Rig::new();
    let (_, user) = rig.window();
    let shared = rig.shared();
    let running = || false;
    let stopping = || true;
    let now = crate::clock::now;
    // Nothing yet: a check is over at once, and a wait sleeps no longer than
    // it may last, nor than a slice.
    let step = wait_step(&shared, &running, now(), Some(Duration::ZERO));
    assert!(matches!(step, WaitStep::Done(Ok(0))));
    let step = wait_step(&shared, &running, now(), Some(Duration::from_millis(5)));
    assert!(matches!(step, WaitStep::Sleep(slice) if slice <= Duration::from_millis(5)));
    let step = wait_step(&shared, &running, now(), None);
    assert!(matches!(step, WaitStep::Sleep(super::instance::WAIT_SLICE)));
    // Input and a wake both end it and are both said, even to a guest that
    // is being stopped. The wake is used up; the input stays until it is read.
    user.key(key(PhysicalKey::KeyA, true));
    shared.wait.wake();
    let step = wait_step(&shared, &stopping, now(), None);
    assert!(matches!(step, WaitStep::Done(Ok(why))
        if why == wait_reasons::EVENTS | wait_reasons::WOKEN));
    let step = wait_step(&shared, &running, now(), None);
    assert!(matches!(step, WaitStep::Done(Ok(wait_reasons::EVENTS))));
    rig.events();
    // With nothing owed, a guest being stopped is stopped, and one whose
    // instance went is handed back.
    let step = wait_step(&shared, &stopping, now(), None);
    assert!(matches!(step, WaitStep::Done(Err(AbiError::Stopped))));
    rig.call(|host, cx| host.instance_destroy(cx, rig.instance));
    let step = wait_step(&shared, &running, now(), None);
    assert!(matches!(
        step,
        WaitStep::Done(Ok(wait_reasons::INTERRUPTED))
    ));
}

#[test]
fn what_ends_a_wait_changes_the_counter_a_suspended_wait_sleeps_on() {
    // The browser's suspending wait reads the counter, looks, and then sleeps
    // until the counter changes (`web_wait`). Whatever would end the wait
    // after the look has to change it, or the wait sleeps through it.
    use super::instance::{WaitStep, wait_step};
    let rig = Rig::new();
    let (_, user) = rig.window();
    let shared = rig.shared();
    let running = || false;
    let look = || {
        let seen = shared.wait.epoch();
        let step = wait_step(&shared, &running, crate::clock::now(), None);
        (seen, step)
    };
    let ctx = GuiCtx {
        inner: Arc::clone(&rig.host().ctx),
    };
    let external = ctx.event_waker();

    let (seen, step) = look();
    assert!(matches!(step, WaitStep::Sleep(_)));
    std::thread::spawn(move || external())
        .join()
        .expect("joins");
    assert_ne!(shared.wait.epoch(), seen, "a wake from outside");
    assert!(matches!(look().1, WaitStep::Done(Ok(wait_reasons::WOKEN))));

    let (seen, _) = look();
    rig.call(|host, cx| host.instance_wake(cx, rig.instance))
        .expect("no fault");
    assert_ne!(shared.wait.epoch(), seen, "wguiInstanceWake");
    assert!(matches!(look().1, WaitStep::Done(Ok(wait_reasons::WOKEN))));

    let (seen, _) = look();
    user.key(key(PhysicalKey::KeyA, true));
    assert_ne!(shared.wait.epoch(), seen, "input");
    assert!(matches!(look().1, WaitStep::Done(Ok(wait_reasons::EVENTS))));
    rig.events();

    let (seen, step) = look();
    assert!(matches!(step, WaitStep::Sleep(_)));
    rig.call(|host, cx| host.instance_destroy(cx, rig.instance));
    assert_ne!(shared.wait.epoch(), seen, "the instance destroyed");
    assert!(matches!(
        look().1,
        WaitStep::Done(Ok(wait_reasons::INTERRUPTED))
    ));

    let rig = Rig::new();
    let shared = rig.shared();
    let seen = shared.wait.epoch();
    rig.scope.close();
    assert_ne!(shared.wait.epoch(), seen, "the scope closed");
}

#[test]
fn a_kill_shows_on_the_guest_s_memory() {
    // What WASIX does to the memory of a process it kills.
    let mut store = Store::default();
    let memory = Memory::new(&mut store, MemoryType::new(1, Some(1), true)).expect("a shared page");
    let shared = memory.as_shared(&store).expect("shared");
    let probe = shared.ops();
    assert!(!super::killed(&probe));
    assert!(!super::killed(&probe), "asking changes nothing");
    shared.disable_atomics().expect("supported");
    assert!(super::killed(&probe));
}

#[test]
fn external_wake_is_sticky_for_live_scopes_without_synthetic_input() {
    use crate::abi::structs::InstanceDescriptor;
    let ctx = GuiCtx::default();
    let scopes = [ctx.scope(), ctx.scope()];
    let instances: Vec<_> = scopes
        .iter()
        .map(|scope| {
            let handle = scope
                .host()
                .open_instance(&InstanceDescriptor {
                    abi_version: crate::abi::enums::ABI_VERSION,
                    ..Default::default()
                })
                .expect("instance opens");
            scope.host().instance(handle).expect("instance lives")
        })
        .collect();
    let unrelated = GuiCtx::default();
    let foreign = unrelated.scope();
    let handle = foreign
        .host()
        .open_instance(&InstanceDescriptor {
            abi_version: crate::abi::enums::ABI_VERSION,
            ..Default::default()
        })
        .expect("foreign instance opens");
    let foreign_instance = foreign.host().instance(handle).expect("foreign lives");
    let wake = ctx.event_waker();
    std::thread::spawn(move || wake())
        .join()
        .expect("producer finishes");
    for instance in &instances {
        assert!(
            instance.shared.wait.take_wake(),
            "a wake before the wait is kept"
        );
        assert!(!instance.shared.wait.take_wake(), "wake is consumed once");
        assert!(
            !instance.shared.has_events(),
            "wake did not fabricate input"
        );
    }
    assert!(
        !foreign_instance.shared.wait.take_wake(),
        "different contexts remain isolated"
    );
    assert!(scopes.iter().all(|scope| !scope.is_closed()));
    // A closed scope has no instances left to wake.
    scopes[0].close();
    ctx.runtime_control().wake_all();
    assert!(
        !instances[0].shared.wait.take_wake(),
        "closed instance is ignored"
    );
    assert!(
        instances[1].shared.wait.take_wake(),
        "remaining scope still wakes"
    );
}

#[test]
fn an_external_wake_ends_a_wait_at_once() {
    use std::sync::mpsc;
    use std::time::Duration;
    let (setup, set_up) = mpsc::channel();
    let (parking, parked) = mpsc::channel();
    let (done, completion) = mpsc::channel();
    let guest = std::thread::spawn(move || {
        // The rig stays on this thread. The test gets the waker, and what
        // releases a regressed host.
        let rig = Rig::new();
        let wake = GuiCtx {
            inner: Arc::clone(&rig.host().ctx),
        }
        .event_waker();
        setup.send((wake, rig.shared())).expect("the test listens");
        // Each park of this wait lasts a minute rather than WAIT_SLICE, and
        // the host doesn't interrupt the wait before then either: if the wake
        // doesn't unpark the guest, the guest doesn't come round to see it.
        super::instance::STRETCHED_WAIT.set(Some(Duration::from_secs(60)));
        super::wait::PARKING.set(Some(parking));
        let status = rig.call(|host, cx| {
            host.instance_wait_events(cx, rig.instance, WAIT_FOREVER, OutPtr::new(SIZE))
        });
        done.send((status, rig.read_u32(SIZE)))
            .expect("completion receiver");
    });
    let (wake, shared) = set_up.recv().expect("the guest sets up");
    // Woken once it is done looking, when only an unpark ends its park.
    parked
        .recv_timeout(Duration::from_secs(10))
        .expect("the guest parks");
    wake();
    let outcome = completion.recv_timeout(Duration::from_secs(5));
    // Release a regressed host without waiting out its park.
    shared.wait.notify();
    guest.join().expect("joins");
    assert_eq!(
        outcome,
        Ok((Ok(Status::Success), wait_reasons::WOKEN)),
        "the wake left the guest parked"
    );
}

// -- frame pacing --------------------------------------------------------------

/// How a headless display paces a guest that draws every frame, at 60 Hz on
/// a real clock: the distribution of the intervals between its frames, and
/// of how late each refresh came past its time. On macOS it is measured
/// twice: with the strict timer the display thread waits on, and with the
/// plain sleep it falls back on.
///
/// A stand-in guest asks for a redraw, polls until `WindowRedrawRequested`
/// arrives, and takes the record's timestamp, 600 times over. Timestamps
/// and refreshes count from the system's start, so how late a refresh came
/// is exact.
///
/// It prints what it measured and checks nothing of it: a strict timer is
/// a request the system does its best with, and a busy machine is not a
/// failure. Run it on its own, on a machine that is otherwise idle:
///
/// ```text
/// cargo test --release --features cli --lib frame_pacing -- --ignored --nocapture
/// ```
#[test]
#[ignore = "measures for seconds and checks nothing of it: run by hand, as its documentation says"]
fn frame_pacing() {
    #[cfg(target_os = "macos")]
    {
        pace("strict timer");
        crate::headless::SLEEP_FOR_REFRESH.store(true, Ordering::Relaxed);
        pace("thread::sleep");
        crate::headless::SLEEP_FOR_REFRESH.store(false, Ordering::Relaxed);
    }
    #[cfg(not(target_os = "macos"))]
    pace("thread::sleep");
}

/// One run of `frame_pacing`, printed as waiting with `wait`.
fn pace(wait: &str) {
    use std::time::{Duration, Instant};

    const FRAMES: usize = 600;
    /// A frame at 60 Hz, in nanoseconds.
    const INTERVAL: u64 = 1_000_000_000 / 60;
    let rig = Rig::with(
        HeadlessOptions {
            clock: HeadlessClock::Real,
            redraws: crate::headless::HeadlessRedraws::Paced,
            refresh_rate: 60_000,
            ..HeadlessOptions::default()
        },
        GuiPolicy::default(),
        GuiLimits::default(),
    );
    let (window, _user) = rig.window();
    rig.events();
    let mut stamps = Vec::with_capacity(FRAMES + 1);
    while stamps.len() <= FRAMES {
        assert_eq!(
            rig.call(|host, cx| host.window_request_redraw(cx, window)),
            Ok(Status::Success)
        );
        let asked = Instant::now();
        let stamp = loop {
            let redrawn = rig.records().into_iter().find_map(|(header, event)| {
                matches!(event, Event::WindowRedrawRequested).then_some(header.timestamp)
            });
            if let Some(stamp) = redrawn {
                break stamp;
            }
            assert!(asked.elapsed() < Duration::from_secs(10), "no refresh");
            std::thread::yield_now();
        };
        stamps.push(stamp);
    }
    assert_eq!(stamps.len(), FRAMES + 1);

    let ms = |nanos: u64| nanos as f64 / 1e6;
    let mut intervals: Vec<u64> = stamps.windows(2).map(|pair| pair[1] - pair[0]).collect();
    // Past the refresh before it: the one it waited for, unless it came a
    // whole frame late, which shows as a frame skipped.
    let mut late: Vec<u64> = stamps[1..].iter().map(|stamp| stamp % INTERVAL).collect();
    let skipped = intervals
        .iter()
        .filter(|&&interval| interval > INTERVAL * 3 / 2)
        .count();
    intervals.sort_unstable();
    late.sort_unstable();
    let at = |sorted: &[u64], quantile: f64| {
        ms(sorted[((sorted.len() - 1) as f64 * quantile).round() as usize])
    };
    println!(
        "frame_pacing, {wait}: {FRAMES} frames at 60 Hz; \
         interval ms p50 {:.2} p95 {:.2} p99 {:.2} max {:.2}; \
         late ms p50 {:.3} p95 {:.3} max {:.3}; skipped {skipped}",
        at(&intervals, 0.5),
        at(&intervals, 0.95),
        at(&intervals, 0.99),
        at(&intervals, 1.0),
        at(&late, 0.5),
        at(&late, 0.95),
        at(&late, 1.0),
    );
}

/// A headless system that says it tells redraw reasons, for the sink's side
/// of them. The headless and native systems do not.
struct RedrawReasonSystem(HeadlessSystem);

impl WindowSystem for RedrawReasonSystem {
    fn info(&self) -> SystemInfo {
        let mut info = self.0.info();
        info.features.push(FeatureName::RedrawReasons);
        info
    }
    fn now(&self) -> u64 {
        self.0.now()
    }
    fn displays(&self) -> Vec<crate::system::DisplayData> {
        self.0.displays()
    }
    fn attach(&self, instance: crate::system::InstanceSink) {
        self.0.attach(instance);
    }
    fn create_window(
        &self,
        attributes: WindowAttributes,
        sink: WindowSink,
    ) -> Result<Box<dyn HostWindow>, Status> {
        self.0.create_window(attributes, sink)
    }
}

fn redraw_reason_rig() -> Rig {
    let system = HeadlessSystem::new(manual());
    Rig::on(
        Arc::new(RedrawReasonSystem(system.clone())),
        system,
        GuiPolicy::default(),
        GuiLimits::default(),
    )
}

/// A sink of its own for the rig's window, sized, its first redraw (an
/// exposure) read, and nothing queued.
fn redraw_sink(rig: &Rig, window: Handle) -> (Arc<super::Window>, WindowSink) {
    let state = rig.host().window(window).expect("alive");
    let mut sink = WindowSink::new(&state.instance, &state.shared, modifiers::CONTROL);
    sink.resized(640, 480, 1.0);
    sink.redraw_requested();
    rig.events();
    (state, sink)
}

#[test]
fn a_merged_redraw_keeps_its_exposure_whichever_came_first() {
    for exposure_first in [false, true] {
        let rig = redraw_reason_rig();
        let (window, _) = rig.window();
        let (_state, mut sink) = redraw_sink(&rig, window);
        if exposure_first {
            sink.redraw_requested();
            sink.frame_callback_requested(false);
        } else {
            sink.frame_callback_requested(false);
            sink.redraw_requested();
        }
        let events = rig.events();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0].1, Event::WindowRedrawRequested));
        assert_eq!(
            events[0].0.flags,
            event_flags::FRAME_CALLBACK | event_flags::EXPOSURE | event_flags::COALESCED
        );
    }
}

#[test]
fn merged_frame_callbacks_are_not_an_exposure() {
    let rig = redraw_reason_rig();
    let (window, _) = rig.window();
    let (_state, mut sink) = redraw_sink(&rig, window);
    sink.frame_callback_requested(false);
    sink.frame_callback_requested(false);
    let events = rig.events();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].0.flags,
        event_flags::FRAME_CALLBACK | event_flags::COALESCED
    );
    sink.redraw_requested();
    assert_eq!(rig.events()[0].0.flags, event_flags::EXPOSURE);
}

#[test]
fn frame_callbacks_come_only_while_the_window_is_there() {
    let rig = redraw_reason_rig();
    let (window, _) = rig.window();
    let state = rig.host().window(window).expect("alive");
    let mut sink = WindowSink::new(&state.instance, &state.shared, modifiers::CONTROL);
    // Not before the window has a size, as with every other redraw.
    sink.frame_callback_requested(false);
    assert!(rig.events().is_empty());
    sink.resized(640, 480, 1.0);
    rig.events();
    // The window's first redraw: nothing is drawn in it yet.
    sink.frame_callback_requested(false);
    assert_eq!(
        rig.events()[0].0.flags,
        event_flags::FRAME_CALLBACK | event_flags::EXPOSURE
    );
    sink.frame_callback_requested(false);
    assert_eq!(rig.events()[0].0.flags, event_flags::FRAME_CALLBACK);
    state.destroy();
    sink.frame_callback_requested(false);
    sink.redraw_requested();
    assert!(rig.events().is_empty());
}

#[test]
fn a_window_new_or_resized_is_exposed_at_its_next_redraw() {
    let rig = redraw_reason_rig();
    let (window, _) = rig.window();
    let (_state, mut sink) = redraw_sink(&rig, window);
    // A canvas is cleared when its size changes: nothing drawn is left.
    sink.resized(800, 600, 1.0);
    rig.events();
    sink.frame_callback_requested(false);
    assert_eq!(
        rig.events()[0].0.flags,
        event_flags::FRAME_CALLBACK | event_flags::EXPOSURE
    );
    sink.resized(800, 600, 2.0);
    rig.events();
    sink.frame_callback_requested(false);
    assert_eq!(
        rig.events()[0].0.flags,
        event_flags::FRAME_CALLBACK | event_flags::EXPOSURE
    );
    // The same size again changes nothing.
    sink.resized(800, 600, 2.0);
    sink.frame_callback_requested(false);
    assert_eq!(rig.events()[0].0.flags, event_flags::FRAME_CALLBACK);
}

#[test]
fn a_system_that_tells_no_reasons_sends_none() {
    let rig = Rig::new();
    let (window, _) = rig.window();
    let (_state, mut sink) = redraw_sink(&rig, window);
    sink.frame_callback_requested(false);
    assert_eq!(rig.events()[0].0.flags, 0);
    sink.redraw_requested();
    assert_eq!(rig.events()[0].0.flags, 0);
    // Merged, a redraw says so, as other merged events do.
    sink.redraw_requested();
    sink.redraw_requested();
    assert_eq!(rig.events()[0].0.flags, event_flags::COALESCED);
    assert!(!rig.host().features().contains(&FeatureName::RedrawReasons));
}

#[test]
fn a_resize_exposes_the_redraw_already_waiting() {
    // The guest has not taken a frame callback yet when the window changes
    // size: the canvas is cleared, so the redraw it takes is an exposure.
    for (width, height, scale) in [(800, 600, 1.0), (640, 480, 2.0)] {
        let rig = redraw_reason_rig();
        let (window, _) = rig.window();
        let (_state, mut sink) = redraw_sink(&rig, window);
        sink.frame_callback_requested(false);
        sink.resized(width, height, scale);
        let events = rig.events();
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(matches!(events[0].1, Event::WindowResized(_)));
        assert!(matches!(events[1].1, Event::WindowRedrawRequested));
        assert_eq!(
            events[1].0.flags,
            event_flags::FRAME_CALLBACK | event_flags::EXPOSURE
        );
        // Told once: the next one is a frame callback again.
        sink.frame_callback_requested(false);
        assert_eq!(rig.events()[0].0.flags, event_flags::FRAME_CALLBACK);
    }
}

#[test]
fn a_resize_after_the_guest_took_its_redraw_exposes_the_next() {
    let rig = redraw_reason_rig();
    let (window, _) = rig.window();
    let (_state, mut sink) = redraw_sink(&rig, window);
    sink.frame_callback_requested(false);
    assert_eq!(rig.events()[0].0.flags, event_flags::FRAME_CALLBACK);
    sink.resized(800, 600, 1.0);
    rig.events();
    sink.frame_callback_requested(false);
    assert_eq!(
        rig.events()[0].0.flags,
        event_flags::FRAME_CALLBACK | event_flags::EXPOSURE
    );
}

#[test]
fn one_call_tells_both_reasons_in_one_redraw() {
    // What the browser does for a window seen again while the guest's request
    // waits: the guest reads both reasons in the record it is handed.
    let rig = redraw_reason_rig();
    let (window, _) = rig.window();
    let (_state, mut sink) = redraw_sink(&rig, window);
    sink.frame_callback_requested(true);
    let events = rig.events();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].0.flags,
        event_flags::FRAME_CALLBACK | event_flags::EXPOSURE
    );
}

/// The page resizes the window while the guest's poll is between its two
/// looks at the window's mailboxes (`take_slots`), and with `then_key` the user
/// presses a key, which moves the resize to the queue. The guest is handed the
/// older redraw before the resize either way. That redraw is an exposure (the
/// canvas was cleared), and so is the first one at the new size, wherever the
/// resize came from.
fn resize_between_the_guests_two_looks(then_key: bool) {
    use std::sync::Barrier;
    let rig = redraw_reason_rig();
    let (window, _) = rig.window();
    let (state, mut sink) = redraw_sink(&rig, window);
    sink.frame_callback_requested(false);
    let paused = Arc::new(Barrier::new(2));
    let resumed = Arc::new(Barrier::new(2));
    {
        let (paused, resumed) = (Arc::clone(&paused), Arc::clone(&resumed));
        *lock(&state.shared.between_looks) = Some(Box::new(move || {
            paused.wait();
            resumed.wait();
        }));
    }
    let page = std::thread::spawn(move || {
        paused.wait();
        sink.resized(800, 600, 1.0);
        if then_key {
            sink.key(key(PhysicalKey::KeyA, true));
        }
        resumed.wait();
        sink
    });
    // The guest's poll, which pauses between its looks while the page acts.
    let events = rig.events();
    let mut sink = page.join().expect("the page's thread");
    let names: Vec<_> = events.iter().map(|(_, event)| event.ty().name()).collect();
    let expected: &[&str] = if then_key {
        &["WindowRedrawRequested", "WindowResized", "KeyDown"]
    } else {
        &["WindowRedrawRequested", "WindowResized"]
    };
    assert_eq!(names, expected);
    assert_eq!(
        events[0].0.flags,
        event_flags::FRAME_CALLBACK | event_flags::EXPOSURE
    );
    // The first frame at the new size.
    sink.frame_callback_requested(false);
    assert_eq!(
        rig.events()[0].0.flags,
        event_flags::FRAME_CALLBACK | event_flags::EXPOSURE
    );
    sink.frame_callback_requested(false);
    assert_eq!(rig.events()[0].0.flags, event_flags::FRAME_CALLBACK);
}

#[test]
fn a_resize_between_the_guests_two_looks_exposes_the_frame_after_it() {
    resize_between_the_guests_two_looks(false);
}

#[test]
fn a_resize_flushed_to_the_queue_by_input_still_exposes_the_frame_after_it() {
    resize_between_the_guests_two_looks(true);
}
