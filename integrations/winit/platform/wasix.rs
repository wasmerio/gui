//! # WASIX
//!
//! Windows and input from the Wasmer runtime, through `<wasmer/gui.h>`
//! (the WebAssembly import module `wasmer_gui_v0`). A program runs where
//! that module is provided: `wasmer run --experimental-gui`, or a sandbox of
//! the Wasmer SDK made with `gui`, natively or in a browser.
//!
//! A window is one of the system's where the host has those, and a target
//! the embedder provides where it does not: a canvas of the page, in a
//! browser. What a host cannot do is refused, and reported as on other
//! platforms: positions where windows have none, a second window where the
//! page gave one canvas.
//!
//! There is no OpenGL. A window is drawn with frames the program makes
//! itself (softbuffer), or with WebGPU on its
//! [`surface_selector`](WindowExtWasix::surface_selector).

use crate::window::{Window, WindowAttributes};

/// Additional methods on [`Window`] that are specific to WASIX.
pub trait WindowExtWasix {
    /// The name to draw on this window with WebGPU: what goes in
    /// `WGPUWasixSurfaceSourceCanvas.selector` of `webgpu.h`.
    fn surface_selector(&self) -> String;

    /// The host's handle of the window, a `WGUIWindow`, for calling
    /// `<wasmer/gui.h>` directly. It is also what the window's raw window
    /// handle holds.
    fn host_window(&self) -> u32;
}

impl WindowExtWasix for Window {
    fn surface_selector(&self) -> String {
        self.window.surface_selector()
    }

    fn host_window(&self) -> u32 {
        self.window.id().0
    }
}

/// Additional methods on [`WindowAttributes`] that are specific to WASIX.
pub trait WindowAttributesExtWasix {
    /// The target the window adopts, by the name the embedder gave it: one
    /// of the page's canvases, in a browser. Without one the window is the
    /// embedder's default target, or a window of the system's own.
    fn with_target(self, target: impl Into<String>) -> Self;
}

impl WindowAttributesExtWasix for WindowAttributes {
    fn with_target(mut self, target: impl Into<String>) -> Self {
        self.platform_specific.target = Some(target.into());
        self
    }
}
