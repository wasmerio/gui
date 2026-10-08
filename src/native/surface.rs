//! Native windows, as something wasmer-webgpu can draw on.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, WindowHandle,
};
use wasmer_webgpu::surface::{SurfaceProvider, SurfaceTarget, WindowTarget};
use winit::window::Window;

use super::{Command, Shared, WindowKey, WinitSystem};
use crate::system::SURFACE_SELECTOR_PREFIX;

/// How many times in a row the GPU has to find a window unseen before the
/// guest is told. A new window is unseen for a moment, which is not news.
const UNSEEN_BEFORE_TOLD: u32 = 6;

/// What the loop hands out of a window for a surface.
pub(super) struct SurfaceParts {
    pub(super) window: Arc<Window>,
    pub(super) key: WindowKey,
    /// The window's size, kept current by the loop: width, then height in
    /// the upper half.
    pub(super) size: Arc<AtomicU64>,
    /// How many times the window system has said whether the window can be
    /// seen, counted by the loop.
    pub(super) seen_said: Arc<AtomicU64>,
}

/// A native window a guest's surface presents to.
struct SurfaceWindow {
    /// `None` only while being dropped.
    window: Option<Arc<Window>>,
    key: WindowKey,
    size: Arc<AtomicU64>,
    shared: Arc<Shared>,
    seen_said: Arc<AtomicU64>,
    /// `seen_said` as it was before the GPU last looked at the window.
    said_before: AtomicU64,
    /// How many times in a row the GPU has found the window unseen.
    unseen: AtomicU32,
    /// One more than the `seen_said` the loop was last told "unseen" with,
    /// so that it is told once.
    told: AtomicU64,
}

impl SurfaceWindow {
    fn window(&self) -> &Window {
        self.window.as_ref().expect("present until dropped")
    }
}

impl HasWindowHandle for SurfaceWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        self.window().window_handle()
    }
}

impl HasDisplayHandle for SurfaceWindow {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        self.window().display_handle()
    }
}

impl WindowTarget for SurfaceWindow {
    fn size(&self) -> (u32, u32) {
        // Not asked of the window: on some platforms that is a round trip
        // to the loop's thread.
        let size = self.size.load(Ordering::Acquire);
        (size as u32, (size >> 32) as u32)
    }

    fn run_on_owner_thread(&self, work: Box<dyn FnOnce() + Send>) {
        let (done, wait) = mpsc::channel();
        let sent = self.shared.send(Command::Run(Box::new(move || {
            work();
            let _ = done.send(());
        })));
        if sent {
            // Ends without the work having run when the loop has ended.
            let _ = wait.recv();
        }
    }

    fn pre_present(&self) {
        self.unseen.store(0, Ordering::Release);
        // Wayland paces a window's redraw requests by its presents. Asked of
        // the window directly there; elsewhere winit has nothing to do, and
        // on some platforms would cross to the loop's thread to do it.
        #[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
        self.window().pre_present_notify();
    }

    /// The GPU is sometimes the only one to know that a window cannot be
    /// seen: a window opened behind a locked screen on macOS was never seen,
    /// so the window system has no change to announce. The guest is told
    /// through the window all the same, and stops drawing for nobody.
    ///
    /// The loop is given the count of what the window system had said before
    /// the GPU looked, and believes this only if nothing was said since: a
    /// window that has just come into view is not called unseen by a word
    /// that was on its way.
    fn wait_presentable(&self, timeout: Duration) {
        let said = self
            .said_before
            .swap(self.seen_said.load(Ordering::Acquire), Ordering::AcqRel);
        let unseen = self.unseen.fetch_add(1, Ordering::AcqRel) + 1;
        if unseen >= UNSEEN_BEFORE_TOLD && self.told.swap(said + 1, Ordering::AcqRel) != said + 1 {
            self.shared.send(Command::Unseen {
                window: self.key,
                said,
            });
        }
        std::thread::sleep(timeout);
    }
}

impl Drop for SurfaceWindow {
    /// A window is let go of on the thread that made it. A surface is
    /// dropped wherever the guest drops it, so the window is sent home; and
    /// when there is no loop left to send it to, the process is ending and
    /// the window is left as it is rather than freed from the wrong thread.
    fn drop(&mut self) {
        if let Some(window) = self.window.take()
            && let Err(window) = self.shared.send_window_home(window)
        {
            std::mem::forget(window);
        }
    }
}

struct NativeSurfaces {
    shared: Arc<Shared>,
    fallback: Option<Arc<dyn SurfaceProvider>>,
}

impl SurfaceProvider for NativeSurfaces {
    fn open(&self, selector: &str) -> Option<SurfaceTarget> {
        if !selector.starts_with(SURFACE_SELECTOR_PREFIX) {
            return self.fallback.as_ref()?.open(selector);
        }
        let (reply, answer) = mpsc::sync_channel(1);
        if !self.shared.send(Command::Surface {
            selector: selector.to_owned(),
            reply,
        }) {
            return None;
        }
        let parts = answer.recv().ok()??;
        Some(SurfaceTarget::Window(Arc::new(SurfaceWindow {
            window: Some(parts.window),
            key: parts.key,
            size: parts.size,
            shared: Arc::clone(&self.shared),
            said_before: AtomicU64::new(parts.seen_said.load(Ordering::Acquire)),
            seen_said: parts.seen_said,
            unseen: AtomicU32::new(0),
            told: AtomicU64::new(0),
        })))
    }
}

impl WinitSystem {
    /// What wasmer-webgpu needs to draw on this system's windows: a provider
    /// that knows them by their selectors. Selectors that name no window
    /// here go to `fallback`.
    pub fn surfaces(&self, fallback: Option<Arc<dyn SurfaceProvider>>) -> Arc<dyn SurfaceProvider> {
        Arc::new(NativeSurfaces {
            shared: Arc::clone(&self.shared),
            fallback,
        })
    }
}
