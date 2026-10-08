//! `WGUIWindow`.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};

use super::request::PendingRequest;
use super::shared::WindowShared;
use super::sink::WindowSink;
use super::{Host, Object, ObjectKind, Window, lock, log};
use crate::abi::api::WindowApi;
use crate::abi::enums::*;
use crate::abi::structs::*;
use crate::abi::{AbiResult, Cx, Handle, OutPtr};
use crate::system::{Fullscreen, TextInput, WindowAttributes, WindowChange};

/// The flags `wguiWindowSetFlags` changes only where windows have states.
const STATE_FLAGS: u64 = window_flags::VISIBLE
    | window_flags::RESIZABLE
    | window_flags::DECORATED
    | window_flags::MINIMIZED
    | window_flags::MAXIMIZED
    | window_flags::ALWAYS_ON_TOP
    | window_flags::ALWAYS_ON_BOTTOM;

impl Host {
    /// The window system's id of the display behind a guest's handle. `Ok(None)`
    /// for the null handle: the window system chooses.
    fn display_id(&self, display: Handle) -> Result<Option<u64>, Status> {
        if display == 0 {
            return Ok(None);
        }
        self.display(display)
            .map(|display| Some(display.id))
            .ok_or(Status::InvalidHandle)
    }

    /// `wguiInstanceCreateWindow`, up to handing the handle to the guest.
    pub(super) fn create_window(
        &self,
        instance: Handle,
        descriptor: Option<&WindowDescriptor>,
    ) -> Result<Handle, Status> {
        let state = self.instance(instance).ok_or(Status::InvalidHandle)?;
        let default = WindowDescriptor {
            flags: window_flags::VISIBLE | window_flags::RESIZABLE | window_flags::DECORATED,
            ..WindowDescriptor::default()
        };
        let descriptor = descriptor.unwrap_or(&default);
        if descriptor.flags & !window_flags::REQUESTABLE != 0
            || descriptor.options & !window_options::KNOWN != 0
        {
            return Err(Status::InvalidArgument);
        }
        if !self.has(FeatureName::Windows) {
            return Err(Status::Unsupported);
        }
        if self
            .ctx
            .limits
            .max_windows
            .is_some_and(|max| self.count(ObjectKind::Window) >= max)
        {
            return Err(Status::LimitExceeded);
        }
        let positioned = descriptor.options & window_options::POSITIONED != 0;
        if positioned && !self.has(FeatureName::WindowPosition) {
            return Err(Status::Unsupported);
        }
        let attributes = WindowAttributes {
            title: descriptor.title.clone(),
            target: descriptor.target.clone(),
            application_id: state.application_id.clone(),
            flags: descriptor.flags,
            logical_units: descriptor.options & window_options::LOGICAL_UNITS != 0,
            size: (descriptor.width != 0 && descriptor.height != 0)
                .then_some((descriptor.width, descriptor.height)),
            min_size: (descriptor.min_width, descriptor.min_height),
            max_size: (descriptor.max_width, descriptor.max_height),
            position: positioned.then_some((descriptor.x, descriptor.y)),
            display: self.display_id(descriptor.display)?,
            top_level: self.has(FeatureName::TopLevelWindows),
        };

        // The window has its handle before the window system hears of it:
        // what the window system reports while creating it is already about
        // a window the guest can name.
        let shared = Arc::new(WindowShared::default());
        let window = Arc::new(Window {
            instance: Arc::clone(&state.shared),
            shared: Arc::clone(&shared),
            host: Mutex::new(None),
            selector: OnceLock::new(),
            cursor: Mutex::new(0),
        });
        let handle = self
            .insert(Object::Window(Arc::clone(&window)), instance)
            .ok_or(Status::LimitExceeded)?;
        shared.handle.store(handle, Ordering::Release);
        state.shared.add_window(&shared);

        let system = &self.ctx.system;
        let sink = WindowSink::new(&state.shared, &shared, system.info().shortcut_modifier);
        let host = match system.create_window(attributes, sink) {
            Ok(host) => host,
            Err(status) => {
                self.destroy_window(handle);
                // Never the two statuses that mean something else here.
                return Err(match status {
                    Status::Success | Status::Unimplemented => Status::Error,
                    status => status,
                });
            }
        };
        if shared.snapshot.read().serial == 0 {
            log(|| "the window system created a window without reporting its size".to_owned());
            drop(host);
            self.destroy_window(handle);
            return Err(Status::Error);
        }
        let _ = window.selector.set(host.surface_selector());
        *lock(&window.host) = Some(host);
        Ok(handle)
    }

    pub(super) fn destroy_window(&self, window: Handle) {
        let removed = lock(&self.table).remove(window, ObjectKind::Window);
        if let Some(Object::Window(window)) = removed {
            window.destroy();
        }
    }

    /// The window behind a handle, with the instance it was opened on.
    fn window_and_instance(&self, window: Handle) -> Result<(Arc<Window>, Handle), Status> {
        let state = self.window(window).ok_or(Status::InvalidHandle)?;
        Ok((state, self.instance_of(window)))
    }

    /// Passes a change that needs nothing checked on to the window system.
    fn change(&self, window: Handle, change: WindowChange) -> Status {
        match self.window(window) {
            Some(window) => {
                window.apply(change);
                Status::Success
            }
            None => Status::InvalidHandle,
        }
    }

    /// The same for a change that only some hosts can make.
    fn change_if(&self, feature: FeatureName, window: Handle, change: WindowChange) -> Status {
        if self.window(window).is_none() {
            Status::InvalidHandle
        } else if !self.has(feature) {
            Status::Unsupported
        } else {
            self.change(window, change)
        }
    }
}

impl WindowApi for Host {
    fn window_destroy(&self, _cx: &mut Cx<'_>, window: Handle) {
        self.destroy_window(window);
    }

    fn window_get_state(
        &self,
        cx: &mut Cx<'_>,
        window: Handle,
        state: OutPtr<WindowState>,
    ) -> AbiResult<Status> {
        let (window, instance) = match self.window_and_instance(window) {
            Ok(found) => found,
            Err(status) => return Ok(status),
        };
        let data = window.shared.snapshot.read();
        state.write(
            &cx.mem,
            &WindowState {
                width: data.width,
                height: data.height,
                scale_factor: data.scale_factor,
                x: data.x,
                y: data.y,
                frame_left: data.frame[0],
                frame_top: data.frame[1],
                frame_right: data.frame[2],
                frame_bottom: data.frame[3],
                safe_left: data.safe_area[0],
                safe_top: data.safe_area[1],
                safe_right: data.safe_area[2],
                safe_bottom: data.safe_area[3],
                flags: data.flags,
                display: self.display_handle(instance, &window.instance, data.display),
                fullscreen: data.fullscreen,
                cursor_grab: data.cursor_grab,
                theme: data.theme,
                serial: data.serial,
            },
        )?;
        Ok(Status::Success)
    }

    fn window_get_surface_selector(
        &self,
        cx: &mut Cx<'_>,
        window: Handle,
        buffer: u32,
        capacity: u32,
        length: OutPtr<u32>,
    ) -> AbiResult<Status> {
        let Some(window) = self.window(window) else {
            return Ok(Status::InvalidHandle);
        };
        let selector = window.selector.get().map_or("", String::as_str);
        cx.mem
            .write_truncated(buffer, capacity, length, selector.as_bytes())?;
        Ok(Status::Success)
    }

    fn window_set_title(&self, _cx: &mut Cx<'_>, window: Handle, title: &str) -> AbiResult<Status> {
        Ok(self.change(window, WindowChange::Title(title.to_owned())))
    }

    fn window_set_size(
        &self,
        _cx: &mut Cx<'_>,
        window: Handle,
        width: u32,
        height: u32,
    ) -> AbiResult<Status> {
        if width == 0 || height == 0 {
            return Ok(match self.window(window) {
                Some(_) => Status::InvalidArgument,
                None => Status::InvalidHandle,
            });
        }
        Ok(self.change(window, WindowChange::Size { width, height }))
    }

    fn window_set_size_limits(
        &self,
        _cx: &mut Cx<'_>,
        window: Handle,
        min_width: u32,
        min_height: u32,
        max_width: u32,
        max_height: u32,
    ) -> AbiResult<Status> {
        let crossed = |min: u32, max: u32| max != 0 && min > max;
        if crossed(min_width, max_width) || crossed(min_height, max_height) {
            return Ok(match self.window(window) {
                Some(_) => Status::InvalidArgument,
                None => Status::InvalidHandle,
            });
        }
        Ok(self.change(
            window,
            WindowChange::SizeLimits {
                min: (min_width, min_height),
                max: (max_width, max_height),
            },
        ))
    }

    fn window_set_position(
        &self,
        _cx: &mut Cx<'_>,
        window: Handle,
        x: i32,
        y: i32,
    ) -> AbiResult<Status> {
        Ok(self.change_if(
            FeatureName::WindowPosition,
            window,
            WindowChange::Position { x, y },
        ))
    }

    fn window_set_flags(
        &self,
        cx: &mut Cx<'_>,
        window: Handle,
        mask: u64,
        values: u64,
        unsupported: OutPtr<u64>,
    ) -> AbiResult<Status> {
        let Some(window) = self.window(window) else {
            return Ok(Status::InvalidHandle);
        };
        // What cannot be asked for: flags that are only reported, ones this
        // version does not know, a transparency that is fixed at creation,
        // and states where windows have none.
        let mut refused = mask & (!window_flags::REQUESTABLE | window_flags::TRANSPARENT);
        if !self.has(FeatureName::WindowStates) {
            refused |= mask & STATE_FLAGS;
        }
        unsupported.write_if_present(&cx.mem, &refused)?;
        let mask = mask & !refused;
        if mask != 0 {
            window.apply(WindowChange::Flags {
                mask,
                values: values & mask,
            });
        }
        Ok(Status::Success)
    }

    fn window_set_fullscreen(
        &self,
        _cx: &mut Cx<'_>,
        window: Handle,
        options: Option<&FullscreenOptions>,
        tag: u64,
    ) -> AbiResult<Status> {
        let Some(state) = self.window(window) else {
            return Ok(Status::InvalidHandle);
        };
        let options = match options {
            None => None,
            Some(options) => {
                let feature = match options.mode {
                    FullscreenMode::None => return Ok(Status::InvalidArgument),
                    FullscreenMode::Borderless => FeatureName::FullscreenBorderless,
                    FullscreenMode::Exclusive => FeatureName::FullscreenExclusive,
                };
                if options.flags & !fullscreen_flags::KNOWN != 0 {
                    return Ok(Status::InvalidArgument);
                }
                if !self.has(feature) {
                    return Ok(Status::Unsupported);
                }
                let display = match self.display_id(options.display) {
                    Ok(display) => display,
                    Err(status) => return Ok(status),
                };
                Some(Fullscreen {
                    mode: options.mode,
                    display,
                    no_deferral: options.flags & fullscreen_flags::NO_DEFERRAL != 0,
                })
            }
        };
        if !state.instance.owe_completion() {
            return Ok(Status::LimitExceeded);
        }
        let request = PendingRequest::new(&state.instance, window, RequestKind::Fullscreen, tag);
        state.apply(WindowChange::Fullscreen { options, request });
        Ok(Status::Success)
    }

    fn window_focus(&self, _cx: &mut Cx<'_>, window: Handle) -> AbiResult<Status> {
        Ok(self.change(window, WindowChange::Focus))
    }

    fn window_request_attention(
        &self,
        _cx: &mut Cx<'_>,
        window: Handle,
        level: AttentionLevel,
    ) -> AbiResult<Status> {
        Ok(self.change_if(
            FeatureName::AttentionRequest,
            window,
            WindowChange::Attention(level),
        ))
    }

    fn window_request_redraw(&self, _cx: &mut Cx<'_>, window: Handle) -> AbiResult<Status> {
        Ok(self.change(window, WindowChange::Redraw))
    }

    fn window_set_cursor_shape(
        &self,
        _cx: &mut Cx<'_>,
        window: Handle,
        shape: CursorShape,
    ) -> AbiResult<Status> {
        let Some(state) = self.window(window) else {
            return Ok(Status::InvalidHandle);
        };
        if !self.has(FeatureName::CursorShapes) {
            return Ok(Status::Unsupported);
        }
        *lock(&state.cursor) = 0;
        state.apply(WindowChange::CursorShape(shape));
        Ok(Status::Success)
    }

    fn window_set_custom_cursor(
        &self,
        _cx: &mut Cx<'_>,
        window: Handle,
        cursor: Handle,
    ) -> AbiResult<Status> {
        let Some(state) = self.window(window) else {
            return Ok(Status::InvalidHandle);
        };
        let Some(image) = self.cursor(cursor).map(|cursor| Arc::clone(&cursor.image)) else {
            return Ok(Status::InvalidHandle);
        };
        if !self.has(FeatureName::CustomCursors) {
            return Ok(Status::Unsupported);
        }
        *lock(&state.cursor) = cursor;
        state.apply(WindowChange::CustomCursor(Some(image)));
        Ok(Status::Success)
    }

    fn window_set_cursor_visible(
        &self,
        _cx: &mut Cx<'_>,
        window: Handle,
        visible: bool,
    ) -> AbiResult<Status> {
        Ok(self.change(window, WindowChange::CursorVisible(visible)))
    }

    fn window_set_cursor_grab(
        &self,
        _cx: &mut Cx<'_>,
        window: Handle,
        grab: CursorGrab,
        tag: u64,
    ) -> AbiResult<Status> {
        let Some(state) = self.window(window) else {
            return Ok(Status::InvalidHandle);
        };
        // The host never gives one kind of grab in place of another: a kind
        // it does not have is refused here.
        let available = match grab {
            CursorGrab::None => true,
            CursorGrab::Confined => self.has(FeatureName::CursorConfine),
            CursorGrab::Locked => self.has(FeatureName::CursorLock),
        };
        if !available {
            return Ok(Status::Unsupported);
        }
        if !state.instance.owe_completion() {
            return Ok(Status::LimitExceeded);
        }
        let request = PendingRequest::new(&state.instance, window, RequestKind::CursorGrab, tag);
        state.apply(WindowChange::CursorGrab { grab, request });
        Ok(Status::Success)
    }

    fn window_set_cursor_position(
        &self,
        _cx: &mut Cx<'_>,
        window: Handle,
        x: f64,
        y: f64,
    ) -> AbiResult<Status> {
        if !(x.is_finite() && y.is_finite()) {
            return Ok(match self.window(window) {
                Some(_) => Status::InvalidArgument,
                None => Status::InvalidHandle,
            });
        }
        Ok(self.change_if(
            FeatureName::CursorWarp,
            window,
            WindowChange::CursorPosition { x, y },
        ))
    }

    fn window_set_text_input(
        &self,
        _cx: &mut Cx<'_>,
        window: Handle,
        descriptor: Option<&TextInputDescriptor>,
    ) -> AbiResult<Status> {
        let Some(state) = self.window(window) else {
            return Ok(Status::InvalidHandle);
        };
        if !self.has(FeatureName::TextInput) {
            return Ok(Status::Unsupported);
        }
        if descriptor.is_some_and(|descriptor| descriptor.hints & !text_input_hints::KNOWN != 0) {
            return Ok(Status::InvalidArgument);
        }
        state
            .shared
            .text_input
            .store(descriptor.is_some(), Ordering::Release);
        state.apply(WindowChange::TextInput(descriptor.map(|descriptor| {
            TextInput {
                purpose: descriptor.purpose,
                hints: descriptor.hints,
                screen_keyboard: descriptor.screen_keyboard,
                cursor_area: descriptor.cursor_area.clone(),
                input_area: descriptor.input_area.clone(),
            }
        })));
        Ok(Status::Success)
    }

    fn window_reset_text_input(&self, _cx: &mut Cx<'_>, window: Handle) -> AbiResult<Status> {
        Ok(self.change_if(FeatureName::TextInput, window, WindowChange::ResetTextInput))
    }

    fn window_set_default_actions(
        &self,
        _cx: &mut Cx<'_>,
        window: Handle,
        prevented: u32,
    ) -> AbiResult<Status> {
        if prevented & !default_actions::KNOWN != 0 {
            return Ok(match self.window(window) {
                Some(_) => Status::InvalidArgument,
                None => Status::InvalidHandle,
            });
        }
        Ok(self.change(window, WindowChange::DefaultActions(prevented)))
    }

    fn window_present_pixels(
        &self,
        cx: &mut Cx<'_>,
        window: Handle,
        frame: &PixelFrame,
        damage: &[Rect],
    ) -> AbiResult<Status> {
        if self.window(window).is_none() {
            return Ok(Status::InvalidHandle);
        }
        if !self.has(FeatureName::PixelPresent) {
            return Ok(Status::Unsupported);
        }
        let (width, height) = (u64::from(frame.width), u64::from(frame.height));
        let row = width * 4;
        let stride = match frame.stride {
            0 => row,
            stride => u64::from(stride),
        };
        if width == 0
            || height == 0
            || width > MAX_FRAME_EDGE
            || height > MAX_FRAME_EDGE
            || stride < row
            || stride > u64::from(u32::MAX)
        {
            return Ok(Status::InvalidArgument);
        }
        // What is read of the guest's memory: every row but the last in
        // full, and of the last one only its pixels.
        let needed = stride * (height - 1) + row;
        if needed > u64::from(frame.pixels_size) || frame.pixels == 0 {
            return Ok(Status::InvalidArgument);
        }
        if self
            .ctx
            .limits
            .max_frame_bytes
            .is_some_and(|max| needed > max as u64)
        {
            return Ok(Status::LimitExceeded);
        }
        // A rectangle that reaches outside the frame is cut to it; one that
        // lies outside altogether says nothing.
        let mut inside = Vec::with_capacity(damage.len());
        for rect in damage {
            let (left, top) = (i64::from(rect.x).max(0), i64::from(rect.y).max(0));
            let right = (i64::from(rect.x) + i64::from(rect.width)).min(width as i64);
            let bottom = (i64::from(rect.y) + i64::from(rect.height)).min(height as i64);
            if right > left && bottom > top {
                inside.push(Rect {
                    x: left as i32,
                    y: top as i32,
                    width: (right - left) as u32,
                    height: (bottom - top) as u32,
                });
            }
        }
        if !damage.is_empty() && inside.is_empty() {
            // Nothing of the window changed: there is nothing to show.
            return Ok(Status::Success);
        }
        let pixels = cx.mem.read_vec(frame.pixels, needed)?;
        Ok(self.change(
            window,
            WindowChange::Pixels(crate::system::PixelFrame {
                width: frame.width,
                height: frame.height,
                stride: stride as u32,
                format: frame.format,
                alpha: frame.alpha,
                pixels,
                damage: inside,
            }),
        ))
    }
}

/// The longest edge a frame may have, whatever the limit on its bytes.
const MAX_FRAME_EDGE: u64 = 32_768;
