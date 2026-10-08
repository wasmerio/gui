//! WASIX: windows and input from the Wasmer runtime, through `<wasmer/gui.h>`
//! (the WebAssembly import module `wasmer_gui_v0`, declared for Rust by the
//! `wasmer-gui-sys` crate). The same programs run in native windows, in a
//! browser's canvases, and on hosts where nobody sees them.
//!
//! Selected by the `wasix_platform` cfg alias, which is
//! `all(target_os = "wasi", target_vendor = "wasmer")`.
//!
//! The host never calls into the program: what happens arrives as records
//! the event loop reads, and it sleeps in the host until there are any.
//! Handles are numbers that work on every thread, and the host answers what
//! a window is like from what it last saw of it, so nothing here needs the
//! loop's thread: a `Window` calls the host from whichever thread it is on.
//!
//! What a host cannot do it says, and so does this backend: positions where
//! windows have none, more than one window in a page that gave one canvas.

#![cfg(wasix_platform)]
// The host's API keeps the names its C header has.
#![allow(non_upper_case_globals)]

use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use smol_str::SmolStr;
use wasmer_gui_sys::*;

use crate::dpi::{PhysicalPosition, PhysicalSize};
use crate::keyboard::Key;

pub(crate) use self::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy, OwnedDisplayHandle};
mod event_loop;
mod events;
mod keys;
mod signals;

pub(crate) use self::window::Window;
mod window;

pub(crate) use crate::cursor::OnlyCursorImageSource as PlatformCustomCursorSource;
pub(crate) use crate::icon::NoIcon as PlatformIcon;

#[derive(Default, Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PlatformSpecificEventLoopAttributes {}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PlatformSpecificWindowAttributes {
    /// The target the window adopts: see `WindowAttributesExtWasix`.
    pub(crate) target: Option<String>,
}

/// The host's handle of the window.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WindowId(pub(crate) WGUIWindow);

impl WindowId {
    pub const fn dummy() -> Self {
        // 0 is never a handle.
        WindowId(0)
    }
}

impl From<WindowId> for u64 {
    fn from(id: WindowId) -> Self {
        id.0 as u64
    }
}

impl From<u64> for WindowId {
    fn from(raw_id: u64) -> Self {
        Self(raw_id as u32)
    }
}

/// Which piece of hardware an event is from, where the host can tell them
/// apart; 0 otherwise.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DeviceId(pub(crate) u32);

impl DeviceId {
    pub const fn dummy() -> Self {
        DeviceId(0)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct KeyEventExtra {
    pub key_without_modifiers: Key,
    pub text_with_all_modifiers: Option<SmolStr>,
}

/// The host's answer to a call, when that was not "done".
#[derive(Clone, Debug)]
pub enum OsError {
    Host { call: &'static str, status: WGUIStatus },
}

impl fmt::Display for OsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OsError::Host { call, status } => {
                let why = match *status {
                    WGUIStatus_Unimplemented => "the host does not have this call",
                    WGUIStatus_Unsupported => "the host cannot do this",
                    WGUIStatus_InvalidArgument => "the host would not take what it was given",
                    WGUIStatus_InvalidHandle => "the handle is not one the host knows",
                    WGUIStatus_Denied => "the program is not allowed to",
                    WGUIStatus_LimitExceeded => "the program has as many as it may have",
                    WGUIStatus_NotFound => "the host has nothing by that name",
                    WGUIStatus_Busy => "it is in use",
                    WGUIStatus_Lost => "it is gone",
                    _ => "the host failed",
                };
                write!(f, "{call}: {why} (status {status})")
            },
        }
    }
}

/// An image the host shows as a cursor. Made once, when the cursor is
/// created, and given back when the last copy of it goes.
#[derive(Clone, Debug)]
pub(crate) struct PlatformCustomCursor(pub(crate) Arc<HostCursor>);

#[derive(Debug)]
pub(crate) struct HostCursor {
    /// 0 when the host would not make one: such a cursor shows as the
    /// default one.
    pub(crate) handle: WGUICursor,
}

impl Drop for HostCursor {
    fn drop(&mut self) {
        if self.handle != 0 {
            unsafe { wguiCursorDestroy(self.handle) };
        }
    }
}

impl Hash for PlatformCustomCursor {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
    }
}

impl PartialEq for PlatformCustomCursor {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for PlatformCustomCursor {}

/// One of the host's displays.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MonitorHandle(pub(crate) WGUIDisplay);

impl MonitorHandle {
    fn info(&self) -> WGUIDisplayInfo {
        let mut info = WGUIDisplayInfo::INIT;
        // A display that is gone leaves this as it is: a display of no size.
        unsafe { wguiDisplayGetInfo(self.0, &mut info) };
        info
    }

    pub fn name(&self) -> Option<String> {
        let mut name = [0u8; 256];
        let mut length = 0;
        let status =
            unsafe { wguiDisplayGetName(self.0, name.as_mut_ptr(), name.len(), &mut length) };
        if status != WGUIStatus_Success || length == 0 {
            return None;
        }
        Some(String::from_utf8_lossy(&name[..length.min(name.len())]).into_owned())
    }

    pub fn size(&self) -> PhysicalSize<u32> {
        let info = self.info();
        PhysicalSize::new(info.width, info.height)
    }

    pub fn position(&self) -> PhysicalPosition<i32> {
        let info = self.info();
        PhysicalPosition::new(info.x, info.y)
    }

    pub fn scale_factor(&self) -> f64 {
        match self.info().scaleFactor {
            scale if scale > 0.0 => scale,
            _ => 1.0,
        }
    }

    pub fn refresh_rate_millihertz(&self) -> Option<u32> {
        match self.info().refreshRate {
            0 => None,
            rate => Some(rate),
        }
    }

    /// The mode the display is in. The host has no others to switch to.
    pub fn video_modes(&self) -> impl Iterator<Item = VideoModeHandle> {
        let info = self.info();
        std::iter::once(VideoModeHandle {
            size: (info.width, info.height),
            bit_depth: 32,
            refresh_rate_millihertz: info.refreshRate,
            monitor: self.clone(),
        })
        .filter(|mode| mode.size != (0, 0))
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct VideoModeHandle {
    size: (u32, u32),
    bit_depth: u16,
    refresh_rate_millihertz: u32,
    monitor: MonitorHandle,
}

impl VideoModeHandle {
    pub fn size(&self) -> PhysicalSize<u32> {
        self.size.into()
    }

    pub fn bit_depth(&self) -> u16 {
        self.bit_depth
    }

    pub fn refresh_rate_millihertz(&self) -> u32 {
        self.refresh_rate_millihertz
    }

    pub fn monitor(&self) -> MonitorHandle {
        self.monitor.clone()
    }
}

/// Whether the host has a feature. What it lacks is never asked of it.
pub(crate) fn has(feature: WGUIFeatureName) -> bool {
    unsafe { wguiHasFeature(feature) != 0 }
}
