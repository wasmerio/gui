//! The window system that is always there: windows nobody sees.
//!
//! A guest of a [`HeadlessSystem`] opens windows, sizes them, goes
//! fullscreen and grabs the cursor, and all of it works, in a model with no
//! screen behind it. Input comes from whoever holds the system: a test that
//! scripts it, a server that forwards a remote user's, a tool that replays a
//! recording.
//!
//! It is what a [`GuiCtx`](crate::GuiCtx) uses when it is given nothing
//! else, and what the tests of this crate run on, which makes it the
//! reference for how events are ordered.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use crate::system::*;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// How time passes for a headless system.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HeadlessClock {
    /// As it does: event timestamps are real elapsed time.
    #[default]
    Real,
    /// Only when [`HeadlessSystem::advance`] says so, for runs that must be
    /// the same every time.
    Manual,
}

/// When a headless window that was asked to redraw says that it can be.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HeadlessRedraws {
    /// At once. A guest that draws whenever it is asked runs as fast as it
    /// can.
    #[default]
    Immediate,
    /// When the display next refreshes, as a real one does: at
    /// [`HeadlessOptions::refresh_rate`] on the system's clock. On a manual
    /// clock that is when [`HeadlessSystem::advance`] passes a refresh, so a
    /// guest can be walked through its frames one at a time.
    Paced,
}

/// What a headless system is like.
#[derive(Clone, Debug)]
pub struct HeadlessOptions {
    /// The size of a window that asks for none, in physical pixels.
    pub size: (u32, u32),
    pub scale_factor: f64,
    /// The size of the one display.
    pub display: (u32, u32),
    /// Its refresh rate, in thousandths of a hertz.
    pub refresh_rate: u32,
    /// The targets an embedder would provide (canvases, views), by name.
    /// The first is the default target. A window can also be opened without
    /// one, unless [`HeadlessOptions::top_level`] is off.
    pub targets: Vec<String>,
    /// Whether windows can be opened beyond the targets.
    pub top_level: bool,
    pub theme: Theme,
    /// The modifier of the platform's shortcuts.
    pub shortcut_modifier: u32,
    /// Whether the clipboard can be read without asking first. Off behaves
    /// as a browser does: a read answers `Pending` until a request is made.
    pub clipboard_sync: bool,
    pub clock: HeadlessClock,
    pub redraws: HeadlessRedraws,
    /// Whether the frames guests present to windows are kept, for
    /// [`HeadlessWindow::last_frame`]. Reading a frame back from the GPU
    /// costs a copy, so they are only counted unless this is on.
    pub capture_frames: bool,
}

impl Default for HeadlessOptions {
    fn default() -> Self {
        Self {
            size: (800, 600),
            scale_factor: 1.0,
            display: (1920, 1080),
            refresh_rate: 60_000,
            targets: Vec::new(),
            top_level: true,
            theme: Theme::Light,
            shortcut_modifier: modifiers::CONTROL,
            clipboard_sync: true,
            clock: HeadlessClock::Real,
            redraws: HeadlessRedraws::Immediate,
            capture_frames: false,
        }
    }
}

/// The id of the one display.
const DISPLAY: u64 = 1;

/// Makes the display threads started from now on wait for each refresh with
/// a plain sleep, as where the strict timer cannot be made: for the
/// `frame_pacing` measurement's comparison.
#[cfg(all(test, target_os = "macos"))]
pub(crate) static SLEEP_FOR_REFRESH: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
struct Clipboard {
    items: Vec<ClipboardData>,
    sequence: u32,
    /// The content has been fetched since it last changed, so a read answers.
    fetched: bool,
}

#[derive(Default)]
struct State {
    windows: Vec<Arc<WindowState>>,
    instances: Vec<InstanceSink>,
    clipboard: Clipboard,
    primary: Clipboard,
}

impl State {
    fn clipboard(&mut self, selection: ClipboardSelection) -> &mut Clipboard {
        match selection {
            ClipboardSelection::Clipboard => &mut self.clipboard,
            ClipboardSelection::Primary => &mut self.primary,
        }
    }
}

struct Inner {
    options: HeadlessOptions,
    started: Instant,
    /// Nanoseconds, for the manual clock.
    time: AtomicU64,
    next_window: AtomicU64,
    state: Mutex<State>,
    /// Told of every window a guest opens.
    watchers: Mutex<Vec<WindowWatcher>>,
    /// Whether a window is waiting for the display to refresh, for the
    /// thread that plays the display on a real clock.
    redraw_asked: Mutex<bool>,
    redraw_signal: Condvar,
    ticking: AtomicBool,
}

impl Inner {
    /// How long the display takes for a frame, in nanoseconds.
    fn frame_interval(&self) -> u64 {
        // The rate is in thousandths of a hertz.
        1_000_000_000_000 / u64::from(self.options.refresh_rate.max(1))
    }

    /// The display refreshed: the windows that were asked to redraw can be.
    fn refresh(&self) {
        let windows = lock(&self.state).windows.clone();
        for window in windows {
            if window.redraw_wanted.swap(false, Ordering::AcqRel) {
                lock(&window.sink).redraw_requested();
            }
        }
    }

    /// A window waits for the next refresh. On a real clock a thread plays
    /// the display; on a manual one, whoever advances it does.
    fn redraw_wanted(self: &Arc<Self>) {
        if self.options.clock != HeadlessClock::Real {
            return;
        }
        if !self.ticking.swap(true, Ordering::AcqRel) {
            let system = Arc::downgrade(self);
            let spawned = std::thread::Builder::new()
                .name("wasmer-gui-headless-display".to_owned())
                .spawn(move || Inner::tick(system));
            if spawned.is_err() {
                // No thread to wait on: the window is told at once.
                self.ticking.store(false, Ordering::Release);
                self.refresh();
                return;
            }
        }
        *lock(&self.redraw_asked) = true;
        self.redraw_signal.notify_one();
    }

    fn tick(system: Weak<Inner>) {
        // macOS can wake a thread that sleeps milliseconds late: there a
        // strict timer ends the waits for the display.
        #[cfg(target_os = "macos")]
        let timer = crate::macos_deadline::Deadline::new({
            let thread = std::thread::current();
            move || thread.unpark()
        });
        #[cfg(all(test, target_os = "macos"))]
        let timer = timer.filter(|_| !SLEEP_FOR_REFRESH.load(Ordering::Relaxed));
        loop {
            let Some(inner) = system.upgrade() else {
                return;
            };
            {
                let mut asked = lock(&inner.redraw_asked);
                while !*asked {
                    // Held by nobody else: the system and its guests are gone.
                    if Arc::strong_count(&inner) == 1 {
                        return;
                    }
                    asked = inner
                        .redraw_signal
                        .wait_timeout(asked, Duration::from_millis(250))
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .0;
                }
                *asked = false;
            }
            let interval = inner.frame_interval();
            let elapsed = inner.started.elapsed().as_nanos() as u64;
            let delay = Duration::from_nanos(interval - elapsed % interval);
            // The refresh, on the system's clock.
            #[cfg(target_os = "macos")]
            let refresh = inner.started + Duration::from_nanos(elapsed) + delay;
            drop(inner);
            #[cfg(target_os = "macos")]
            if let Some(timer) = &timer {
                timer.wait_until(refresh);
            } else {
                std::thread::sleep(refresh.saturating_duration_since(Instant::now()));
            }
            #[cfg(not(target_os = "macos"))]
            std::thread::sleep(delay);
            if let Some(inner) = system.upgrade() {
                inner.refresh();
            }
        }
    }
}

type WindowWatcher = Arc<dyn Fn(&HeadlessWindow) + Send + Sync>;

/// What a headless window is like right now.
#[derive(Clone, Debug, PartialEq)]
pub struct HeadlessWindowModel {
    pub title: String,
    pub size: (u32, u32),
    pub min_size: (u32, u32),
    pub max_size: (u32, u32),
    /// The requestable [`window_flags`] in effect.
    pub flags: u64,
    pub fullscreen: FullscreenMode,
    pub focused: bool,
    pub cursor_shape: CursorShape,
    pub custom_cursor: bool,
    pub cursor_visible: bool,
    pub cursor_grab: CursorGrab,
    pub text_input: Option<TextInput>,
    /// The [`default_actions`] the guest wants prevented.
    pub default_actions: u32,
    /// The size to return to when fullscreen ends.
    restore: (u32, u32),
}

/// A frame a guest presented to a headless window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresentedFrame {
    pub width: u32,
    pub height: u32,
    /// Blue comes first in each pixel, not red.
    pub bgra: bool,
    /// Four bytes a pixel, rows from the top. Empty unless the system
    /// captures frames.
    pub pixels: Vec<u8>,
}

impl PresentedFrame {
    /// The frame as a PNG file, for looking at what a guest drew. Not
    /// compressed: what is wanted here is a file every viewer opens, made
    /// without one more dependency. `None` for a frame that was not
    /// captured.
    pub fn to_png(&self) -> Option<Vec<u8>> {
        let row = self.width as usize * 4;
        if row == 0 || self.pixels.len() != row * self.height as usize {
            return None;
        }
        // What a PNG holds: each row behind a byte that says how it is
        // filtered (0: not at all), red first, alpha not multiplied in.
        let mut data = Vec::with_capacity(self.pixels.len() + self.height as usize);
        for line in self.pixels.chunks_exact(row) {
            data.push(0);
            if self.bgra {
                for pixel in line.as_chunks::<4>().0 {
                    data.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
                }
            } else {
                data.extend_from_slice(line);
            }
        }

        // The same bytes as a zlib stream of stored blocks.
        let mut stream = vec![0x78, 0x01];
        let mut blocks = data.chunks(0xFFFF).peekable();
        while let Some(block) = blocks.next() {
            let length = block.len() as u16;
            stream.push(u8::from(blocks.peek().is_none()));
            stream.extend_from_slice(&length.to_le_bytes());
            stream.extend_from_slice(&(!length).to_le_bytes());
            stream.extend_from_slice(block);
        }
        let (mut a, mut b) = (1u32, 0u32);
        for chunk in data.chunks(5552) {
            for byte in chunk {
                a += u32::from(*byte);
                b += a;
            }
            a %= 65521;
            b %= 65521;
        }
        stream.extend_from_slice(&((b << 16) | a).to_be_bytes());

        let mut table = [0u32; 256];
        for (index, entry) in table.iter_mut().enumerate() {
            let mut value = index as u32;
            for _ in 0..8 {
                value = if value & 1 != 0 {
                    0xEDB8_8320 ^ (value >> 1)
                } else {
                    value >> 1
                };
            }
            *entry = value;
        }
        let mut png = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'];
        let mut chunk = |kind: &[u8; 4], body: &[u8]| {
            png.extend_from_slice(&(body.len() as u32).to_be_bytes());
            let mut crc = !0u32;
            for byte in kind.iter().chain(body) {
                crc = table[((crc ^ u32::from(*byte)) & 0xFF) as usize] ^ (crc >> 8);
            }
            png.extend_from_slice(kind);
            png.extend_from_slice(body);
            png.extend_from_slice(&(!crc).to_be_bytes());
        };
        let mut header = Vec::with_capacity(13);
        header.extend_from_slice(&self.width.to_be_bytes());
        header.extend_from_slice(&self.height.to_be_bytes());
        // 8 bits a channel, red green blue alpha, no interlacing.
        header.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(b"IHDR", &header);
        chunk(b"IDAT", &stream);
        chunk(b"IEND", &[]);
        Some(png)
    }
}

#[derive(Default)]
struct Frames {
    presented: u64,
    last: Option<PresentedFrame>,
}

struct WindowState {
    id: u64,
    target: String,
    /// The name a surface for the window is created with.
    selector: String,
    system: Weak<Inner>,
    alive: AtomicBool,
    sink: Mutex<WindowSink>,
    model: Mutex<HeadlessWindowModel>,
    /// What the guest asked of the window, in words, oldest first.
    requests: Mutex<Vec<String>>,
    /// What the guest drew on it.
    frames: Mutex<Frames>,
    /// Asked to redraw, and waiting for the display to refresh.
    redraw_wanted: AtomicBool,
}

impl WindowState {
    fn note(&self, request: String) {
        lock(&self.requests).push(request);
    }

    /// Makes this window the one with focus.
    fn take_focus(self: &Arc<Self>) {
        let others: Vec<Arc<WindowState>> = match self.system.upgrade() {
            Some(system) => lock(&system.state).windows.clone(),
            None => Vec::new(),
        };
        for other in others {
            if !Arc::ptr_eq(&other, self) {
                other.set_focus(false);
            }
        }
        self.set_focus(true);
    }

    fn set_focus(&self, focused: bool) {
        let mut model = lock(&self.model);
        if model.focused == focused {
            return;
        }
        model.focused = focused;
        // A grab is lent while the window has focus.
        let grab = model.cursor_grab;
        drop(model);
        let mut sink = lock(&self.sink);
        if !focused && grab != CursorGrab::None {
            sink.cursor_grab_changed(CursorGrab::None, CursorGrabReason::FocusLost);
        }
        sink.focused(focused);
        if focused && grab != CursorGrab::None {
            sink.cursor_grab_changed(grab, CursorGrabReason::FocusGained);
        }
    }

    fn resize(&self, width: u32, height: u32) {
        let clamp = |value: u32, min: u32, max: u32| {
            let value = value.max(min).max(1);
            if max != 0 {
                value.min(max.max(min))
            } else {
                value
            }
        };
        let (size, scale_factor) = {
            let mut model = lock(&self.model);
            model.size = (
                clamp(width, model.min_size.0, model.max_size.0),
                clamp(height, model.min_size.1, model.max_size.1),
            );
            let scale_factor = self
                .system
                .upgrade()
                .map_or(1.0, |system| system.options.scale_factor);
            (model.size, scale_factor)
        };
        lock(&self.sink).resized(size.0, size.1, scale_factor);
    }
}

struct Window(Arc<WindowState>);

impl HostWindow for Window {
    fn apply(&self, change: WindowChange) {
        let window = &self.0;
        match change {
            WindowChange::Title(title) => {
                window.note(format!("title {title:?}"));
                lock(&window.model).title = title;
            }
            WindowChange::Size { width, height } => {
                window.note(format!("size {width}x{height}"));
                if lock(&window.model).fullscreen == FullscreenMode::None {
                    window.resize(width, height);
                }
            }
            WindowChange::SizeLimits { min, max } => {
                window.note(format!(
                    "size-limits {}x{} to {}x{}",
                    min.0, min.1, max.0, max.1
                ));
                let size = {
                    let mut model = lock(&window.model);
                    model.min_size = min;
                    model.max_size = max;
                    model.size
                };
                window.resize(size.0, size.1);
            }
            WindowChange::Position { x, y } => {
                window.note(format!("position {x},{y}"));
                lock(&window.sink).moved(x, y);
            }
            WindowChange::Flags { mask, values } => {
                window.note(format!("flags {values:#x} of {mask:#x}"));
                {
                    let mut model = lock(&window.model);
                    model.flags = (model.flags & !mask) | (values & mask);
                }
                lock(&window.sink).flags(mask, values);
            }
            WindowChange::Fullscreen { options, request } => {
                let mode = options.as_ref().map_or(FullscreenMode::None, |o| o.mode);
                window.note(format!("fullscreen {mode:?}"));
                let display = window
                    .system
                    .upgrade()
                    .map_or((0, 0), |system| system.options.display);
                let size = {
                    let mut model = lock(&window.model);
                    if model.fullscreen == FullscreenMode::None {
                        model.restore = model.size;
                    }
                    model.fullscreen = mode;
                    if mode == FullscreenMode::None {
                        model.restore
                    } else {
                        display
                    }
                };
                lock(&window.sink).fullscreen(mode);
                {
                    let scale_factor = window
                        .system
                        .upgrade()
                        .map_or(1.0, |system| system.options.scale_factor);
                    lock(&window.model).size = size;
                    lock(&window.sink).resized(size.0, size.1, scale_factor);
                }
                request.complete(Status::Success);
            }
            WindowChange::Focus => {
                window.note("focus".to_owned());
                window.take_focus();
            }
            WindowChange::Attention(level) => window.note(format!("attention {level:?}")),
            WindowChange::Redraw => {
                window.note("redraw".to_owned());
                match window.system.upgrade() {
                    Some(system) if system.options.redraws == HeadlessRedraws::Paced => {
                        window.redraw_wanted.store(true, Ordering::Release);
                        system.redraw_wanted();
                    }
                    _ => lock(&window.sink).redraw_requested(),
                }
            }
            WindowChange::CursorShape(shape) => {
                window.note(format!("cursor-shape {shape:?}"));
                let mut model = lock(&window.model);
                model.cursor_shape = shape;
                model.custom_cursor = false;
            }
            WindowChange::CustomCursor(image) => {
                window.note(match &image {
                    Some(image) => format!(
                        "custom-cursor {}x{} hotspot {},{}",
                        image.width, image.height, image.hotspot_x, image.hotspot_y
                    ),
                    None => "custom-cursor none".to_owned(),
                });
                lock(&window.model).custom_cursor = image.is_some();
            }
            WindowChange::CursorVisible(visible) => {
                window.note(format!("cursor-visible {visible}"));
                lock(&window.model).cursor_visible = visible;
            }
            WindowChange::CursorGrab { grab, request } => {
                window.note(format!("cursor-grab {grab:?}"));
                let focused = {
                    let mut model = lock(&window.model);
                    model.cursor_grab = grab;
                    model.focused
                };
                // Granted at once; in effect while the window has focus.
                if focused || grab == CursorGrab::None {
                    lock(&window.sink).cursor_grab_changed(grab, CursorGrabReason::Requested);
                }
                request.complete(Status::Success);
            }
            WindowChange::CursorPosition { x, y } => {
                window.note(format!("cursor-position {x},{y}"));
                lock(&window.sink).pointer(PointerInput {
                    x,
                    y,
                    ..PointerInput::default()
                });
            }
            WindowChange::TextInput(text_input) => {
                window.note(match &text_input {
                    Some(text_input) => format!(
                        "text-input {:?} at {},{} {}x{}",
                        text_input.purpose,
                        text_input.cursor_area.x,
                        text_input.cursor_area.y,
                        text_input.cursor_area.width,
                        text_input.cursor_area.height
                    ),
                    None => "text-input off".to_owned(),
                });
                let was = {
                    let mut model = lock(&window.model);
                    let was = model.text_input.is_some();
                    model.text_input = text_input.clone();
                    was
                };
                if was != text_input.is_some() {
                    lock(&window.sink).text_input_state(
                        text_input.is_some(),
                        false,
                        Rect::default(),
                    );
                }
            }
            WindowChange::ResetTextInput => {
                window.note("reset-text-input".to_owned());
                lock(&window.sink).text_composition("", None);
            }
            WindowChange::DefaultActions(prevented) => {
                window.note(format!("default-actions {prevented:#x}"));
                lock(&window.model).default_actions = prevented;
            }
            WindowChange::Pixels(frame) => {
                window.note(format!("pixels {}x{}", frame.width, frame.height));
                let capture = window
                    .system
                    .upgrade()
                    .is_some_and(|system| system.options.capture_frames);
                let mut frames = lock(&window.frames);
                frames.presented += 1;
                frames.last = Some(PresentedFrame {
                    width: frame.width,
                    height: frame.height,
                    bgra: false,
                    pixels: if capture { frame.to_rgba() } else { Vec::new() },
                });
            }
        }
    }

    fn surface_selector(&self) -> String {
        self.0.selector.clone()
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        self.0.alive.store(false, Ordering::Release);
        if let Some(system) = self.0.system.upgrade() {
            lock(&system.state)
                .windows
                .retain(|window| !Arc::ptr_eq(window, &self.0));
        }
    }
}

/// A window of a [`HeadlessSystem`], for whoever plays the user.
#[derive(Clone)]
pub struct HeadlessWindow {
    state: Arc<WindowState>,
}

impl HeadlessWindow {
    /// A number that tells the system's windows apart, in creation order.
    pub fn id(&self) -> u64 {
        self.state.id
    }

    /// The target the window adopted, or the empty string.
    pub fn target(&self) -> &str {
        &self.state.target
    }

    /// The name a surface for the window is created with.
    pub fn surface_selector(&self) -> &str {
        &self.state.selector
    }

    /// The guest's handle for the window.
    pub fn handle(&self) -> u32 {
        lock(&self.state.sink).handle()
    }

    /// Whether the guest still has the window.
    pub fn is_alive(&self) -> bool {
        self.state.alive.load(Ordering::Acquire)
    }

    pub fn model(&self) -> HeadlessWindowModel {
        lock(&self.state.model).clone()
    }

    /// What the guest has asked of the window since this was last called, in
    /// words: `title "Hello"`, `fullscreen Borderless`, `cursor-grab Locked`.
    pub fn take_requests(&self) -> Vec<String> {
        std::mem::take(&mut lock(&self.state.requests))
    }

    /// How many frames the guest has presented to the window.
    pub fn frames_presented(&self) -> u64 {
        lock(&self.state.frames).presented
    }

    /// The frame the guest presented last, if the system captures frames.
    pub fn last_frame(&self) -> Option<PresentedFrame> {
        lock(&self.state.frames).last.clone()
    }

    /// A guest presented a frame. For whatever draws on the window.
    pub fn present(&self, frame: PresentedFrame) {
        let mut frames = lock(&self.state.frames);
        frames.presented += 1;
        frames.last = Some(frame);
    }

    /// Reports anything a window system can, in its own words.
    pub fn report<R>(&self, report: impl FnOnce(&mut WindowSink) -> R) -> R {
        report(&mut lock(&self.state.sink))
    }

    /// Gives or takes keyboard focus. Giving it takes it from the others.
    pub fn focus(&self, focused: bool) {
        if focused {
            self.state.take_focus();
        } else {
            self.state.set_focus(false);
        }
    }

    /// The user resized the window.
    pub fn resize(&self, width: u32, height: u32) {
        self.state.resize(width, height);
    }

    /// The user asked to close the window.
    pub fn close(&self) {
        self.report(WindowSink::close_requested);
    }

    pub fn key(&self, input: KeyInput) {
        self.report(|sink| sink.key(input));
    }

    pub fn pointer(&self, input: PointerInput) {
        self.report(|sink| sink.pointer(input));
    }

    pub fn wheel(&self, input: WheelInput) {
        self.report(|sink| sink.wheel(input));
    }

    /// The user ended fullscreen or a cursor grab themselves, as Escape does
    /// in a browser.
    pub fn escape(&self) {
        let (fullscreen, grab, restore) = {
            let mut model = lock(&self.state.model);
            let was = (model.fullscreen, model.cursor_grab, model.restore);
            model.fullscreen = FullscreenMode::None;
            model.cursor_grab = CursorGrab::None;
            was
        };
        if grab != CursorGrab::None {
            self.report(|sink| sink.cursor_grab_changed(CursorGrab::None, CursorGrabReason::User));
        }
        if fullscreen != FullscreenMode::None {
            self.report(|sink| sink.fullscreen(FullscreenMode::None));
            self.state.resize(restore.0, restore.1);
        }
    }
}

/// A window system with no screen. Cloning shares it.
#[derive(Clone)]
pub struct HeadlessSystem {
    inner: Arc<Inner>,
}

impl Default for HeadlessSystem {
    fn default() -> Self {
        Self::new(HeadlessOptions::default())
    }
}

impl HeadlessSystem {
    pub fn new(options: HeadlessOptions) -> Self {
        Self {
            inner: Arc::new(Inner {
                options,
                started: Instant::now(),
                time: AtomicU64::new(0),
                next_window: AtomicU64::new(1),
                state: Mutex::new(State::default()),
                watchers: Mutex::new(Vec::new()),
                redraw_asked: Mutex::new(false),
                redraw_signal: Condvar::new(),
                ticking: AtomicBool::new(false),
            }),
        }
    }

    pub fn options(&self) -> &HeadlessOptions {
        &self.inner.options
    }

    /// Lets time pass on a manual clock. Where redraws are paced, passing a
    /// refresh of the display tells the windows that were asked to redraw.
    pub fn advance(&self, nanoseconds: u64) {
        let before = self.inner.time.fetch_add(nanoseconds, Ordering::AcqRel);
        let interval = self.inner.frame_interval();
        if self.inner.options.redraws == HeadlessRedraws::Paced
            && self.inner.options.clock == HeadlessClock::Manual
            && before.saturating_add(nanoseconds) / interval != before / interval
        {
            self.inner.refresh();
        }
    }

    /// How long the display takes for a frame.
    pub fn frame_interval(&self) -> Duration {
        Duration::from_nanos(self.inner.frame_interval())
    }

    /// Calls `watcher` with every window a guest opens from now on, on the
    /// guest's thread and before the guest hears of the window. A window's
    /// handle outlives the window, so this is where to take hold of one whose
    /// frames are wanted, however short its life.
    pub fn on_window(&self, watcher: impl Fn(&HeadlessWindow) + Send + Sync + 'static) {
        lock(&self.inner.watchers).push(Arc::new(watcher));
    }

    /// The windows guests have open, oldest first.
    pub fn windows(&self) -> Vec<HeadlessWindow> {
        lock(&self.inner.state)
            .windows
            .iter()
            .map(|state| HeadlessWindow {
                state: Arc::clone(state),
            })
            .collect()
    }

    /// The window a surface selector names, if a guest has it open.
    pub fn window_for_selector(&self, selector: &str) -> Option<HeadlessWindow> {
        self.windows()
            .into_iter()
            .find(|window| window.surface_selector() == selector)
    }

    /// The guests connected right now.
    pub fn instances(&self) -> Vec<InstanceSink> {
        let mut state = lock(&self.inner.state);
        state.instances.retain(|instance| !instance.is_closed());
        state.instances.clone()
    }

    /// Whether every connected guest has read all its input and is waiting
    /// for more, and there is at least one. Feeding a guest its next input
    /// only then makes a run repeatable.
    pub fn is_idle(&self) -> bool {
        let instances = self.instances();
        !instances.is_empty() && instances.iter().all(InstanceSink::is_idle)
    }

    /// Waits for [`HeadlessSystem::is_idle`]. `false` if it did not come.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let started = Instant::now();
        loop {
            if self.is_idle() {
                return true;
            }
            if started.elapsed() >= timeout {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// The text on the clipboard, whoever put it there.
    pub fn clipboard_text(&self) -> Option<String> {
        lock(&self.inner.state)
            .clipboard
            .items
            .iter()
            .find(|item| item.mime_type == "text/plain")
            .map(|item| String::from_utf8_lossy(&item.data).into_owned())
    }

    /// Another program copied this text.
    pub fn set_clipboard_text(&self, text: &str) {
        let (state, instances) = {
            let mut state = lock(&self.inner.state);
            let clipboard = &mut state.clipboard;
            clipboard.items = vec![ClipboardData {
                mime_type: "text/plain".to_owned(),
                data: text.as_bytes().to_vec(),
            }];
            clipboard.sequence = clipboard.sequence.wrapping_add(1);
            clipboard.fetched = false;
            let clipboard_state = self.state_of(clipboard);
            (clipboard_state, state.instances.clone())
        };
        for instance in instances {
            instance.clipboard_changed(ClipboardSelection::Clipboard, state);
        }
    }

    fn state_of(&self, clipboard: &Clipboard) -> ClipboardState {
        let known = self.inner.options.clipboard_sync || clipboard.fetched;
        let mut flags = 0;
        if !known {
            flags |= clipboard_flags::NEEDS_REQUEST;
        } else if clipboard
            .items
            .iter()
            .any(|item| item.mime_type == "text/plain")
        {
            flags |= clipboard_flags::HAS_TEXT;
        }
        ClipboardState {
            sequence: clipboard.sequence,
            flags,
        }
    }
}

impl WindowSystem for HeadlessSystem {
    fn info(&self) -> SystemInfo {
        use FeatureName::*;
        let options = &self.inner.options;
        let mut features = vec![
            Windows,
            WindowPosition,
            WindowStates,
            FullscreenBorderless,
            Displays,
            TextInput,
            Touch,
            Pen,
            RawInput,
            CursorShapes,
            CustomCursors,
            CursorWarp,
            CursorConfine,
            CursorLock,
            ClipboardText,
            Theme,
            PixelPresent,
        ];
        if options.top_level {
            features.push(TopLevelWindows);
        }
        if options.clipboard_sync {
            features.push(ClipboardSyncRead);
        }
        SystemInfo {
            host_kind: HostKind::Headless,
            features,
            shortcut_modifier: options.shortcut_modifier,
            input_devices: input_devices::KEYBOARD | input_devices::MOUSE,
            theme: options.theme,
            max_cursor_size: 256,
        }
    }

    fn now(&self) -> u64 {
        match self.inner.options.clock {
            HeadlessClock::Real => self.inner.started.elapsed().as_nanos() as u64,
            HeadlessClock::Manual => self.inner.time.load(Ordering::Acquire),
        }
    }

    fn displays(&self) -> Vec<DisplayData> {
        let options = &self.inner.options;
        let (width, height) = options.display;
        vec![DisplayData {
            id: DISPLAY,
            name: "Headless".to_owned(),
            x: 0,
            y: 0,
            width,
            height,
            work_area: (0, 0, width, height),
            scale_factor: options.scale_factor,
            refresh_rate: options.refresh_rate,
            primary: true,
        }]
    }

    fn attach(&self, instance: InstanceSink) {
        let mut state = lock(&self.inner.state);
        state.instances.retain(|instance| !instance.is_closed());
        state.instances.push(instance);
    }

    fn create_window(
        &self,
        attributes: WindowAttributes,
        mut sink: WindowSink,
    ) -> Result<Box<dyn HostWindow>, Status> {
        let options = &self.inner.options;
        let mut state = lock(&self.inner.state);
        let taken = |state: &State, target: &str| {
            state.windows.iter().any(|window| window.target == target)
        };
        let target = if !attributes.target.is_empty() {
            if !options.targets.contains(&attributes.target) {
                return Err(Status::NotFound);
            }
            if taken(&state, &attributes.target) {
                return Err(Status::Busy);
            }
            attributes.target.clone()
        } else if let Some(free) = options.targets.iter().find(|name| !taken(&state, name)) {
            // The default target is the first one nobody has yet.
            free.clone()
        } else if attributes.top_level && options.top_level {
            String::new()
        } else if options.targets.is_empty() {
            return Err(Status::Unsupported);
        } else {
            return Err(Status::Busy);
        };

        let scale = |value: u32| {
            if attributes.logical_units {
                (f64::from(value) * options.scale_factor).round() as u32
            } else {
                value
            }
        };
        let pair = |(a, b): (u32, u32)| (scale(a), scale(b));
        let (min_size, max_size) = (pair(attributes.min_size), pair(attributes.max_size));
        let size = attributes.size.map_or(options.size, pair);
        let model = HeadlessWindowModel {
            title: attributes.title,
            size,
            min_size,
            max_size,
            flags: attributes.flags,
            fullscreen: FullscreenMode::None,
            focused: false,
            cursor_shape: CursorShape::Default,
            custom_cursor: false,
            cursor_visible: true,
            cursor_grab: CursorGrab::None,
            text_input: None,
            default_actions: default_actions::DEFAULT,
            restore: size,
        };

        sink.display_changed(DISPLAY);
        sink.theme_changed(options.theme);
        sink.flags(window_flags::REQUESTABLE, attributes.flags);
        if let Some((x, y)) = attributes.position {
            let position = |value: i32| {
                if attributes.logical_units {
                    (f64::from(value) * options.scale_factor).round() as i32
                } else {
                    value
                }
            };
            sink.moved(position(x), position(y));
        }
        let window = Arc::new(WindowState {
            id: self.inner.next_window.fetch_add(1, Ordering::AcqRel),
            // A window that adopted a target is drawn on by that target's
            // name; one of its own gets a name nobody can guess.
            selector: if target.is_empty() {
                new_surface_selector()
            } else {
                target.clone()
            },
            target,
            system: Arc::downgrade(&self.inner),
            alive: AtomicBool::new(true),
            sink: Mutex::new(sink),
            model: Mutex::new(model),
            requests: Mutex::new(Vec::new()),
            frames: Mutex::new(Frames::default()),
            redraw_wanted: AtomicBool::new(false),
        });
        state.windows.push(Arc::clone(&window));
        drop(state);
        // Not called under a lock: a watcher may ask anything of the system.
        let watchers = lock(&self.inner.watchers).clone();
        for watcher in watchers {
            watcher(&HeadlessWindow {
                state: Arc::clone(&window),
            });
        }
        // The size comes last, clamped to the limits: it is what tells the
        // guest that the window exists.
        window.resize(size.0, size.1);
        Ok(Box::new(Window(window)))
    }

    fn clipboard_state(&self, selection: ClipboardSelection) -> ClipboardState {
        let mut state = lock(&self.inner.state);
        let clipboard = state.clipboard(selection);
        self.state_of(clipboard)
    }

    fn clipboard_read(
        &self,
        selection: ClipboardSelection,
        mime_type: &str,
    ) -> Result<Vec<u8>, Status> {
        let mut state = lock(&self.inner.state);
        let clipboard = state.clipboard(selection);
        if !self.inner.options.clipboard_sync && !clipboard.fetched {
            return Err(Status::Pending);
        }
        clipboard
            .items
            .iter()
            .find(|item| item.mime_type == mime_type)
            .map(|item| item.data.clone())
            .ok_or(Status::NotFound)
    }

    fn clipboard_request(
        &self,
        selection: ClipboardSelection,
        mime_type: &str,
        request: PendingRequest,
    ) {
        let found = {
            let mut state = lock(&self.inner.state);
            let clipboard = state.clipboard(selection);
            clipboard.fetched = true;
            clipboard
                .items
                .iter()
                .any(|item| item.mime_type == mime_type)
        };
        request.complete(if found {
            Status::Success
        } else {
            Status::NotFound
        });
    }

    fn clipboard_write(
        &self,
        selection: ClipboardSelection,
        items: Vec<ClipboardData>,
        request: PendingRequest,
    ) {
        let (clipboard_state, instances) = {
            let mut state = lock(&self.inner.state);
            let clipboard = state.clipboard(selection);
            clipboard.items = items;
            clipboard.sequence = clipboard.sequence.wrapping_add(1);
            clipboard.fetched = true;
            let clipboard_state = self.state_of(clipboard);
            (clipboard_state, state.instances.clone())
        };
        request.complete(Status::Success);
        for instance in instances {
            instance.clipboard_changed(selection, clipboard_state);
        }
    }
}
