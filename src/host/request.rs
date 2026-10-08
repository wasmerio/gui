//! Requests that finish later, and the promise that each finishes once.

use std::sync::Arc;

use super::shared::{InstanceShared, Queued};
use crate::abi::Handle;
use crate::abi::enums::{RequestKind, Status};
use crate::abi::events::{Event, RequestCompletedEvent};

/// A request a guest made with a tag (fullscreen, a cursor grab, a clipboard
/// operation) and is owed exactly one `WGUIEventType_RequestCompleted` for.
///
/// Whoever holds this answers it with [`PendingRequest::complete`]. Dropping
/// it unanswered answers `Cancelled`, which is what makes "exactly one" hold
/// when a later request replaces it or its window goes away.
pub struct PendingRequest {
    inner: Option<Inner>,
}

struct Inner {
    instance: Arc<InstanceShared>,
    window: Handle,
    kind: RequestKind,
    tag: u64,
}

impl PendingRequest {
    pub(crate) fn new(
        instance: &Arc<InstanceShared>,
        window: Handle,
        kind: RequestKind,
        tag: u64,
    ) -> Self {
        Self {
            inner: Some(Inner {
                instance: Arc::clone(instance),
                window,
                kind,
                tag,
            }),
        }
    }

    pub fn kind(&self) -> RequestKind {
        self.inner.as_ref().expect("not completed").kind
    }

    /// Tells the guest how the request ended.
    pub fn complete(mut self, status: Status) {
        if let Some(inner) = self.inner.take() {
            inner.finish(status);
        }
    }
}

impl Inner {
    fn finish(self, status: Status) {
        self.instance.push_completion(Queued {
            window: self.window,
            device: 0,
            time: self.instance.system.now(),
            flags: 0,
            event: Event::RequestCompleted(RequestCompletedEvent {
                tag: self.tag,
                kind: self.kind,
                status,
            }),
        });
    }
}

impl Drop for PendingRequest {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            inner.finish(Status::Cancelled);
        }
    }
}

impl std::fmt::Debug for PendingRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("PendingRequest");
        if let Some(inner) = &self.inner {
            debug.field("kind", &inner.kind).field("tag", &inner.tag);
        }
        debug.finish()
    }
}
