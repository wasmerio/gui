//! User motion preferences. macOS supplies notifications, without polling.
use crate::system::ReducedMotion;

#[cfg(not(target_os = "macos"))]
pub(super) fn reduced_motion() -> ReducedMotion {
    ReducedMotion::Unknown
}

#[cfg(target_os = "macos")]
pub(super) use mac::{Observer, reduced_motion};

#[cfg(target_os = "macos")]
mod mac {
    use super::ReducedMotion;
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{NSWorkspace, NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification};
    use objc2_foundation::{NSNotificationCenter, NSObjectProtocol};
    use std::sync::atomic::{AtomicU32, Ordering};

    pub(in crate::native) fn reduced_motion() -> ReducedMotion {
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
            let previous = AtomicU32::new(reduced_motion() as u32);
            let block = block2::RcBlock::new(move |_| {
                let preference = reduced_motion();
                if previous.swap(preference as u32, Ordering::AcqRel) != preference as u32 {
                    changed(preference);
                }
            });
            // The workspace's center owns the block. No queue means delivery on
            // the posting thread; captured state is Send + Sync. Keep the token
            // until the native loop ends, then unregister it.
            let token = unsafe {
                center.addObserverForName_object_queue_usingBlock(
                    Some(NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification),
                    None,
                    None,
                    &block,
                )
            };
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
