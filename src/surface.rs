//! Drawing on this crate's windows with wasmer-webgpu.
//!
//! A guest draws on a window by creating a WebGPU surface with the window's
//! selector (`wguiWindowGetSurfaceSelector`). wasmer-webgpu asks its
//! `SurfaceProvider` what the selector names; these are the providers that
//! know this crate's windows:
//!
//! ```ignore
//! let webgpu = WebGpuCtx::builder()
//!     .surface_provider(windows.surfaces(None))
//!     .build();
//! ```

use std::sync::Arc;

use wasmer_webgpu::surface::{Frame, FrameFormat, FrameSink, SurfaceProvider, SurfaceTarget};

use crate::headless::{HeadlessSystem, HeadlessWindow, PresentedFrame};

struct HeadlessSurfaces {
    system: HeadlessSystem,
    fallback: Option<Arc<dyn SurfaceProvider>>,
}

impl SurfaceProvider for HeadlessSurfaces {
    fn open(&self, selector: &str) -> Option<SurfaceTarget> {
        match self.system.window_for_selector(selector) {
            Some(window) => Some(SurfaceTarget::Headless(Arc::new(HeadlessFrames {
                capture: self.system.options().capture_frames,
                window,
            }))),
            None => self.fallback.as_ref()?.open(selector),
        }
    }
}

/// A headless window, as something to present frames to.
struct HeadlessFrames {
    window: HeadlessWindow,
    capture: bool,
}

impl FrameSink for HeadlessFrames {
    fn size(&self) -> (u32, u32) {
        self.window.model().size
    }

    fn wants_pixels(&self) -> bool {
        self.capture
    }

    fn present(&self, frame: Frame<'_>) {
        self.window.present(PresentedFrame {
            width: frame.width,
            height: frame.height,
            bgra: frame.format == FrameFormat::Bgra8,
            pixels: frame.pixels.map(<[u8]>::to_vec).unwrap_or_default(),
        });
    }
}

impl HeadlessSystem {
    /// What wasmer-webgpu needs to draw on this system's windows: a provider
    /// that knows them by their selectors. Frames are counted, and kept when
    /// the system captures them. Selectors that name no window here go to
    /// `fallback`.
    pub fn surfaces(&self, fallback: Option<Arc<dyn SurfaceProvider>>) -> Arc<dyn SurfaceProvider> {
        Arc::new(HeadlessSurfaces {
            system: self.clone(),
            fallback,
        })
    }
}
