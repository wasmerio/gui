use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use wasmer_gui_sys::*;

use crate::cursor::Cursor;
use crate::dpi::{PhysicalPosition, PhysicalSize, Position, Size};
use crate::error::{ExternalError, NotSupportedError, OsError as RootOsError};
use crate::icon::Icon;
use crate::platform_impl::Fullscreen;
use crate::window::{
    CursorGrabMode, CursorIcon, ImePurpose, ResizeDirection, Theme, UserAttentionType,
    WindowAttributes, WindowButtons, WindowLevel,
};

use super::event_loop::Shared;
use super::{events, has, ActiveEventLoop, MonitorHandle, OsError, WindowId};

/// What the event loop and a window's handle both know of the window.
pub(crate) struct WindowShared {
    pub(crate) handle: WGUIWindow,
    pub(crate) state: Mutex<WindowState>,
}

pub(crate) struct WindowState {
    /// The scale factor the program was last told of.
    pub(crate) scale_factor: f64,
    /// The cursor is locked: the mouse says how far it moved, not where it
    /// is.
    pub(crate) locked: bool,
    /// An input method is in the middle of composing text.
    pub(crate) composing: bool,
    /// The program has been told that text entry is on.
    pub(crate) ime_enabled: bool,
    /// Where the program was last told the cursor is.
    pub(crate) cursor: Option<(f64, f64)>,
    // What the program said of the window, which the host is not asked
    // back for.
    title: String,
    min_inner_size: Option<Size>,
    max_inner_size: Option<Size>,
    ime_allowed: bool,
    ime_purpose: ImePurpose,
    ime_cursor_area: Option<(Position, Size)>,
}

/// A window. Every method can be called from any thread: the host's handles
/// work on all of them, and what it knows of a window it answers from what
/// it last saw, without waiting for the window system.
pub struct Window {
    shared: Arc<Shared>,
    window: Arc<WindowShared>,
}

fn host_state(handle: WGUIWindow) -> WGUIWindowState {
    let mut state = WGUIWindowState::INIT;
    // A window that is gone leaves this as it is: no size, no flags.
    unsafe { wguiWindowGetState(handle, &mut state) };
    state
}

fn scale_of(state: &WGUIWindowState) -> f64 {
    if state.scaleFactor > 0.0 {
        state.scaleFactor
    } else {
        1.0
    }
}

fn cursor_shape(icon: CursorIcon) -> WGUICursorShape {
    match icon {
        CursorIcon::Default => WGUICursorShape_Default,
        CursorIcon::ContextMenu => WGUICursorShape_ContextMenu,
        CursorIcon::Help => WGUICursorShape_Help,
        CursorIcon::Pointer => WGUICursorShape_Pointer,
        CursorIcon::Progress => WGUICursorShape_Progress,
        CursorIcon::Wait => WGUICursorShape_Wait,
        CursorIcon::Cell => WGUICursorShape_Cell,
        CursorIcon::Crosshair => WGUICursorShape_Crosshair,
        CursorIcon::Text => WGUICursorShape_Text,
        CursorIcon::VerticalText => WGUICursorShape_VerticalText,
        CursorIcon::Alias => WGUICursorShape_Alias,
        CursorIcon::Copy => WGUICursorShape_Copy,
        CursorIcon::Move => WGUICursorShape_Move,
        CursorIcon::NoDrop => WGUICursorShape_NoDrop,
        CursorIcon::NotAllowed => WGUICursorShape_NotAllowed,
        CursorIcon::Grab => WGUICursorShape_Grab,
        CursorIcon::Grabbing => WGUICursorShape_Grabbing,
        CursorIcon::EResize => WGUICursorShape_EResize,
        CursorIcon::NResize => WGUICursorShape_NResize,
        CursorIcon::NeResize => WGUICursorShape_NeResize,
        CursorIcon::NwResize => WGUICursorShape_NwResize,
        CursorIcon::SResize => WGUICursorShape_SResize,
        CursorIcon::SeResize => WGUICursorShape_SeResize,
        CursorIcon::SwResize => WGUICursorShape_SwResize,
        CursorIcon::WResize => WGUICursorShape_WResize,
        CursorIcon::EwResize => WGUICursorShape_EwResize,
        CursorIcon::NsResize => WGUICursorShape_NsResize,
        CursorIcon::NeswResize => WGUICursorShape_NeswResize,
        CursorIcon::NwseResize => WGUICursorShape_NwseResize,
        CursorIcon::ColResize => WGUICursorShape_ColResize,
        CursorIcon::RowResize => WGUICursorShape_RowResize,
        CursorIcon::AllScroll => WGUICursorShape_AllScroll,
        CursorIcon::ZoomIn => WGUICursorShape_ZoomIn,
        CursorIcon::ZoomOut => WGUICursorShape_ZoomOut,
        // The nearest of the shapes the host has names for.
        CursorIcon::AllResize => WGUICursorShape_Move,
        _ => WGUICursorShape_Default,
    }
}

impl Window {
    pub(crate) fn new(
        event_loop: &ActiveEventLoop,
        attributes: WindowAttributes,
    ) -> Result<Self, RootOsError> {
        let shared = event_loop.shared.clone();

        let mut descriptor = WGUIWindowDescriptor::INIT;
        descriptor.title = WGUIStringView::new(&attributes.title);
        if let Some(target) = &attributes.platform_specific.target {
            descriptor.target = WGUIStringView::new(target);
        }
        descriptor.flags = 0;
        for (wanted, flag) in [
            (attributes.visible, WGUIWindowFlags_Visible),
            (attributes.resizable, WGUIWindowFlags_Resizable),
            (attributes.decorations, WGUIWindowFlags_Decorated),
            (attributes.maximized, WGUIWindowFlags_Maximized),
            (attributes.transparent, WGUIWindowFlags_Transparent),
            (attributes.window_level == WindowLevel::AlwaysOnTop, WGUIWindowFlags_AlwaysOnTop),
            (attributes.window_level == WindowLevel::AlwaysOnBottom, WGUIWindowFlags_AlwaysOnBottom),
        ] {
            if wanted {
                descriptor.flags |= flag;
            }
        }
        // A size in logical units can only be said here: how many pixels
        // one is, is not known before the window exists. Without a size the
        // host chooses one.
        match attributes.inner_size {
            Some(Size::Logical(size)) => {
                descriptor.options |= WGUIWindowOptions_LogicalUnits;
                descriptor.width = size.width.round().max(1.0) as u32;
                descriptor.height = size.height.round().max(1.0) as u32;
            },
            Some(Size::Physical(size)) => {
                descriptor.width = size.width.max(1);
                descriptor.height = size.height.max(1);
            },
            None => {},
        }
        // A position goes along when it is in the same units; one that is
        // not is asked for once the window is there.
        let logical = descriptor.options & WGUIWindowOptions_LogicalUnits != 0;
        let mut position_later = None;
        if has(WGUIFeatureName_WindowPosition) {
            match attributes.position {
                Some(Position::Logical(position)) if logical || attributes.inner_size.is_none() => {
                    descriptor.options |= WGUIWindowOptions_LogicalUnits | WGUIWindowOptions_Positioned;
                    descriptor.x = position.x.round() as i32;
                    descriptor.y = position.y.round() as i32;
                },
                Some(Position::Physical(position)) if !logical => {
                    descriptor.options |= WGUIWindowOptions_Positioned;
                    descriptor.x = position.x;
                    descriptor.y = position.y;
                },
                other => position_later = other,
            }
        }
        if let Some(crate::window::Fullscreen::Borderless(Some(monitor))) = &attributes.fullscreen {
            descriptor.display = monitor.inner.0;
        }

        let mut handle = 0;
        let status =
            unsafe { wguiInstanceCreateWindow(shared.instance, &descriptor, &mut handle) };
        if status != WGUIStatus_Success {
            return Err(os_error!(OsError::Host { call: "wguiInstanceCreateWindow", status }));
        }

        let window = Arc::new(WindowShared {
            handle,
            state: Mutex::new(WindowState {
                scale_factor: scale_of(&host_state(handle)),
                locked: false,
                composing: false,
                ime_enabled: false,
                cursor: None,
                title: attributes.title.clone(),
                min_inner_size: attributes.min_inner_size,
                max_inner_size: attributes.max_inner_size,
                ime_allowed: false,
                ime_purpose: ImePurpose::Normal,
                ime_cursor_area: None,
            }),
        });
        shared.windows.lock().unwrap().insert(handle, window.clone());
        let this = Self { shared, window };

        // The rest of what was asked, now that there is a window to ask it
        // of.
        if attributes.min_inner_size.is_some() || attributes.max_inner_size.is_some() {
            this.apply_size_limits();
        }
        if let Some(position) = position_later {
            this.set_outer_position(position);
        }
        if let Some(fullscreen) = attributes.fullscreen {
            this.set_fullscreen(Some(fullscreen.into()));
        }
        this.set_cursor(attributes.cursor);
        Ok(this)
    }

    pub(crate) fn maybe_queue_on_main(&self, f: impl FnOnce(&Self) + Send + 'static) {
        f(self)
    }

    pub(crate) fn maybe_wait_on_main<R: Send>(&self, f: impl FnOnce(&Self) -> R + Send) -> R {
        f(self)
    }

    fn handle(&self) -> WGUIWindow {
        self.window.handle
    }

    fn host(&self) -> WGUIWindowState {
        host_state(self.handle())
    }

    /// Asks the host for flags. What it will not do it leaves, and the
    /// window's state says what the window is.
    fn set_flags(&self, mask: WGUIWindowFlags, on: bool) {
        let mut unsupported = 0;
        unsafe {
            wguiWindowSetFlags(self.handle(), mask, if on { mask } else { 0 }, &mut unsupported)
        };
    }

    // --- Identity, scale, redraw -------------------------------------------------------------

    #[inline]
    pub fn id(&self) -> WindowId {
        WindowId(self.handle())
    }

    #[inline]
    pub fn scale_factor(&self) -> f64 {
        scale_of(&self.host())
    }

    /// The host says when the display is ready for the window's next frame:
    /// once per refresh, and not while the window cannot be seen.
    #[inline]
    pub fn request_redraw(&self) {
        unsafe { wguiWindowRequestRedraw(self.handle()) };
    }

    #[inline]
    pub fn pre_present_notify(&self) {}

    #[inline]
    pub fn reset_dead_keys(&self) {}

    /// The name to draw on this window with WebGPU.
    pub(crate) fn surface_selector(&self) -> String {
        let mut name = [0u8; 256];
        let mut length = 0;
        let status = unsafe {
            wguiWindowGetSurfaceSelector(self.handle(), name.as_mut_ptr(), name.len(), &mut length)
        };
        if status != WGUIStatus_Success {
            return String::new();
        }
        String::from_utf8_lossy(&name[..length.min(name.len())]).into_owned()
    }

    // --- Size and position -------------------------------------------------------------------

    #[inline]
    pub fn inner_position(&self) -> Result<PhysicalPosition<i32>, NotSupportedError> {
        let host = self.host();
        if host.x == WGUI_POSITION_UNDEFINED || host.y == WGUI_POSITION_UNDEFINED {
            // Where windows are is not for programs to know on this host.
            return Err(NotSupportedError::new());
        }
        Ok(PhysicalPosition::new(host.x, host.y))
    }

    #[inline]
    pub fn outer_position(&self) -> Result<PhysicalPosition<i32>, NotSupportedError> {
        let host = self.host();
        let inner = self.inner_position()?;
        Ok(PhysicalPosition::new(inner.x - host.frameLeft as i32, inner.y - host.frameTop as i32))
    }

    #[inline]
    pub fn set_outer_position(&self, position: Position) {
        if !has(WGUIFeatureName_WindowPosition) {
            return;
        }
        let host = self.host();
        let outer = position.to_physical::<i32>(scale_of(&host));
        // The host places a window by its content.
        unsafe {
            wguiWindowSetPosition(
                self.handle(),
                outer.x + host.frameLeft as i32,
                outer.y + host.frameTop as i32,
            )
        };
    }

    #[inline]
    pub fn inner_size(&self) -> PhysicalSize<u32> {
        let host = self.host();
        PhysicalSize::new(host.width, host.height)
    }

    #[inline]
    pub fn outer_size(&self) -> PhysicalSize<u32> {
        let host = self.host();
        PhysicalSize::new(
            host.width + host.frameLeft + host.frameRight,
            host.height + host.frameTop + host.frameBottom,
        )
    }

    /// The host is asked, and what came of it arrives as `Resized`. Where
    /// the embedder decides how big a window is, the size stays.
    #[inline]
    pub fn request_inner_size(&self, size: Size) -> Option<PhysicalSize<u32>> {
        let host = self.host();
        let size = size.to_physical::<u32>(scale_of(&host));
        let status = unsafe { wguiWindowSetSize(self.handle(), size.width, size.height) };
        if status == WGUIStatus_Success {
            None
        } else {
            Some(PhysicalSize::new(host.width, host.height))
        }
    }

    fn apply_size_limits(&self) {
        let scale = self.scale_factor();
        let (min, max) = {
            let state = self.window.state.lock().unwrap();
            (state.min_inner_size, state.max_inner_size)
        };
        // 0 is no limit.
        let min = min.map(|size| size.to_physical::<u32>(scale)).unwrap_or_default();
        let max = max.map(|size| size.to_physical::<u32>(scale)).unwrap_or_default();
        unsafe {
            wguiWindowSetSizeLimits(self.handle(), min.width, min.height, max.width, max.height)
        };
    }

    #[inline]
    pub fn set_min_inner_size(&self, dimensions: Option<Size>) {
        self.window.state.lock().unwrap().min_inner_size = dimensions;
        self.apply_size_limits();
    }

    #[inline]
    pub fn set_max_inner_size(&self, dimensions: Option<Size>) {
        self.window.state.lock().unwrap().max_inner_size = dimensions;
        self.apply_size_limits();
    }

    #[inline]
    pub fn resize_increments(&self) -> Option<PhysicalSize<u32>> {
        None
    }

    #[inline]
    pub fn set_resize_increments(&self, _increments: Option<Size>) {}

    // --- Window state ------------------------------------------------------------------------

    #[inline]
    pub fn set_title(&self, title: &str) {
        self.window.state.lock().unwrap().title = title.to_owned();
        unsafe { wguiWindowSetTitle(self.handle(), title.as_ptr(), title.len()) };
    }

    #[inline]
    pub fn title(&self) -> String {
        self.window.state.lock().unwrap().title.clone()
    }

    #[inline]
    pub fn set_transparent(&self, transparent: bool) {
        self.set_flags(WGUIWindowFlags_Transparent, transparent);
    }

    #[inline]
    pub fn set_blur(&self, _blur: bool) {}

    #[inline]
    pub fn set_visible(&self, visible: bool) {
        self.set_flags(WGUIWindowFlags_Visible, visible);
    }

    #[inline]
    pub fn is_visible(&self) -> Option<bool> {
        Some(self.host().flags & WGUIWindowFlags_Visible != 0)
    }

    #[inline]
    pub fn set_resizable(&self, resizable: bool) {
        self.set_flags(WGUIWindowFlags_Resizable, resizable);
    }

    #[inline]
    pub fn is_resizable(&self) -> bool {
        self.host().flags & WGUIWindowFlags_Resizable != 0
    }

    #[inline]
    pub fn set_enabled_buttons(&self, _buttons: WindowButtons) {}

    #[inline]
    pub fn enabled_buttons(&self) -> WindowButtons {
        WindowButtons::all()
    }

    #[inline]
    pub fn set_minimized(&self, minimized: bool) {
        self.set_flags(WGUIWindowFlags_Minimized, minimized);
    }

    #[inline]
    pub fn is_minimized(&self) -> Option<bool> {
        Some(self.host().flags & WGUIWindowFlags_Minimized != 0)
    }

    #[inline]
    pub fn set_maximized(&self, maximized: bool) {
        self.set_flags(WGUIWindowFlags_Maximized, maximized);
    }

    #[inline]
    pub fn is_maximized(&self) -> bool {
        self.host().flags & WGUIWindowFlags_Maximized != 0
    }

    #[inline]
    pub fn set_decorations(&self, decorations: bool) {
        self.set_flags(WGUIWindowFlags_Decorated, decorations);
    }

    #[inline]
    pub fn is_decorated(&self) -> bool {
        self.host().flags & WGUIWindowFlags_Decorated != 0
    }

    #[inline]
    pub fn set_window_level(&self, level: WindowLevel) {
        let mask = WGUIWindowFlags_AlwaysOnTop | WGUIWindowFlags_AlwaysOnBottom;
        let values = match level {
            WindowLevel::AlwaysOnTop => WGUIWindowFlags_AlwaysOnTop,
            WindowLevel::AlwaysOnBottom => WGUIWindowFlags_AlwaysOnBottom,
            WindowLevel::Normal => 0,
        };
        let mut unsupported = 0;
        unsafe { wguiWindowSetFlags(self.handle(), mask, values, &mut unsupported) };
    }

    #[inline]
    pub fn set_window_icon(&self, _window_icon: Option<Icon>) {}

    #[inline]
    pub fn focus_window(&self) {
        unsafe { wguiWindowFocus(self.handle()) };
    }

    #[inline]
    pub fn has_focus(&self) -> bool {
        self.host().flags & WGUIWindowFlags_Focused != 0
    }

    #[inline]
    pub fn request_user_attention(&self, request_type: Option<UserAttentionType>) {
        if !has(WGUIFeatureName_AttentionRequest) {
            return;
        }
        let level = match request_type {
            None => WGUIAttentionLevel_None,
            Some(UserAttentionType::Informational) => WGUIAttentionLevel_Informational,
            Some(UserAttentionType::Critical) => WGUIAttentionLevel_Critical,
        };
        unsafe { wguiWindowRequestAttention(self.handle(), level) };
    }

    /// A window's theme is the system's: there is none to set.
    #[inline]
    pub fn set_theme(&self, _theme: Option<Theme>) {}

    #[inline]
    pub fn theme(&self) -> Option<Theme> {
        events::theme(self.host().theme)
    }

    #[inline]
    pub fn set_content_protected(&self, _protected: bool) {}

    // --- Fullscreen --------------------------------------------------------------------------

    #[inline]
    pub(crate) fn fullscreen(&self) -> Option<Fullscreen> {
        let host = self.host();
        (host.fullscreen != WGUIFullscreenMode_None)
            .then(|| Fullscreen::Borderless((host.display != 0).then_some(MonitorHandle(host.display))))
    }

    /// Asked of the host, which says when it has happened: a browser waits
    /// for something the user does first. The host's fullscreen is the
    /// borderless kind; a video mode is asked for as its display.
    #[inline]
    pub(crate) fn set_fullscreen(&self, fullscreen: Option<Fullscreen>) {
        if !has(WGUIFeatureName_FullscreenBorderless) {
            return;
        }
        let tag = self.shared.next_tag();
        match fullscreen {
            None => unsafe {
                wguiWindowSetFullscreen(self.handle(), core::ptr::null(), tag);
            },
            Some(fullscreen) => {
                let display = match fullscreen {
                    Fullscreen::Borderless(monitor) => monitor.map_or(0, |monitor| monitor.0),
                    Fullscreen::Exclusive(mode) => mode.monitor().0,
                };
                let options = WGUIFullscreenOptions { display, ..WGUIFullscreenOptions::INIT };
                unsafe { wguiWindowSetFullscreen(self.handle(), &options, tag) };
            },
        }
    }

    // --- Cursor ------------------------------------------------------------------------------

    #[inline]
    pub fn set_cursor(&self, cursor: Cursor) {
        match cursor {
            Cursor::Icon(icon) => unsafe {
                wguiWindowSetCursorShape(self.handle(), cursor_shape(icon));
            },
            Cursor::Custom(custom) => match custom.inner.0.handle {
                // An image the host would not take shows as the default
                // cursor.
                0 => unsafe {
                    wguiWindowSetCursorShape(self.handle(), WGUICursorShape_Default);
                },
                image => unsafe {
                    wguiWindowSetCustomCursor(self.handle(), image);
                },
            },
        }
    }

    #[inline]
    pub fn set_cursor_position(&self, position: Position) -> Result<(), ExternalError> {
        if !has(WGUIFeatureName_CursorWarp) {
            return Err(ExternalError::NotSupported(NotSupportedError::new()));
        }
        let position = position.to_physical::<f64>(self.scale_factor());
        let status = unsafe { wguiWindowSetCursorPosition(self.handle(), position.x, position.y) };
        if status == WGUIStatus_Success {
            Ok(())
        } else {
            Err(ExternalError::Os(os_error!(OsError::Host {
                call: "wguiWindowSetCursorPosition",
                status
            })))
        }
    }

    /// A grab is lent, not owned: the host says when it is given, and the
    /// user or the system can end it. A kind of grab the host does not have
    /// is refused here, and never given as another.
    #[inline]
    pub fn set_cursor_grab(&self, mode: CursorGrabMode) -> Result<(), ExternalError> {
        let (grab, feature) = match mode {
            CursorGrabMode::None => (WGUICursorGrab_None, None),
            CursorGrabMode::Confined => (WGUICursorGrab_Confined, Some(WGUIFeatureName_CursorConfine)),
            CursorGrabMode::Locked => (WGUICursorGrab_Locked, Some(WGUIFeatureName_CursorLock)),
        };
        if feature.is_some_and(|feature| !has(feature)) {
            return Err(ExternalError::NotSupported(NotSupportedError::new()));
        }
        let status =
            unsafe { wguiWindowSetCursorGrab(self.handle(), grab, self.shared.next_tag()) };
        match status {
            WGUIStatus_Success => Ok(()),
            WGUIStatus_Unsupported | WGUIStatus_Unimplemented => {
                Err(ExternalError::NotSupported(NotSupportedError::new()))
            },
            status => Err(ExternalError::Os(os_error!(OsError::Host {
                call: "wguiWindowSetCursorGrab",
                status
            }))),
        }
    }

    #[inline]
    pub fn set_cursor_visible(&self, visible: bool) {
        unsafe { wguiWindowSetCursorVisible(self.handle(), visible.into()) };
    }

    #[inline]
    pub fn set_cursor_hittest(&self, _hittest: bool) -> Result<(), ExternalError> {
        Err(ExternalError::NotSupported(NotSupportedError::new()))
    }

    #[inline]
    pub fn drag_window(&self) -> Result<(), ExternalError> {
        Err(ExternalError::NotSupported(NotSupportedError::new()))
    }

    #[inline]
    pub fn drag_resize_window(&self, _direction: ResizeDirection) -> Result<(), ExternalError> {
        Err(ExternalError::NotSupported(NotSupportedError::new()))
    }

    #[inline]
    pub fn show_window_menu(&self, _position: Position) {}

    // --- IME ---------------------------------------------------------------------------------

    /// Tells the host of the text entry the program wants: where its caret
    /// is and what is typed there, or that there is none.
    fn describe_text_input(&self) {
        let scale = self.scale_factor();
        let state = self.window.state.lock().unwrap();
        if !state.ime_allowed {
            unsafe { wguiWindowSetTextInput(self.handle(), core::ptr::null()) };
            return;
        }
        let mut descriptor = WGUITextInputDescriptor::INIT;
        descriptor.purpose = match state.ime_purpose {
            ImePurpose::Password => WGUITextInputPurpose_Password,
            ImePurpose::Terminal => WGUITextInputPurpose_Terminal,
            _ => WGUITextInputPurpose_Normal,
        };
        if state.ime_purpose == ImePurpose::Password {
            descriptor.hints |= WGUITextInputHints_SensitiveData | WGUITextInputHints_HiddenText;
        }
        if let Some((position, size)) = state.ime_cursor_area {
            let position = position.to_physical::<i32>(scale);
            let size = size.to_physical::<u32>(scale);
            descriptor.cursorArea =
                WGUIRect { x: position.x, y: position.y, width: size.width, height: size.height };
        }
        unsafe { wguiWindowSetTextInput(self.handle(), &descriptor) };
    }

    #[inline]
    pub fn set_ime_allowed(&self, allowed: bool) {
        self.window.state.lock().unwrap().ime_allowed = allowed;
        self.describe_text_input();
    }

    #[inline]
    pub fn set_ime_cursor_area(&self, position: Position, size: Size) {
        let allowed = {
            let mut state = self.window.state.lock().unwrap();
            state.ime_cursor_area = Some((position, size));
            state.ime_allowed
        };
        if allowed {
            self.describe_text_input();
        }
    }

    #[inline]
    pub fn set_ime_purpose(&self, purpose: ImePurpose) {
        let allowed = {
            let mut state = self.window.state.lock().unwrap();
            state.ime_purpose = purpose;
            state.ime_allowed
        };
        if allowed {
            self.describe_text_input();
        }
    }

    // --- Monitors ----------------------------------------------------------------------------

    #[inline]
    pub fn current_monitor(&self) -> Option<MonitorHandle> {
        match self.host().display {
            0 => None,
            display => Some(MonitorHandle(display)),
        }
    }

    #[inline]
    pub fn available_monitors(&self) -> VecDeque<MonitorHandle> {
        self.shared.monitors()
    }

    #[inline]
    pub fn primary_monitor(&self) -> Option<MonitorHandle> {
        self.shared.primary_monitor()
    }

    // --- raw-window-handle -------------------------------------------------------------------
    //
    // raw-window-handle has no handles for WASIX. The web's are borrowed: a
    // window that is a number, which here is the host's handle of the window
    // (a `WGUIWindow`), and a display that is nothing.

    #[cfg(feature = "rwh_04")]
    #[inline]
    pub fn raw_window_handle_rwh_04(&self) -> rwh_04::RawWindowHandle {
        let mut window_handle = rwh_04::WebHandle::empty();
        window_handle.id = self.handle();
        rwh_04::RawWindowHandle::Web(window_handle)
    }

    #[cfg(feature = "rwh_05")]
    #[inline]
    pub fn raw_window_handle_rwh_05(&self) -> rwh_05::RawWindowHandle {
        let mut window_handle = rwh_05::WebWindowHandle::empty();
        window_handle.id = self.handle();
        rwh_05::RawWindowHandle::Web(window_handle)
    }

    #[cfg(feature = "rwh_05")]
    #[inline]
    pub fn raw_display_handle_rwh_05(&self) -> rwh_05::RawDisplayHandle {
        rwh_05::RawDisplayHandle::Web(rwh_05::WebDisplayHandle::empty())
    }

    #[cfg(feature = "rwh_06")]
    #[inline]
    pub fn raw_window_handle_rwh_06(&self) -> Result<rwh_06::RawWindowHandle, rwh_06::HandleError> {
        Ok(rwh_06::RawWindowHandle::Web(rwh_06::WebWindowHandle::new(self.handle())))
    }

    #[cfg(feature = "rwh_06")]
    #[inline]
    pub fn raw_display_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
        Ok(rwh_06::RawDisplayHandle::Web(rwh_06::WebDisplayHandle::new()))
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        self.shared.windows.lock().unwrap().remove(&self.handle());
        unsafe { wguiWindowDestroy(self.handle()) };
        // The loop owes the program a `Destroyed` for it, and may be asleep.
        self.shared.destroyed.lock().unwrap().push(self.handle());
        self.shared.wake();
    }
}
