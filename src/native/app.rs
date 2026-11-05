//! The loop's thread: the only place that touches winit's windows.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize, PhysicalPosition, PhysicalSize, Position, Size};
use winit::event::{DeviceEvent, DeviceId, ElementState, Ime, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::monitor::MonitorHandle;
use winit::window::{
    CursorGrabMode, CustomCursor, Fullscreen as WinitFullscreen, ImePurpose, ResizeDirection,
    UserAttentionType, Window, WindowId, WindowLevel,
};

use super::title_bar::{self, DoubleClick};
use super::{CloseBehavior, Command, Created, Shared, WindowKey, convert, lock};
use crate::host::log;
use crate::system::*;

/// A press in a drag region: when, where in the window, and where the window
/// was then.
type DragPress = (Instant, (f64, f64), Option<(i32, i32)>);

/// One window, as the loop keeps it.
struct Entry {
    key: WindowKey,
    window: Arc<Window>,
    /// The name a surface for the window is created with.
    selector: String,
    /// The window's size for whoever draws on it: width, then height in the
    /// upper half.
    size: Arc<std::sync::atomic::AtomicU64>,
    /// How many times the window system has said whether the window can be
    /// seen. Whoever draws on it has a word on that too (`Command::Unseen`),
    /// and this is how a word that comes too late is known.
    seen_said: Arc<std::sync::atomic::AtomicU64>,
    sink: WindowSink,
    /// Where the mouse cursor last was, in the window.
    cursor: (f64, f64),
    /// The modifiers last reported for the window.
    modifiers: u32,
    focused: bool,
    /// Whether the guest has the window shown. That is what it is told, not
    /// what the window system says at the moment: X11 puts a window on
    /// screen when it gets to it, which is after the window was made, and
    /// whether anything of a window can be seen is another flag.
    shown: bool,
    /// What the guest wants of the cursor, and whether it has it right now.
    grab: CursorGrab,
    grabbed: bool,
    cursor_visible: bool,
    /// When the user asked to close the window, and how many records the
    /// guest had read by then.
    close_asked: Option<(Instant, u64)>,
    /// winit answers a request to redraw at once, however often it is asked
    /// and whether or not anybody can see the window. A guest is to be told
    /// once per refresh of the display, and not at all while its window
    /// cannot be seen, so requests are held back here: the refresh the last
    /// request was for, when a request that came before the next one is due,
    /// and whether the window is known to be out of sight.
    redraw_tick: Option<Instant>,
    redraw_due: Option<Instant>,
    unseen: bool,
    /// What the window shows when its guest draws it without a GPU.
    #[cfg(feature = "pixels")]
    painted: Option<super::pixels::Painted>,
    /// The title bar the window has, and how tall the guest draws it, in
    /// logical units (0: the system's usual height).
    title_bar: TitleBarStyle,
    title_bar_height: f64,
    /// Where presses move the window instead of reaching the guest.
    drag_regions: Vec<Rect>,
    /// A press went to the window system (a move or a resize): its release
    /// is not the guest's either. Some systems keep that release (X11,
    /// Wayland and macOS end a move with it), so the next press, or the
    /// pointer coming back, ends the debt too.
    owed_release: bool,
    /// The last press in a drag region: when, where in the window, and where
    /// the window was. What tells a double click where the platform does not
    /// count clicks; a window that moved in between was dragged, not clicked.
    last_drag_press: Option<DragPress>,
    /// Whether the window's edges resize it: resizable, and neither maximized
    /// nor fullscreen. Kept from the last report for the cursor, as asking
    /// X11 on every motion would be a round trip each time; a press asks the
    /// window.
    resizes_from_edges: bool,
    /// Whether the window was fullscreen when its title bar was last
    /// reported: macOS leaves fullscreen after the last resize it reports.
    was_fullscreen: bool,
    /// The edge of a `Custom` window the cursor is over, which shows a
    /// resize cursor instead of the guest's. Only for the cursor: a press
    /// finds its edge again, as the window may have changed under a pointer
    /// that has not moved since.
    resize_edge: Option<ResizeDirection>,
    /// Whether the pointer is in the window, where `cursor` is.
    pointer_inside: bool,
    /// The cursor the guest asked for, shown again off the edges.
    guest_cursor: winit::window::Cursor,
    /// The guest's icon, for showing again at another scale.
    icon: Option<Arc<Icon>>,
}

impl Entry {
    /// Tells the guest, and whoever draws on the window, its size.
    fn resized(&mut self, width: u32, height: u32, scale_factor: f64) {
        self.size.store(
            u64::from(width) | (u64::from(height) << 32),
            std::sync::atomic::Ordering::Release,
        );
        self.sink.resized(width, height, scale_factor);
    }

    /// Tells the guest what the window is like now, beyond its size.
    fn report_state(&mut self) {
        let window = &self.window;
        let mut flags = 0;
        let mut set = |on: bool, flag: u64| {
            if on {
                flags |= flag;
            }
        };
        set(self.shown, window_flags::VISIBLE);
        set(window.is_resizable(), window_flags::RESIZABLE);
        set(window.is_decorated(), window_flags::DECORATED);
        set(
            window.is_minimized().unwrap_or(false),
            window_flags::MINIMIZED,
        );
        set(window.is_maximized(), window_flags::MAXIMIZED);
        self.resizes_from_edges = flags & window_flags::RESIZABLE != 0
            && flags & window_flags::MAXIMIZED == 0
            && window.fullscreen().is_none();
        self.sink.flags(
            window_flags::VISIBLE
                | window_flags::RESIZABLE
                | window_flags::DECORATED
                | window_flags::MINIMIZED
                | window_flags::MAXIMIZED,
            flags,
        );
        let fullscreen = match window.fullscreen() {
            None => FullscreenMode::None,
            Some(WinitFullscreen::Borderless(_)) => FullscreenMode::Borderless,
            Some(WinitFullscreen::Exclusive(_)) => FullscreenMode::Exclusive,
        };
        self.sink.fullscreen(fullscreen);
        self.report_place();
    }

    /// Where the window's content is, and how far its frame reaches beyond.
    fn report_place(&mut self) {
        let window = &self.window;
        let (Ok(inner), Ok(outer)) = (window.inner_position(), window.outer_position()) else {
            return;
        };
        let (inner_size, outer_size) = (window.inner_size(), window.outer_size());
        let left = (inner.x - outer.x).max(0) as u32;
        let top = (inner.y - outer.y).max(0) as u32;
        self.sink.frame([
            left,
            top,
            outer_size.width.saturating_sub(inner_size.width + left),
            outer_size.height.saturating_sub(inner_size.height + top),
        ]);
        self.sink.moved(inner.x, inner.y);
    }

    /// Takes or releases the cursor as the guest wants it, as far as the
    /// window having focus allows. `Err` when the platform will not.
    fn apply_grab(&mut self, reason: CursorGrabReason) -> Result<(), Status> {
        let wanted = if self.focused {
            self.grab
        } else {
            CursorGrab::None
        };
        let mode = match wanted {
            CursorGrab::None => CursorGrabMode::None,
            CursorGrab::Confined => CursorGrabMode::Confined,
            CursorGrab::Locked => CursorGrabMode::Locked,
        };
        if let Err(error) = self.window.set_cursor_grab(mode) {
            log(|| format!("cursor grab {wanted:?}: {error}"));
            self.grabbed = false;
            self.window.set_cursor_visible(self.cursor_visible);
            self.sink
                .cursor_grab_changed(CursorGrab::None, CursorGrabReason::Host);
            return Err(Status::Unsupported);
        }
        self.grabbed = wanted != CursorGrab::None;
        // A locked cursor is not shown, whatever its visibility otherwise.
        self.window
            .set_cursor_visible(self.cursor_visible && wanted != CursorGrab::Locked);
        self.sink.cursor_grab_changed(wanted, reason);
        Ok(())
    }

    /// Tells the guest its title bar, after placing the system's controls
    /// in it.
    fn report_title_bar(&mut self) {
        let scale = self.window.scale_factor();
        let size = self.window.inner_size();
        let physical = |value: f64| (value * scale).round().max(0.0) as u32;
        let height = if self.title_bar_height > 0.0 {
            self.title_bar_height
        } else {
            title_bar::CUSTOM_HEIGHT
        };
        self.was_fullscreen = self.window.fullscreen().is_some();
        let area = match self.title_bar {
            TitleBarStyle::System => [0; 4],
            // The title bar hides with the rest of the window's frame.
            _ if self.was_fullscreen => [0; 4],
            #[cfg(target_os = "macos")]
            TitleBarStyle::Overlay => {
                match title_bar::layout_controls(&self.window, self.title_bar_height) {
                    // What is right of the buttons, less as much space as is
                    // left of them.
                    Some(controls) => {
                        let x = physical(controls.right + controls.left);
                        [x, 0, size.width.saturating_sub(x), physical(controls.bar)]
                    }
                    None => [0; 4],
                }
            }
            _ => [0, 0, size.width, physical(height)],
        };
        self.sink.title_bar(self.title_bar, area);
        // This follows every change to the window's size, scale or state,
        // as do the edges that resize it: a pointer that stayed put may be
        // on one now, or off one. Not while the system moves or resizes the
        // window, which owns the pointer meanwhile: Windows reports the
        // sizes of its resize but none of the pointer's moves, and the
        // cursor set would replace the one its resize shows.
        if !self.owed_release {
            self.hover_edges();
        }
    }

    /// A press of the primary button that the window system takes, because
    /// it is in a drag region or on an edge that resizes the window: moves,
    /// zooms or resizes the window, and says so.
    fn system_press(&mut self) -> bool {
        // Whatever release a move before kept, this press starts anew.
        self.owed_release = false;
        // A fullscreen window is not resized, moved or zoomed.
        let fullscreen = self.window.fullscreen().is_some();
        // The edge is found from the window's size and state at the press,
        // not from the cursor shown: the window may have been resized,
        // maximized or made fullscreen under a pointer that has not moved
        // since. The window is asked only on an edge of the size last
        // reported, as on X11 each question is a round trip.
        let edge = if self.edge_at(self.last_size(), true).is_some() {
            let size = self.window.inner_size();
            let resizable =
                !fullscreen && self.window.is_resizable() && !self.window.is_maximized();
            self.edge_at((size.width, size.height), resizable)
        } else {
            None
        };
        self.show_edge(edge);
        if let Some(edge) = edge {
            if self.window.drag_resize_window(edge).is_ok() {
                self.owed_release = true;
                return true;
            }
            return false;
        }
        if fullscreen || !title_bar::in_regions(&self.drag_regions, self.cursor) {
            self.last_drag_press = None;
            return false;
        }
        let now = Instant::now();
        let place = self
            .window
            .outer_position()
            .ok()
            .map(|place| (place.x, place.y));
        let double = match title_bar::click_count(&self.window) {
            // The second click only, as AppKit's own title bar: a third
            // does not undo what the second did.
            Some(count) => count == 2,
            None => self.last_drag_press.is_some_and(|(when, (x, y), before)| {
                now.duration_since(when) <= title_bar::DOUBLE_CLICK
                    && before == place
                    && (x - self.cursor.0).hypot(y - self.cursor.1)
                        <= title_bar::DOUBLE_CLICK_DISTANCE * self.window.scale_factor()
            }),
        };
        if double {
            self.last_drag_press = None;
            match title_bar::double_click_action() {
                // Not a window the user may not resize, as the system's own
                // title bar does not zoom one either. winit maximizes one
                // when asked to, by setting its frame.
                DoubleClick::Zoom if self.window.is_resizable() => {
                    self.window.set_maximized(!self.window.is_maximized());
                }
                DoubleClick::Minimize => self.window.set_minimized(true),
                DoubleClick::Zoom | DoubleClick::Nothing => {}
            }
            self.report_state();
        } else {
            self.last_drag_press = Some((now, self.cursor, place));
            if let Err(error) = self.window.drag_window() {
                log(|| format!("cannot move the window: {error}"));
            }
        }
        self.owed_release = true;
        true
    }

    /// The edge the pointer is on that resizes the window, when the window
    /// is `size` big and its edges resize it or not (`resizable`): what the
    /// cursor shown and a press both go by.
    fn edge_at(&self, size: (u32, u32), resizable: bool) -> Option<ResizeDirection> {
        if !resizable || !title_bar::host_resizes(self.title_bar) {
            return None;
        }
        let band = title_bar::RESIZE_BAND * self.window.scale_factor();
        title_bar::resize_edge(self.cursor, size, band)
    }

    /// Shows a resize cursor over the edges of a `Custom` window, where the
    /// window system draws no frame to resize it by.
    fn hover_edges(&mut self) {
        let edge = if self.pointer_inside {
            self.edge_at(self.last_size(), self.resizes_from_edges)
        } else {
            None
        };
        self.show_edge(edge);
    }

    /// The size last reported, in physical pixels.
    fn last_size(&self) -> (u32, u32) {
        let size = self.size.load(std::sync::atomic::Ordering::Acquire);
        (size as u32, (size >> 32) as u32)
    }

    /// Shows the resize cursor for `edge`, or the guest's off the edges.
    fn show_edge(&mut self, edge: Option<ResizeDirection>) {
        if edge != self.resize_edge {
            self.resize_edge = edge;
            match edge {
                Some(edge) => self.window.set_cursor(title_bar::resize_cursor(edge)),
                None => self.window.set_cursor(self.guest_cursor.clone()),
            }
        }
    }

    fn pointer(&mut self, phase: PointerPhase, button: u32) {
        self.sink.pointer(PointerInput {
            phase,
            button,
            x: self.cursor.0,
            y: self.cursor.1,
            ..PointerInput::default()
        });
    }
}

pub(super) struct App {
    shared: Arc<Shared>,
    windows: HashMap<WindowId, Entry>,
    keys: HashMap<WindowKey, WindowId>,
    next_key: WindowKey,
    /// The displays the guest has been told of, each with the id it has here.
    monitors: Vec<(MonitorHandle, u64)>,
    /// The platform is ready for windows (winit's `resumed`). Before that,
    /// what guests ask for waits.
    ready: bool,
    closed_by_user: bool,
    /// Wakes the loop when the next redraw held back for a window that can
    /// be seen is due: on macOS the loop's own timer can be milliseconds
    /// late. What the timer calls holds `shared` only weakly, and may still
    /// be running when this is dropped.
    #[cfg(target_os = "macos")]
    redraw_waker: Option<crate::macos_deadline::Deadline>,
    /// Shows the frames guests draw themselves, from the first one on.
    #[cfg(feature = "pixels")]
    painter: Option<super::pixels::Painter>,
    /// The windows that have the guest's icon, in the order they were given
    /// it, and the application's icon (macOS: icon.rs), which puts the one
    /// from before back when this is dropped.
    icon_order: Vec<WindowKey>,
    app_icon: super::icon::AppIcon,
    #[cfg(target_os = "macos")]
    _motion_observer: super::preferences::Observer,
}

impl App {
    pub(super) fn new(shared: Arc<Shared>) -> Self {
        // On a thread of libdispatch's: it only wakes the loop.
        #[cfg(target_os = "macos")]
        let redraw_waker = crate::macos_deadline::Deadline::new({
            let shared = Arc::downgrade(&shared);
            move || {
                if let Some(shared) = shared.upgrade()
                    && let super::Link::Running(proxy) = &*lock(&shared.link)
                {
                    let _ = proxy.send_event(());
                }
            }
        });
        #[cfg(target_os = "macos")]
        let motion_observer = super::preferences::Observer::new({
            let shared = Arc::downgrade(&shared);
            move |preference| {
                if let Some(shared) = shared.upgrade() {
                    shared.update_reduced_motion(preference);
                }
            }
        });
        Self {
            shared,
            windows: HashMap::new(),
            keys: HashMap::new(),
            next_key: 1,
            monitors: Vec::new(),
            ready: false,
            closed_by_user: false,
            #[cfg(target_os = "macos")]
            redraw_waker,
            #[cfg(feature = "pixels")]
            painter: None,
            icon_order: Vec::new(),
            app_icon: super::icon::AppIcon::new(),
            #[cfg(target_os = "macos")]
            _motion_observer: motion_observer,
        }
    }

    pub(super) fn closed_by_user(&self) -> bool {
        self.closed_by_user
    }

    fn entry(&mut self, key: WindowKey) -> Option<&mut Entry> {
        let id = self.keys.get(&key)?;
        self.windows.get_mut(id)
    }

    /// The id a monitor has for the guest, given on first sight.
    fn monitor_id(&mut self, monitor: &MonitorHandle) -> u64 {
        if let Some((_, id)) = self.monitors.iter().find(|(known, _)| known == monitor) {
            return *id;
        }
        let id = self.monitors.len() as u64 + 1;
        self.monitors.push((monitor.clone(), id));
        id
    }

    fn monitor(&self, id: u64) -> Option<MonitorHandle> {
        self.monitors
            .iter()
            .find_map(|(monitor, known)| (*known == id).then(|| monitor.clone()))
    }

    fn displays(&mut self, event_loop: &ActiveEventLoop) -> Vec<DisplayData> {
        let primary = event_loop.primary_monitor();
        let mut monitors: Vec<MonitorHandle> = event_loop.available_monitors().collect();
        if monitors.is_empty() {
            // Seen on macOS: the list is empty while there is a primary
            // display. One display is better than none.
            monitors.extend(primary.clone());
        }
        let mut displays: Vec<DisplayData> = monitors
            .into_iter()
            .map(|monitor| {
                let (position, size) = (monitor.position(), monitor.size());
                DisplayData {
                    id: self.monitor_id(&monitor),
                    name: monitor.name().unwrap_or_default(),
                    x: position.x,
                    y: position.y,
                    width: size.width,
                    height: size.height,
                    // winit does not say what docks and menu bars take.
                    work_area: (position.x, position.y, size.width, size.height),
                    scale_factor: monitor.scale_factor(),
                    refresh_rate: monitor.refresh_rate_millihertz().unwrap_or(0),
                    primary: primary.as_ref() == Some(&monitor),
                }
            })
            .collect();
        // The primary display comes first.
        displays.sort_by_key(|display| !display.primary);
        displays
    }

    fn create(
        &mut self,
        event_loop: &ActiveEventLoop,
        attributes: WindowAttributes,
        mut sink: WindowSink,
    ) -> Result<Created, Status> {
        let size = |(width, height): (u32, u32)| -> Size {
            if attributes.logical_units {
                LogicalSize::new(f64::from(width), f64::from(height)).into()
            } else {
                PhysicalSize::new(width, height).into()
            }
        };
        let flag = |flag: u64| attributes.flags & flag != 0;
        let mut wanted = Window::default_attributes()
            .with_title(attributes.title.clone())
            .with_visible(flag(window_flags::VISIBLE))
            .with_resizable(flag(window_flags::RESIZABLE))
            .with_decorations(flag(window_flags::DECORATED))
            .with_maximized(flag(window_flags::MAXIMIZED))
            .with_transparent(flag(window_flags::TRANSPARENT))
            .with_active(self.shared.options.activate)
            .with_window_level(if flag(window_flags::ALWAYS_ON_TOP) {
                WindowLevel::AlwaysOnTop
            } else if flag(window_flags::ALWAYS_ON_BOTTOM) {
                WindowLevel::AlwaysOnBottom
            } else {
                WindowLevel::Normal
            });
        // A title bar the guest draws: the content takes the whole window.
        let title_bar = attributes.title_bar;
        #[cfg(target_os = "macos")]
        if title_bar != TitleBarStyle::System {
            use winit::platform::macos::WindowAttributesExtMacOS;
            wanted = wanted
                .with_titlebar_transparent(true)
                .with_title_hidden(true)
                .with_fullsize_content_view(true)
                .with_titlebar_buttons_hidden(title_bar == TitleBarStyle::Custom);
        }
        #[cfg(not(target_os = "macos"))]
        if title_bar == TitleBarStyle::Custom {
            wanted = wanted.with_decorations(false);
            // Undecorated, a window has no shadow on Windows unless asked.
            #[cfg(target_os = "windows")]
            {
                use winit::platform::windows::WindowAttributesExtWindows;
                wanted = wanted.with_undecorated_shadow(true);
            }
        }
        if let Some(wanted_size) = attributes.size {
            wanted = wanted.with_inner_size(size(wanted_size));
        }
        if attributes.min_size != (0, 0) {
            wanted = wanted.with_min_inner_size(size(attributes.min_size));
        }
        if attributes.max_size != (0, 0) {
            wanted = wanted.with_max_inner_size(size(attributes.max_size));
        }
        if let Some((x, y)) = attributes.position {
            let position: Position = if attributes.logical_units {
                LogicalPosition::new(f64::from(x), f64::from(y)).into()
            } else {
                PhysicalPosition::new(x, y).into()
            };
            wanted = wanted.with_position(position);
        }
        let window = event_loop.create_window(wanted).map_err(|error| {
            log(|| format!("cannot open a window: {error}"));
            Status::Error
        })?;
        if flag(window_flags::MINIMIZED) {
            window.set_minimized(true);
        }

        let key = self.next_key;
        self.next_key += 1;
        let id = window.id();
        if let Some(monitor) = window.current_monitor() {
            let display = self.monitor_id(&monitor);
            sink.display_changed(display);
        }
        sink.theme_changed(convert::theme(window.theme()));
        let size = window.inner_size();
        let scale_factor = window.scale_factor();
        let selector = new_surface_selector();
        let mut entry = Entry {
            key,
            window: Arc::new(window),
            selector: selector.clone(),
            size: Arc::default(),
            seen_said: Arc::default(),
            sink,
            cursor: (0.0, 0.0),
            modifiers: 0,
            focused: false,
            shown: flag(window_flags::VISIBLE),
            grab: CursorGrab::None,
            grabbed: false,
            cursor_visible: true,
            close_asked: None,
            redraw_tick: None,
            redraw_due: None,
            unseen: false,
            #[cfg(feature = "pixels")]
            painted: None,
            title_bar,
            title_bar_height: f64::from(attributes.title_bar_height)
                / if attributes.logical_units {
                    1.0
                } else {
                    scale_factor
                },
            drag_regions: Vec::new(),
            owed_release: false,
            last_drag_press: None,
            resizes_from_edges: false,
            was_fullscreen: false,
            resize_edge: None,
            pointer_inside: false,
            guest_cursor: winit::window::CursorIcon::Default.into(),
            icon: None,
        };
        // Everything the guest can ask about first, then the size, which is
        // what tells it the window is there.
        entry.report_state();
        entry.report_title_bar();
        entry.resized(size.width, size.height, scale_factor);
        self.windows.insert(id, entry);
        self.keys.insert(key, id);
        if attributes.icon.is_some() {
            self.set_icon(key, attributes.icon);
        }
        Ok(Created { key, selector })
    }

    fn destroy(&mut self, key: WindowKey) {
        if let Some(id) = self.keys.remove(&key)
            && let Some(entry) = self.windows.remove(&id)
        {
            // A surface still presenting to the window keeps it alive; it
            // comes back here to be dropped when that surface goes. Until
            // then it is at least not on screen.
            if Arc::strong_count(&entry.window) > 1 {
                entry.window.set_visible(false);
            }
            // Otherwise it is dropped here, on the thread that made it.
            if entry.icon.is_some() {
                self.icon_order.retain(|known| *known != key);
                self.show_app_icon();
            }
        }
    }

    /// Gives a window the guest's icon, or takes it away.
    fn set_icon(&mut self, key: WindowKey, icon: Option<Arc<Icon>>) {
        let Some(entry) = self.entry(key) else {
            return;
        };
        super::icon::show(&entry.window, icon.as_deref());
        let given = icon.is_some();
        entry.icon = icon;
        self.icon_order.retain(|known| *known != key);
        if given {
            self.icon_order.push(key);
        }
        self.show_app_icon();
    }

    /// Where the process has one icon: shows the one given last to a window
    /// that is still open, or the one the application had before.
    fn show_app_icon(&mut self) {
        if !super::icon::ONE_FOR_THE_PROCESS {
            return;
        }
        let wanted = self
            .icon_order
            .last()
            .and_then(|key| self.keys.get(key))
            .and_then(|id| self.windows.get(id))
            .and_then(|entry| entry.icon.clone());
        self.app_icon.show(wanted);
    }

    fn change(&mut self, event_loop: &ActiveEventLoop, key: WindowKey, change: WindowChange) {
        let change = match change {
            WindowChange::Icon(icon) => return self.set_icon(key, icon),
            change => change,
        };
        let monitor_for = |app: &App, display: Option<u64>| display.and_then(|id| app.monitor(id));
        // Fullscreen names a display, which is the loop's to resolve.
        let fullscreen_monitor = match &change {
            WindowChange::Fullscreen {
                options: Some(options),
                ..
            } => monitor_for(self, options.display),
            _ => None,
        };
        let Some(entry) = self.entry(key) else {
            // The window went before the change came. A request among it is
            // answered by being dropped.
            return;
        };
        let window = Arc::clone(&entry.window);
        match change {
            WindowChange::Title(title) => window.set_title(&title),
            WindowChange::Size { width, height } => {
                // `Some` means the size took effect at once and no event will
                // say so; `None` that a resize event follows.
                if let Some(size) = window.request_inner_size(PhysicalSize::new(width, height)) {
                    let scale_factor = window.scale_factor();
                    entry.resized(size.width, size.height, scale_factor);
                    // As after a resize event: the title bar is as wide as
                    // the window, and its edges have moved.
                    entry.report_title_bar();
                }
            }
            WindowChange::SizeLimits { min, max } => {
                let limit = |(width, height): (u32, u32)| {
                    ((width, height) != (0, 0)).then(|| PhysicalSize::new(width, height))
                };
                window.set_min_inner_size(limit(min));
                window.set_max_inner_size(limit(max));
            }
            WindowChange::Position { x, y } => {
                // The guest places the content; winit, the frame around it.
                let offset = match (window.inner_position(), window.outer_position()) {
                    (Ok(inner), Ok(outer)) => (inner.x - outer.x, inner.y - outer.y),
                    _ => (0, 0),
                };
                window.set_outer_position(PhysicalPosition::new(x - offset.0, y - offset.1));
            }
            WindowChange::Flags { mask, values } => {
                let asked = |flag: u64| (mask & flag != 0).then_some(values & flag != 0);
                if let Some(visible) = asked(window_flags::VISIBLE) {
                    entry.shown = visible;
                    window.set_visible(visible);
                }
                if let Some(resizable) = asked(window_flags::RESIZABLE) {
                    window.set_resizable(resizable);
                }
                if let Some(decorated) = asked(window_flags::DECORATED) {
                    window.set_decorations(decorated);
                }
                if let Some(minimized) = asked(window_flags::MINIMIZED) {
                    window.set_minimized(minimized);
                }
                if let Some(maximized) = asked(window_flags::MAXIMIZED) {
                    window.set_maximized(maximized);
                }
                let level = match (
                    asked(window_flags::ALWAYS_ON_TOP),
                    asked(window_flags::ALWAYS_ON_BOTTOM),
                ) {
                    (Some(true), _) => Some(WindowLevel::AlwaysOnTop),
                    (_, Some(true)) => Some(WindowLevel::AlwaysOnBottom),
                    (Some(false), _) | (_, Some(false)) => Some(WindowLevel::Normal),
                    (None, None) => None,
                };
                if let Some(level) = level {
                    window.set_window_level(level);
                }
                // The level is not something winit reports back.
                let levels = window_flags::ALWAYS_ON_TOP | window_flags::ALWAYS_ON_BOTTOM;
                entry.sink.flags(mask & levels, values);
                entry.report_state();
                entry.report_title_bar();
            }
            WindowChange::Fullscreen { options, request } => {
                let status = match &options {
                    Some(options) if options.mode != FullscreenMode::Borderless => {
                        Status::Unsupported
                    }
                    _ => Status::Success,
                };
                if status == Status::Success {
                    window.set_fullscreen(
                        options.map(|_| WinitFullscreen::Borderless(fullscreen_monitor)),
                    );
                    entry.report_state();
                    entry.report_title_bar();
                }
                request.complete(status);
            }
            WindowChange::Focus => window.focus_window(),
            WindowChange::Attention(level) => window.request_user_attention(match level {
                AttentionLevel::None => None,
                AttentionLevel::Informational => Some(UserAttentionType::Informational),
                AttentionLevel::Critical => Some(UserAttentionType::Critical),
            }),
            // A window out of sight is told to redraw when it is seen again.
            WindowChange::Redraw { .. } if entry.unseen => {}
            // Already waiting for the display: one redraw answers both.
            WindowChange::Redraw { .. } if entry.redraw_due.is_some() => {}
            WindowChange::Redraw { .. } => {
                let (tick, held) =
                    next_refresh(entry.redraw_tick, Instant::now(), frame_interval(&window));
                entry.redraw_tick = Some(tick);
                if held {
                    // `about_to_wait` asks when the time has come.
                    entry.redraw_due = Some(tick);
                } else {
                    window.request_redraw();
                }
            }
            WindowChange::CursorShape(shape) => {
                entry.guest_cursor = convert::cursor_icon(shape).into();
                if entry.resize_edge.is_none() {
                    window.set_cursor(entry.guest_cursor.clone());
                }
            }
            WindowChange::CustomCursor(None) => {
                entry.guest_cursor = winit::window::CursorIcon::Default.into();
                if entry.resize_edge.is_none() {
                    window.set_cursor(entry.guest_cursor.clone());
                }
            }
            WindowChange::CustomCursor(Some(image)) => {
                let edge = |value: u32| u16::try_from(value).ok();
                let source = match (
                    edge(image.width),
                    edge(image.height),
                    edge(image.hotspot_x),
                    edge(image.hotspot_y),
                ) {
                    (Some(width), Some(height), Some(x), Some(y)) => {
                        CustomCursor::from_rgba(image.rgba.clone(), width, height, x, y).ok()
                    }
                    _ => None,
                };
                match source {
                    Some(source) => {
                        entry.guest_cursor = event_loop.create_custom_cursor(source).into();
                        if entry.resize_edge.is_none() {
                            window.set_cursor(entry.guest_cursor.clone());
                        }
                    }
                    None => log(|| "a cursor image winit does not take".to_owned()),
                }
            }
            WindowChange::CursorVisible(visible) => {
                entry.cursor_visible = visible;
                let locked = entry.grabbed && entry.grab == CursorGrab::Locked;
                window.set_cursor_visible(visible && !locked);
            }
            WindowChange::CursorGrab { grab, request } => {
                entry.grab = grab;
                // Granted now if the window has focus, and when it next has
                // it otherwise.
                request.complete(match entry.apply_grab(CursorGrabReason::Requested) {
                    Ok(()) => Status::Success,
                    Err(status) => {
                        entry.grab = CursorGrab::None;
                        status
                    }
                });
            }
            WindowChange::CursorPosition { x, y } => {
                if let Err(error) = window.set_cursor_position(PhysicalPosition::new(x, y)) {
                    log(|| format!("cannot move the cursor: {error}"));
                }
            }
            WindowChange::TextInput(text_input) => {
                window.set_ime_allowed(text_input.is_some());
                if let Some(text_input) = text_input {
                    let area = &text_input.cursor_area;
                    window.set_ime_cursor_area(
                        PhysicalPosition::new(area.x, area.y),
                        PhysicalSize::new(area.width, area.height),
                    );
                    window.set_ime_purpose(match text_input.purpose {
                        TextInputPurpose::Password | TextInputPurpose::Pin => ImePurpose::Password,
                        TextInputPurpose::Terminal => ImePurpose::Terminal,
                        _ => ImePurpose::Normal,
                    });
                }
            }
            WindowChange::ResetTextInput => {
                // The way winit has of dropping a composition.
                window.set_ime_allowed(false);
                window.set_ime_allowed(true);
            }
            // A browser's business.
            WindowChange::DefaultActions(_) => {}
            // Frames come by themselves (`Command::Pixels`), where the build
            // can show them at all.
            WindowChange::Pixels(_) => {}
            WindowChange::DragRegions(regions) => entry.drag_regions = regions,
            // Taken above: it may change the application's icon.
            WindowChange::Icon(_) => {}
        }
    }

    /// Takes what guest threads have asked for since the loop last looked.
    fn serve(&mut self, event_loop: &ActiveEventLoop) {
        if !self.ready {
            return;
        }
        loop {
            // Not held while a command runs: a command may make a guest
            // thread ask for more.
            let Some(command) = lock(&self.shared.commands).pop_front() else {
                return;
            };
            match command {
                Command::Create {
                    attributes,
                    sink,
                    reply,
                } => {
                    let _ = reply.send(self.create(event_loop, *attributes, *sink));
                }
                Command::Change { window, change } => self.change(event_loop, window, change),
                Command::Destroy(window) => self.destroy(window),
                Command::Displays(reply) => {
                    let _ = reply.send(self.displays(event_loop));
                }
                #[cfg(feature = "webgpu")]
                Command::Surface { selector, reply } => {
                    let parts = self
                        .windows
                        .values()
                        .find(|entry| entry.selector == selector)
                        .map(|entry| super::surface::SurfaceParts {
                            window: Arc::clone(&entry.window),
                            key: entry.key,
                            size: Arc::clone(&entry.size),
                            seen_said: Arc::clone(&entry.seen_said),
                        });
                    let _ = reply.send(parts);
                }
                #[cfg(feature = "webgpu")]
                Command::Run(work) => work(),
                #[cfg(feature = "webgpu")]
                Command::Unseen { window, said } => {
                    // Believed only while the window system has said nothing
                    // since: what it says is newer, and is what stands.
                    if let Some(entry) = self.entry(window)
                        && entry.seen_said.load(std::sync::atomic::Ordering::Acquire) == said
                    {
                        entry.unseen = true;
                        entry
                            .sink
                            .flags(window_flags::OCCLUDED, window_flags::OCCLUDED);
                    }
                }
                #[cfg(feature = "webgpu")]
                Command::Release(window) => drop(window),
                #[cfg(feature = "pixels")]
                Command::Pixels { window, frame } => {
                    // Taken first: the guest's next frame is then news again,
                    // whatever becomes of this one.
                    let Some(frame) = lock(&frame).take() else {
                        continue;
                    };
                    if self.painter.is_none() {
                        self.painter = super::pixels::Painter::new(event_loop);
                    }
                    let Some(id) = self.keys.get(&window) else {
                        continue;
                    };
                    if let (Some(painter), Some(entry)) = (&self.painter, self.windows.get_mut(id))
                    {
                        painter.show(&entry.window, &mut entry.painted, frame);
                    }
                }
                Command::Exit => event_loop.exit(),
            }
        }
    }

    fn instances(&self) -> Vec<InstanceSink> {
        self.shared.instances().clone()
    }
}

impl ApplicationHandler<()> for App {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if cause == StartCause::Init {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
        self.serve(event_loop);
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let first = !self.ready;
        self.ready = true;
        self.serve(event_loop);
        if !first {
            for instance in self.instances() {
                instance.resumed();
            }
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        self.ready = false;
        #[cfg(target_os = "macos")]
        if let Some(waker) = &self.redraw_waker {
            waker.arm(None);
        }
        for instance in self.instances() {
            instance.suspended();
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, (): ()) {
        self.serve(event_loop);
    }

    fn window_event(&mut self, _event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let monitor = match &event {
            WindowEvent::Moved(_) | WindowEvent::Resized(_) => self
                .windows
                .get(&id)
                .and_then(|entry| entry.window.current_monitor()),
            _ => None,
        };
        let display = monitor.map(|monitor| self.monitor_id(&monitor));
        let Some(entry) = self.windows.get_mut(&id) else {
            return;
        };
        match event {
            WindowEvent::Resized(size) => {
                let scale_factor = entry.window.scale_factor();
                entry.report_state();
                entry.resized(size.width, size.height, scale_factor);
                // AppKit has put the window controls back in their usual
                // place, if it laid the title bar out again. After the size,
                // which the guest lays the title bar out by.
                entry.report_title_bar();
                // The frame on show is stretched to the new size until the
                // guest has drawn one for it.
                #[cfg(feature = "pixels")]
                if let Some(painted) = &mut entry.painted {
                    painted.show();
                }
                if let Some(display) = display {
                    entry.sink.display_changed(display);
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let size = entry.window.inner_size();
                entry.resized(size.width, size.height, scale_factor);
                entry.report_title_bar();
                if super::icon::FOLLOWS_SCALE && entry.icon.is_some() {
                    super::icon::show(&entry.window, entry.icon.as_deref());
                }
            }
            WindowEvent::Moved(_) => {
                entry.report_place();
                if let Some(display) = display {
                    entry.sink.display_changed(display);
                }
            }
            WindowEvent::CloseRequested => {
                entry.sink.close_requested();
                entry.close_asked = Some((Instant::now(), entry.sink.delivered()));
            }
            WindowEvent::Focused(focused) => {
                entry.focused = focused;
                entry.owed_release = false;
                if focused {
                    entry.sink.focused(true);
                    if entry.grab != CursorGrab::None {
                        let _ = entry.apply_grab(CursorGrabReason::FocusGained);
                    }
                    // A user coming back may have copied something elsewhere.
                    // Looking can take a while, so not on this thread.
                    #[cfg(feature = "clipboard")]
                    {
                        let shared = Arc::clone(&self.shared);
                        std::thread::spawn(move || shared.announce_clipboard());
                    }
                } else {
                    if entry.grabbed {
                        let _ = entry.apply_grab(CursorGrabReason::FocusLost);
                    }
                    entry.sink.focused(false);
                    entry.modifiers &=
                        modifiers::CAPS_LOCK | modifiers::NUM_LOCK | modifiers::SCROLL_LOCK;
                }
            }
            WindowEvent::Occluded(occluded) => {
                entry
                    .seen_said
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                entry.sink.flags(
                    window_flags::OCCLUDED,
                    if occluded { window_flags::OCCLUDED } else { 0 },
                );
                entry.unseen = occluded;
                if !occluded {
                    // What was drawn while nobody could see it went nowhere,
                    // and a guest that draws only when asked is waiting.
                    entry.redraw_tick = Some(Instant::now());
                    entry.redraw_due = None;
                    entry.sink.redraw_requested();
                }
            }
            WindowEvent::ThemeChanged(theme) => {
                entry.sink.theme_changed(convert::theme(Some(theme)));
                // An appearance change may lay the title bar out again.
                entry.report_title_bar();
            }
            WindowEvent::RedrawRequested => {
                #[cfg(feature = "pixels")]
                if let Some(painted) = &mut entry.painted {
                    painted.show();
                }
                entry.sink.redraw_requested();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let input = convert::key_input(&event, entry.modifiers);
                if let Some(after) = input.modifiers {
                    entry.modifiers = after;
                }
                entry.sink.key(input);
            }
            WindowEvent::ModifiersChanged(state) => {
                // Locks are not winit's to tell; what is known of them stays.
                let locks = entry.modifiers
                    & (modifiers::CAPS_LOCK | modifiers::NUM_LOCK | modifiers::SCROLL_LOCK);
                entry.modifiers = convert::modifier_bits(&state) | locks;
                entry.sink.modifiers(entry.modifiers);
            }
            WindowEvent::Ime(ime) => match ime {
                Ime::Enabled => entry.sink.text_input_state(true, false, Rect::default()),
                Ime::Disabled => entry.sink.text_input_state(false, false, Rect::default()),
                Ime::Preedit(text, cursor) => entry
                    .sink
                    .text_composition(&text, cursor.map(|(start, end)| (start as u32, end as u32))),
                Ime::Commit(text) => entry.sink.text_commit(&text),
            },
            WindowEvent::CursorMoved { position, .. } => {
                entry.cursor = (position.x, position.y);
                entry.pointer_inside = true;
                entry.hover_edges();
                entry.pointer(PointerPhase::Motion, 0);
            }
            WindowEvent::CursorEntered { .. } => {
                entry.owed_release = false;
                entry.pointer_inside = true;
                entry.pointer(PointerPhase::Enter, 0);
            }
            WindowEvent::CursorLeft { .. } => {
                entry.pointer_inside = false;
                entry.show_edge(None);
                entry.pointer(PointerPhase::Leave, 0);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let primary = button == winit::event::MouseButton::Left;
                match state {
                    // Moving or resizing the window: the guest is not told.
                    ElementState::Pressed if primary && entry.system_press() => {}
                    ElementState::Released if primary && entry.owed_release => {
                        entry.owed_release = false;
                    }
                    _ => entry.pointer(
                        match state {
                            ElementState::Pressed => PointerPhase::Down,
                            ElementState::Released => PointerPhase::Up,
                        },
                        convert::button(button),
                    ),
                }
            }
            WindowEvent::MouseWheel { delta, phase, .. } => {
                entry.sink.wheel(convert::wheel(delta, phase, entry.cursor));
            }
            WindowEvent::Touch(touch) => entry.sink.pointer(PointerInput {
                kind: PointerKind::Touch,
                source: touch.id,
                phase: convert::touch_phase(touch.phase),
                x: touch.location.x,
                y: touch.location.y,
                pressure: touch.force.map(|force| force.normalized() as f32),
                ..PointerInput::default()
            }),
            _ => {}
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device: DeviceId,
        event: DeviceEvent,
    ) {
        // Of what devices do outside any window, only the mouse is passed on,
        // and never keys: a guest is not to watch the keyboard.
        match event {
            DeviceEvent::MouseMotion { delta } => {
                for instance in self.instances() {
                    instance.raw_motion(0, delta.0, delta.1);
                }
                // A locked cursor does not move, so this is all a guest
                // learns of the mouse while it has the lock.
                for entry in self.windows.values_mut() {
                    if entry.focused && entry.grabbed && entry.grab == CursorGrab::Locked {
                        entry.sink.pointer(PointerInput {
                            x: entry.cursor.0,
                            y: entry.cursor.1,
                            delta: Some(delta),
                            ..PointerInput::default()
                        });
                    }
                }
            }
            DeviceEvent::Button { button, state } => {
                for instance in self.instances() {
                    instance.raw_button(0, button + 1, state == ElementState::Pressed);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let mut next: Option<Instant> = None;
        let mut sooner = |deadline: Instant| {
            next = Some(next.map_or(deadline, |other| other.min(deadline)));
        };
        #[cfg(target_os = "macos")]
        let mut redraw_next: Option<Instant> = None;
        // A window that left fullscreen since its title bar was reported:
        // macOS says so only after the last resize of the way out, and
        // lays the title bar out again then.
        for entry in self.windows.values_mut() {
            if entry.title_bar != TitleBarStyle::System
                && entry.window.fullscreen().is_some() != entry.was_fullscreen
            {
                entry.report_state();
                entry.report_title_bar();
            }
        }
        // Redraws that were asked for before the display was ready.
        for entry in self.windows.values_mut() {
            match entry.redraw_due {
                Some(_) if entry.unseen => entry.redraw_due = None,
                Some(due) if now >= due => {
                    entry.redraw_due = None;
                    entry.window.request_redraw();
                }
                Some(due) => {
                    sooner(due);
                    // Shown, and in sight: the redraws of windows out of
                    // sight were let go of above.
                    #[cfg(target_os = "macos")]
                    if self.ready && entry.shown {
                        redraw_next = Some(redraw_next.map_or(due, |other| other.min(due)));
                    }
                }
                None => {}
            }
        }
        #[cfg(target_os = "macos")]
        if let Some(waker) = &self.redraw_waker {
            waker.arm(redraw_next);
        }
        // A window the user asked to close, whose guest has read nothing
        // since, belongs to a guest that is stuck.
        if let CloseBehavior::RequestThenEnd(grace) = self.shared.options.close {
            for entry in self.windows.values_mut() {
                let Some((asked, delivered)) = entry.close_asked else {
                    continue;
                };
                if entry.sink.delivered() != delivered {
                    // The guest has seen it. What it does about it is its own.
                    entry.close_asked = None;
                    continue;
                }
                let deadline = asked + grace;
                if now >= deadline {
                    self.closed_by_user = true;
                    event_loop.exit();
                    return;
                }
                sooner(deadline);
            }
        }
        event_loop.set_control_flow(match next {
            Some(deadline) => ControlFlow::WaitUntil(deadline),
            None => ControlFlow::Wait,
        });
    }
}

/// How long the display the window is on takes for a frame: 60 a second
/// where it does not say.
fn frame_interval(window: &Window) -> Duration {
    let millihertz = window
        .current_monitor()
        .and_then(|monitor| monitor.refresh_rate_millihertz())
        .filter(|rate| *rate > 0)
        .unwrap_or(60_000);
    Duration::from_nanos(1_000_000_000_000 / u64::from(millihertz))
}

/// The refresh of the display a redraw asked for at `now` is for, and
/// whether it has to be waited for. `tick` is the refresh the request before
/// was for: the next one is a frame later, so that a guest which draws
/// whenever it is told and asks again runs at the display's pace, however
/// long each of its frames takes of that. A request that comes later than
/// that is answered at once; one that comes a frame or more late starts the
/// count again from now.
///
/// A guest that draws through a surface which waits for the display itself
/// asks a frame later anyway, and is not held up by this.
fn next_refresh(tick: Option<Instant>, now: Instant, interval: Duration) -> (Instant, bool) {
    let Some(tick) = tick else {
        return (now, false);
    };
    let next = tick + interval;
    if next > now {
        (next, true)
    } else if now - next < interval {
        (next, false)
    } else {
        (now, false)
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        // The window goes with its grab released, and without a word more to
        // a guest that is no longer listening.
        if self.grabbed {
            let _ = self.window.set_cursor_grab(CursorGrabMode::None);
        }
        let _ = (self.key, &self.selector);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redraws_keep_to_the_refreshes_of_the_display() {
        let frame = Duration::from_millis(16);
        let start = Instant::now();
        let at = |milliseconds: u64| start + Duration::from_millis(milliseconds);
        // A window that was never asked to redraw has nothing to wait for.
        assert_eq!(next_refresh(None, at(0), frame), (at(0), false));
        // Asked again while that frame is on show: the next refresh, waited
        // for.
        assert_eq!(next_refresh(Some(at(0)), at(3), frame), (at(16), true));
        // A guest that takes most of a frame to draw keeps to the same
        // refreshes: 16, 32, 48, not 16, 30, 44 or 19, 35, 51.
        assert_eq!(next_refresh(Some(at(16)), at(30), frame), (at(32), true));
        // A little late: answered at once, and still counted from where the
        // refresh was.
        assert_eq!(next_refresh(Some(at(32)), at(50), frame), (at(48), false));
        assert_eq!(next_refresh(Some(at(48)), at(52), frame), (at(64), true));
        // A frame or more late: at once, counting from now.
        assert_eq!(next_refresh(Some(at(64)), at(96), frame), (at(96), false));
        assert_eq!(
            next_refresh(Some(at(96)), at(1000), frame),
            (at(1000), false)
        );
    }
}
