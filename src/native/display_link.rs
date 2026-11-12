//! Main-thread-only AppKit pacing for guest redraw requests.

use std::cell::Cell;
use std::sync::{Arc, Weak};
use std::time::Duration;

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::NSView;
use objc2_foundation::{NSObject, NSObjectProtocol, NSRunLoop, NSRunLoopCommonModes};
use objc2_quartz_core::{CADisplayLink, CAFrameRateRange};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

struct TargetState {
    window: Weak<Window>,
    requested: Cell<bool>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. The target is used
    // only on AppKit's main thread and has no Drop implementation.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "WasmerGuiDisplayLinkTarget"]
    #[ivars = TargetState]
    struct Target;

    unsafe impl NSObjectProtocol for Target {}

    impl Target {
        // SAFETY: CADisplayLink calls the registered selector with itself.
        #[unsafe(method(displayLinkFired:))]
        fn fired(&self, link: &CADisplayLink) {
            let state = self.ivars();
            link.setPaused(true);
            if state.requested.replace(false) && let Some(window) = state.window.upgrade() {
                window.request_redraw();
            }
        }
    }
);

pub(super) struct DisplayLink {
    link: Retained<CADisplayLink>,
    target: Retained<Target>,
    rate: Cell<f32>,
}

impl DisplayLink {
    pub fn new(window: &Arc<Window>) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let handle = window.window_handle().ok()?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return None;
        };
        // SAFETY: winit owns this NSView for as long as the window is alive.
        // Creation, callbacks and invalidation run on the GUI thread.
        let view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
        if !view.respondsToSelector(sel!(displayLinkWithTarget:selector:)) {
            return None;
        }
        let target = Target::alloc(mtm).set_ivars(TargetState {
            window: Arc::downgrade(window),
            requested: Cell::new(false),
        });
        // SAFETY: NSObject supplies init and Target implements the selector
        // with CADisplayLink's callback signature.
        let target: Retained<Target> = unsafe { msg_send![super(target), init] };
        let link = unsafe { view.displayLinkWithTarget_selector(&target, sel!(displayLinkFired:)) };
        link.setPaused(true);
        // SAFETY: the run loop and link are accessed on the main thread.
        // Common modes keep the callback active during native input tracking.
        unsafe { link.addToRunLoop_forMode(&NSRunLoop::mainRunLoop(), NSRunLoopCommonModes) };
        crate::host::log(|| "using the AppKit display link for redraws".to_owned());
        Some(Self {
            link,
            target,
            rate: Cell::new(0.0),
        })
    }

    pub fn request(&self, nominal_interval: Duration) {
        let rate = (1.0 / nominal_interval.as_secs_f64()).round().max(1.0) as f32;
        if self.rate.replace(rate) != rate {
            self.link.setPreferredFrameRateRange(CAFrameRateRange {
                minimum: rate,
                maximum: rate,
                preferred: rate,
            });
        }
        if !self.target.ivars().requested.replace(true) {
            self.link.setPaused(false);
        }
    }

    pub fn cancel(&self) {
        self.target.ivars().requested.set(false);
        self.link.setPaused(true);
    }
}

impl Drop for DisplayLink {
    fn drop(&mut self) {
        self.link.invalidate();
    }
}
