//! The window system behind the API.
//!
//! Everything the host does to a real window, and everything it learns from
//! one, goes through the two traits here. The built-in window systems
//! implement them (a headless one that is always there, winit natively, the
//! page in a browser), and so can an embedder that owns its own views.
//!
//! The division of work: a window system creates windows, applies changes to
//! them and reports what happens, in its own terms and on whatever thread it
//! likes, through the [`WindowSink`] it is given for each window. The host
//! turns those reports into the event records guests read, and keeps the
//! promises the API makes about them (key presses and releases pair up, a
//! window that loses focus sees its keys released first, a flood of motion
//! does not push other input out).

pub use crate::abi::enums::{
    AttentionLevel, ClipboardSelection, CursorGrab, CursorGrabReason, CursorShape, FeatureName,
    FullscreenMode, HostKind, KeyLocation, PhysicalKey, PixelAlpha, PixelFormat, PointerButton,
    PointerKind, PointerTool, ScreenKeyboardPolicy, Status, TextInputPurpose, Theme, WheelPhase,
    WheelUnit,
};
pub use crate::abi::enums::{
    clipboard_flags, default_actions, input_devices, key, modifiers, text_input_hints, wheel_flags,
    window_flags,
};
pub use crate::abi::structs::Rect;
pub use crate::host::request::PendingRequest;
pub use crate::host::sink::{
    InstanceSink, KeyInput, PointerInput, PointerPhase, SinkWatch, WheelInput, WindowSink,
};

/// What a window system is and can do.
#[derive(Clone, Debug)]
pub struct SystemInfo {
    pub host_kind: HostKind,
    /// What it can do. Guests are offered these as far as the embedder's
    /// policy allows.
    pub features: Vec<FeatureName>,
    /// The modifier the platform's shortcuts use: [`modifiers::CONTROL`], or
    /// [`modifiers::SUPER`] on a Mac.
    pub shortcut_modifier: u32,
    /// The [`input_devices`] it knows of.
    pub input_devices: u32,
    pub theme: Theme,
    /// The largest edge of a custom cursor, in pixels. 0 if unknown.
    pub max_cursor_size: u32,
}

/// A screen.
#[derive(Clone, Debug, PartialEq)]
pub struct DisplayData {
    /// The window system's own name for the display: stable while it exists.
    pub id: u64,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// The part windows can use, as (x, y, width, height).
    pub work_area: (i32, i32, u32, u32),
    pub scale_factor: f64,
    /// In thousandths of a hertz. 0 if unknown.
    pub refresh_rate: u32,
    pub primary: bool,
}

/// What a guest asked for when it opened a window.
#[derive(Clone, Debug, Default)]
pub struct WindowAttributes {
    pub title: String,
    /// The embedder-provided target to adopt. Empty is the default target.
    pub target: String,
    pub application_id: String,
    /// The requestable [`window_flags`] to start with.
    pub flags: u64,
    /// Sizes, limits and position are logical: multiply by the scale factor
    /// of the display the window opens on.
    pub logical_units: bool,
    pub size: Option<(u32, u32)>,
    /// (0, 0) is no limit.
    pub min_size: (u32, u32),
    pub max_size: (u32, u32),
    pub position: Option<(i32, i32)>,
    /// A [`DisplayData::id`].
    pub display: Option<u64>,
    /// The guest may be given a window of its own when there is no target
    /// for it to adopt. Without this, only targets can become windows.
    pub top_level: bool,
}

/// A custom cursor: RGBA, 8 bits each, straight alpha, rows from the top.
#[derive(Clone, Debug)]
pub struct CursorImage {
    pub width: u32,
    pub height: u32,
    pub hotspot_x: u32,
    pub hotspot_y: u32,
    pub scale_factor: f64,
    pub rgba: Vec<u8>,
}

/// How a window should go fullscreen.
#[derive(Clone, Debug)]
pub struct Fullscreen {
    pub mode: FullscreenMode,
    /// A [`DisplayData::id`], or the display the window is on.
    pub display: Option<u64>,
    /// Fail rather than wait for a user gesture.
    pub no_deferral: bool,
}

/// A text entry session, as `wguiWindowSetTextInput` describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct TextInput {
    pub purpose: TextInputPurpose,
    /// [`text_input_hints`].
    pub hints: u32,
    pub screen_keyboard: ScreenKeyboardPolicy,
    /// Where the caret is, in the window's physical pixels.
    pub cursor_area: Rect,
    /// The whole field. Zero size if unknown.
    pub input_area: Rect,
}

/// A frame a guest drew itself, to be shown in its window
/// (`wguiWindowPresentPixels`). It is the guest's copy: the window system may
/// keep it for as long as it likes.
#[derive(Clone, Debug, PartialEq)]
pub struct PixelFrame {
    pub width: u32,
    pub height: u32,
    /// Bytes from the start of one row to the start of the next: `width * 4`
    /// at least.
    pub stride: u32,
    pub format: PixelFormat,
    pub alpha: PixelAlpha,
    /// The rows, from the top: `stride * (height - 1) + width * 4` bytes at
    /// least.
    pub pixels: Vec<u8>,
    /// The parts that differ from the frame before, all of them inside this
    /// one. Empty means everything.
    pub damage: Vec<Rect>,
}

impl PixelFrame {
    /// One row of the frame: `width * 4` bytes.
    pub fn row(&self, y: u32) -> &[u8] {
        let start = y as usize * self.stride as usize;
        &self.pixels[start..start + self.width as usize * 4]
    }

    /// The frame as red, green, blue and alpha, row after row with nothing
    /// in between. Alpha is 255 throughout for an opaque frame, and
    /// otherwise as the guest gave it.
    pub fn to_rgba(&self) -> Vec<u8> {
        let row = self.width as usize * 4;
        let mut out = vec![0; row * self.height as usize];
        // Every frame a browser shows goes through here, megabytes of it:
        // a pixel is one word, turned around and made opaque as a whole.
        let opaque = if self.alpha == PixelAlpha::Opaque {
            0xFF00_0000
        } else {
            0
        };
        let swap = self.format == PixelFormat::Bgra8;
        for (y, line) in out.chunks_exact_mut(row.max(1)).enumerate() {
            let (to, _) = line.as_chunks_mut::<4>();
            let (from, _) = self.row(y as u32).as_chunks::<4>();
            for (to, from) in to.iter_mut().zip(from) {
                let pixel = u32::from_le_bytes(*from);
                let pixel = if swap {
                    (pixel & 0xFF00_FF00) | ((pixel >> 16) & 0xFF) | ((pixel & 0xFF) << 16)
                } else {
                    pixel
                };
                *to = (pixel | opaque).to_le_bytes();
            }
        }
        out
    }

    /// A newer frame takes the place of one nobody showed. What the newer
    /// one says changed was said of that one, which was never seen: all of
    /// the newer frame has to be shown.
    pub fn replacing_an_unseen_frame(mut self) -> Self {
        self.damage.clear();
        self
    }
}

/// Something a guest asked of a window. Nothing here can fail on the spot:
/// what happened is reported through the window's [`WindowSink`], and the two
/// requests that complete later carry the [`PendingRequest`] to answer.
pub enum WindowChange {
    Title(String),
    /// A new size for the content area. Answer with [`WindowSink::resized`].
    Size {
        width: u32,
        height: u32,
    },
    /// (0, 0) is no limit.
    SizeLimits {
        min: (u32, u32),
        max: (u32, u32),
    },
    Position {
        x: i32,
        y: i32,
    },
    /// Set the requestable [`window_flags`] in `mask` to their bits in
    /// `values`.
    Flags {
        mask: u64,
        values: u64,
    },
    /// Enter fullscreen, or leave it with `None`.
    Fullscreen {
        options: Option<Fullscreen>,
        request: PendingRequest,
    },
    Focus,
    Attention(AttentionLevel),
    /// Report [`WindowSink::redraw_requested`] when the window can be drawn.
    Redraw {
        /// The guest asked while it was still drawing the frame it was last
        /// told to draw: it had not waited for events since. It is drawing
        /// frame after frame, and should be told at the display's next
        /// frame even by a window system that could tell it sooner.
        while_drawing: bool,
    },
    CursorShape(CursorShape),
    /// Show this cursor, or the default one with `None`.
    CustomCursor(Option<std::sync::Arc<CursorImage>>),
    CursorVisible(bool),
    CursorGrab {
        grab: CursorGrab,
        request: PendingRequest,
    },
    CursorPosition {
        x: f64,
        y: f64,
    },
    /// Start or update text entry, or stop it with `None`.
    TextInput(Option<TextInput>),
    /// Discard the text being composed.
    ResetTextInput,
    /// The embedder reactions to input ([`default_actions`]) to prevent.
    DefaultActions(u32),
    /// Show this frame. Frames that come faster than they can be shown
    /// replace each other: only the last one matters.
    Pixels(PixelFrame),
}

/// A window of the window system. Dropping it destroys the window.
pub trait HostWindow: Send + Sync {
    fn apply(&self, change: WindowChange);

    /// The name a WebGPU surface for this window is created with: what the
    /// guest puts in `WGPUWasixSurfaceSourceCanvas`, and what the window
    /// system is later asked to find the window by. A window that adopted a
    /// named target is called by that name; any other gets a name from
    /// [`new_surface_selector`].
    fn surface_selector(&self) -> String;
}

/// The prefix of the surface selectors made up by [`new_surface_selector`].
pub const SURFACE_SELECTOR_PREFIX: &str = "wgui:";

/// A surface selector nobody can guess, so that only who was given a
/// window's selector can draw on the window.
pub fn new_surface_selector() -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let word = || {
        // Each `RandomState` is keyed from the system's randomness.
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        hasher.finish()
    };
    format!("{SURFACE_SELECTOR_PREFIX}{:016x}{:016x}", word(), word())
}

/// One representation of what is on a clipboard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardData {
    pub mime_type: String,
    pub data: Vec<u8>,
}

/// What a window system knows about a clipboard without reading it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClipboardState {
    /// Goes up when the content changes.
    pub sequence: u32,
    /// [`clipboard_flags`], except `OWNED`, which the host works out.
    pub flags: u32,
}

/// A window system.
pub trait WindowSystem: Send + Sync + 'static {
    fn info(&self) -> SystemInfo;

    /// The time on the clock its events are stamped with, in nanoseconds.
    fn now(&self) -> u64;

    /// The displays, the primary one first.
    fn displays(&self) -> Vec<DisplayData>;

    /// A guest connected. `instance` is where what concerns no window in
    /// particular is reported; it says when the guest has gone.
    fn attach(&self, instance: InstanceSink) {
        let _ = instance;
    }

    /// Opens a window, or adopts the target the attributes name.
    ///
    /// Before returning, report the window's real size and scale factor with
    /// [`WindowSink::resized`], and any flag that is already set: the host
    /// answers the guest's first questions from that. May block.
    ///
    /// Errors a guest can be told: `NotFound` for an unknown target, `Busy`
    /// for one already adopted, `Unsupported`, `Denied`, `Error`.
    fn create_window(
        &self,
        attributes: WindowAttributes,
        sink: WindowSink,
    ) -> Result<Box<dyn HostWindow>, Status>;

    fn clipboard_state(&self, selection: ClipboardSelection) -> ClipboardState {
        let _ = selection;
        ClipboardState::default()
    }

    /// The clipboard's content of one MIME type, if the window system can
    /// tell right away: `Err(Status::Pending)` when it has to ask first (see
    /// [`WindowSystem::clipboard_request`]), `Err(Status::NotFound)` when
    /// there is none.
    fn clipboard_read(
        &self,
        selection: ClipboardSelection,
        mime_type: &str,
    ) -> Result<Vec<u8>, Status> {
        let _ = (selection, mime_type);
        Err(Status::Unsupported)
    }

    /// Fetches the content so that a later read can answer, then completes
    /// `request`.
    fn clipboard_request(
        &self,
        selection: ClipboardSelection,
        mime_type: &str,
        request: PendingRequest,
    ) {
        let _ = (selection, mime_type);
        request.complete(Status::Unsupported);
    }

    /// Replaces the clipboard's content, then completes `request`.
    fn clipboard_write(
        &self,
        selection: ClipboardSelection,
        items: Vec<ClipboardData>,
        request: PendingRequest,
    ) {
        let _ = (selection, items);
        request.complete(Status::Unsupported);
    }
}

/// The window system of an embedder that has none: guests can ask what
/// there is, and are told that there is nothing. What a [`GuiCtx`]
/// (crate::GuiCtx) uses in a browser until it is given the page's.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoWindows;

impl WindowSystem for NoWindows {
    fn info(&self) -> SystemInfo {
        SystemInfo {
            host_kind: HostKind::Unknown,
            features: Vec::new(),
            shortcut_modifier: modifiers::CONTROL,
            input_devices: 0,
            theme: Theme::Unknown,
            max_cursor_size: 0,
        }
    }

    fn now(&self) -> u64 {
        crate::clock::now().as_nanos() as u64
    }

    fn displays(&self) -> Vec<DisplayData> {
        Vec::new()
    }

    fn create_window(
        &self,
        _attributes: WindowAttributes,
        _sink: WindowSink,
    ) -> Result<Box<dyn HostWindow>, Status> {
        Err(Status::Unsupported)
    }
}
