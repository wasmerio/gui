//! `WGUIInstance`: the event queue, waiting, displays, and making things.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::shared::InstanceShared;
use super::{Cursor, Host, Object, ObjectKind, lock};
use crate::abi::api::InstanceApi;
use crate::abi::enums::*;
use crate::abi::structs::*;
use crate::abi::{AbiError, AbiResult, Cx, Handle, OutPtr};
use crate::system::CursorImage;

/// How long a waiting thread sleeps between looks at whether its program is
/// being stopped.
pub(super) const WAIT_SLICE: Duration = Duration::from_millis(50);
/// How long a wait may last before the host hands the thread back for a
/// moment (see `WGUIWaitReasons_Interrupted`).
const INTERRUPT_AFTER: Duration = Duration::from_millis(250);
/// The largest edge of a custom cursor when the window system names none.
const MAX_CURSOR_EDGE: u32 = 512;

/// One look at a waiting instance: why its wait is over, or how long to sleep
/// before looking again. Shared by the parking wait and the browser's
/// suspending one (`web_wait`), so both end for the same reasons.
pub(crate) enum WaitStep {
    /// The wait is over: the reasons it returns, or why the call fails.
    Done(AbiResult<u32>),
    /// Nothing ends it yet. Look again after this long at most, or as soon
    /// as the instance's wait set is notified.
    Sleep(Duration),
}

/// Looks once at a wait that began at `started`, on [`crate::clock`], and may
/// last `limit` (`None` for as long as it takes). A pending wake is used up.
pub(crate) fn wait_step(
    shared: &InstanceShared,
    stopped: &dyn Fn() -> bool,
    started: Duration,
    limit: Option<Duration>,
) -> WaitStep {
    let mut why = 0;
    if shared.has_events() {
        why |= wait_reasons::EVENTS;
    }
    if shared.wait.take_wake() {
        why |= wait_reasons::WOKEN;
    }
    if why != 0 {
        return WaitStep::Done(Ok(why));
    }
    if stopped() {
        return WaitStep::Done(Err(AbiError::Stopped));
    }
    if shared.is_closed() {
        // Another thread destroyed the instance under this one.
        return WaitStep::Done(Ok(wait_reasons::INTERRUPTED));
    }
    let waited = crate::clock::now().saturating_sub(started);
    let left = match limit {
        Some(limit) if waited >= limit => return WaitStep::Done(Ok(0)),
        Some(limit) => limit - waited,
        None => Duration::MAX,
    };
    if waited >= INTERRUPT_AFTER {
        return WaitStep::Done(Ok(wait_reasons::INTERRUPTED));
    }
    WaitStep::Sleep(WAIT_SLICE.min(left).min(INTERRUPT_AFTER - waited))
}

impl InstanceApi for Host {
    fn instance_destroy(&self, cx: &mut Cx<'_>, instance: Handle) {
        cx.owned.retain(|owned| *owned != instance);
        self.destroy_instance(instance);
    }

    fn instance_get_info(
        &self,
        cx: &mut Cx<'_>,
        instance: Handle,
        info: OutPtr<InstanceInfo>,
    ) -> AbiResult<Status> {
        if self.instance(instance).is_none() {
            return Ok(Status::InvalidHandle);
        }
        let system = self.ctx.system.info();
        info.write(
            &cx.mem,
            &InstanceInfo {
                abi_version: ABI_VERSION,
                host_kind: system.host_kind,
                shortcut_modifier: system.shortcut_modifier,
                input_devices: system.input_devices,
                system_theme: system.theme,
                max_cursor_size: system.max_cursor_size,
            },
        )?;
        Ok(Status::Success)
    }

    fn instance_poll_events(
        &self,
        cx: &mut Cx<'_>,
        instance: Handle,
        buffer: u32,
        capacity: u32,
        written: OutPtr<u32>,
    ) -> AbiResult<Status> {
        let Some(instance) = self.instance(instance) else {
            return Ok(Status::InvalidHandle);
        };
        if !buffer.is_multiple_of(8) {
            return Ok(Status::InvalidArgument);
        }
        if capacity < MAX_EVENT_SIZE {
            return Ok(Status::BufferTooSmall);
        }
        // Nothing is taken off the queue for a buffer that cannot be written.
        if u64::from(buffer) + u64::from(capacity) > cx.mem.size() {
            cx.mem.read_bytes::<1>(buffer.wrapping_add(capacity - 1))?;
        }
        let records = instance.shared.poll(capacity as usize);
        cx.mem.write_bytes(buffer, &records)?;
        written.write(&cx.mem, &(records.len() as u32))?;
        Ok(Status::Success)
    }

    fn instance_wait_events(
        &self,
        cx: &mut Cx<'_>,
        instance: Handle,
        timeout: u64,
        reasons: OutPtr<u32>,
    ) -> AbiResult<Status> {
        let Some(instance) = self.instance(instance) else {
            return Ok(Status::InvalidHandle);
        };
        let shared = &instance.shared;
        let limit = (timeout != WAIT_FOREVER).then(|| Duration::from_nanos(timeout));
        let started = crate::clock::now();
        shared.waiting.fetch_add(1, Ordering::AcqRel);
        let outcome = loop {
            match wait_step(shared, cx.stopped, started, limit) {
                WaitStep::Done(outcome) => break outcome,
                WaitStep::Sleep(slice) => shared.wait.park(slice, || shared.has_events()),
            }
        };
        shared.waiting.fetch_sub(1, Ordering::AcqRel);
        reasons.write(&cx.mem, &outcome?)?;
        Ok(Status::Success)
    }

    fn instance_wake(&self, _cx: &mut Cx<'_>, instance: Handle) -> AbiResult<Status> {
        let Some(instance) = self.instance(instance) else {
            return Ok(Status::InvalidHandle);
        };
        instance.shared.wait.wake();
        Ok(Status::Success)
    }

    fn instance_set_event_mask(
        &self,
        _cx: &mut Cx<'_>,
        instance: Handle,
        mask: u64,
    ) -> AbiResult<Status> {
        let Some(instance) = self.instance(instance) else {
            return Ok(Status::InvalidHandle);
        };
        // Classes this version does not have are asked for in vain, which is
        // not an error: a guest built for a newer one may ask for them.
        instance
            .shared
            .event_mask
            .store(mask | event_class::SYSTEM, Ordering::Release);
        Ok(Status::Success)
    }

    fn instance_get_time(
        &self,
        cx: &mut Cx<'_>,
        instance: Handle,
        time: OutPtr<u64>,
    ) -> AbiResult<Status> {
        if self.instance(instance).is_none() {
            return Ok(Status::InvalidHandle);
        }
        time.write(&cx.mem, &self.ctx.system.now())?;
        Ok(Status::Success)
    }

    fn instance_get_displays(
        &self,
        cx: &mut Cx<'_>,
        instance: Handle,
        capacity: u32,
        displays: OutPtr<u32>,
        count: OutPtr<u32>,
    ) -> AbiResult<Status> {
        let Some(state) = self.instance(instance) else {
            return Ok(Status::InvalidHandle);
        };
        let all = self.ctx.system.displays();
        let mut written = 0;
        for display in &all {
            let handle = self.display_handle(instance, &state.shared, display.id);
            if handle != 0 && !displays.is_null() && written < capacity {
                displays.write_at(&cx.mem, written, &handle)?;
                written += 1;
            }
        }
        count.write(&cx.mem, &(all.len() as u32))?;
        Ok(Status::Success)
    }

    fn instance_create_window(
        &self,
        cx: &mut Cx<'_>,
        instance: Handle,
        descriptor: Option<&WindowDescriptor>,
        window: OutPtr<u32>,
    ) -> AbiResult<Status> {
        let handle = match self.create_window(instance, descriptor) {
            Ok(handle) => handle,
            Err(status) => return Ok(status),
        };
        if let Err(error) = window.write(&cx.mem, &handle) {
            self.destroy_window(handle);
            return Err(error);
        }
        Ok(Status::Success)
    }

    fn instance_create_cursor(
        &self,
        cx: &mut Cx<'_>,
        instance: Handle,
        descriptor: &CursorDescriptor,
        cursor: OutPtr<u32>,
    ) -> AbiResult<Status> {
        if self.instance(instance).is_none() {
            return Ok(Status::InvalidHandle);
        }
        if !self.has(FeatureName::CustomCursors) {
            return Ok(Status::Unsupported);
        }
        let (width, height) = (descriptor.width, descriptor.height);
        let edge = match self.ctx.system.info().max_cursor_size {
            0 => MAX_CURSOR_EDGE,
            limit => limit,
        };
        let bytes = u64::from(width) * u64::from(height) * 4;
        if width == 0
            || height == 0
            || width > edge
            || height > edge
            || descriptor.hotspot_x >= width
            || descriptor.hotspot_y >= height
            || u64::from(descriptor.pixels_size) != bytes
            || descriptor.pixels == 0
            || !(descriptor.scale_factor.is_finite() && descriptor.scale_factor > 0.0)
        {
            return Ok(Status::InvalidArgument);
        }
        if self
            .ctx
            .limits
            .max_cursors
            .is_some_and(|max| self.count(ObjectKind::Cursor) >= max)
        {
            return Ok(Status::LimitExceeded);
        }
        let image = Arc::new(CursorImage {
            width,
            height,
            hotspot_x: descriptor.hotspot_x,
            hotspot_y: descriptor.hotspot_y,
            scale_factor: descriptor.scale_factor,
            rgba: cx.mem.read_vec(descriptor.pixels, bytes)?,
        });
        let Some(handle) = self.insert(Object::Cursor(Arc::new(Cursor { image })), instance) else {
            return Ok(Status::LimitExceeded);
        };
        if let Err(error) = cursor.write(&cx.mem, &handle) {
            lock(&self.table).remove(handle, ObjectKind::Cursor);
            return Err(error);
        }
        Ok(Status::Success)
    }

    fn instance_get_clipboard_info(
        &self,
        cx: &mut Cx<'_>,
        instance: Handle,
        selection: ClipboardSelection,
        info: OutPtr<ClipboardInfo>,
    ) -> AbiResult<Status> {
        self.clipboard_info(cx, instance, selection, info)
    }

    fn instance_read_clipboard(
        &self,
        cx: &mut Cx<'_>,
        instance: Handle,
        selection: ClipboardSelection,
        mime_type: &str,
        buffer: u32,
        capacity: u32,
        size: OutPtr<u32>,
    ) -> AbiResult<Status> {
        self.clipboard_read(cx, instance, selection, mime_type, buffer, capacity, size)
    }

    fn instance_request_clipboard(
        &self,
        _cx: &mut Cx<'_>,
        instance: Handle,
        selection: ClipboardSelection,
        mime_type: &str,
        tag: u64,
    ) -> AbiResult<Status> {
        Ok(self.clipboard_request(instance, selection, mime_type, tag))
    }

    fn instance_write_clipboard(
        &self,
        cx: &mut Cx<'_>,
        instance: Handle,
        selection: ClipboardSelection,
        items: &[ClipboardItem],
        tag: u64,
    ) -> AbiResult<Status> {
        self.clipboard_write(cx, instance, selection, items, tag)
    }
}
