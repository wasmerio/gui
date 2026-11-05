//! User motion preferences. macOS supplies notifications, without polling.
use crate::system::ReducedMotion;

#[cfg(not(target_os = "macos"))]
pub(super) fn initial_reduced_motion() -> ReducedMotion {
    ReducedMotion::Unknown
}

#[cfg(target_os = "macos")]
pub(super) use mac::{Observer, initial_reduced_motion};

#[cfg(target_os = "macos")]
mod mac {
    use super::ReducedMotion;
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{NSWorkspace, NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification};
    use objc2_foundation::{NSNotificationCenter, NSObjectProtocol, NSOperationQueue};
    use std::sync::Arc;

    /// Usually called by GuiMainThread's constructor on the process main
    /// thread. If constructed elsewhere, the observer seeds the cache later.
    pub(in crate::native) fn initial_reduced_motion() -> ReducedMotion {
        if MainThreadMarker::new().is_none() {
            return ReducedMotion::Unknown;
        }
        read_reduced_motion()
    }

    fn read_reduced_motion() -> ReducedMotion {
        debug_assert!(MainThreadMarker::new().is_some());
        if NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion() {
            ReducedMotion::Reduce
        } else {
            ReducedMotion::NoPreference
        }
    }

    pub(in crate::native) struct Observer {
        center: Retained<NSNotificationCenter>,
        token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
    }

    impl Observer {
        pub(in crate::native) fn new(
            changed: impl Fn(ReducedMotion) + Send + Sync + 'static,
        ) -> Self {
            let center = NSWorkspace::sharedWorkspace().notificationCenter();
            let changed = Arc::new(changed);
            let notify = Arc::clone(&changed);
            let block = block2::RcBlock::new(move |_| {
                notify(read_reduced_motion());
            });
            // The workspace owns the sendable block. Deliver on the main queue
            // so every AppKit read stays on the main thread. Shared's cached
            // value suppresses unrelated accessibility notifications.
            let token = unsafe {
                center.addObserverForName_object_queue_usingBlock(
                    Some(NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification),
                    None,
                    Some(&NSOperationQueue::mainQueue()),
                    &block,
                )
            };
            // Re-read after registering to cover a change between construction
            // of the window system and starting the native loop.
            changed(read_reduced_motion());
            Self { center, token }
        }
    }

    impl Drop for Observer {
        fn drop(&mut self) {
            unsafe {
                self.center.removeObserver(self.token.as_ref());
            }
        }
    }
}
