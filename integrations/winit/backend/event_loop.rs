use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Instant;

use wasmer_gui_sys::*;

use crate::error::{EventLoopError, NotSupportedError};
use crate::event::{Event, StartCause, WindowEvent};
use crate::event_loop::{
    ActiveEventLoop as RootActiveEventLoop, ControlFlow, DeviceEvents, EventLoopClosed,
};
use crate::window::{
    CustomCursor as RootCustomCursor, CustomCursorSource, Theme, WindowId as RootWindowId,
};

use super::window::WindowShared;
use super::{
    events, has, HostCursor, MonitorHandle, PlatformCustomCursor,
    PlatformSpecificEventLoopAttributes, WindowId,
};

/// Records are read into memory the program supplies, a few of the largest
/// at a time. Words of 64 bits are aligned as records have to be.
const BUFFER_WORDS: usize = 4 * WGUI_MAX_EVENT_SIZE / 8;

/// What the loop and every window share: the connection to the host.
pub(crate) struct Shared {
    pub(crate) instance: WGUIInstance,
    /// The windows that are open, by the host's handle. Records name their
    /// window by it.
    pub(crate) windows: Mutex<HashMap<WGUIWindow, Arc<WindowShared>>>,
    /// Windows that were dropped since the loop last looked, on whichever
    /// thread: each is owed a `Destroyed` event.
    pub(crate) destroyed: Mutex<Vec<WGUIWindow>>,
    /// Tags for requests that complete by event.
    next_tag: AtomicU64,
}

impl Shared {
    pub(crate) fn next_tag(&self) -> u64 {
        self.next_tag.fetch_add(1, Ordering::Relaxed)
    }

    /// Ends the loop's wait in the host, from any thread.
    pub(crate) fn wake(&self) {
        unsafe { wguiInstanceWake(self.instance) };
    }

    pub(crate) fn monitors(&self) -> VecDeque<MonitorHandle> {
        let mut displays = [0; 16];
        let mut count = 0;
        let status = unsafe {
            wguiInstanceGetDisplays(
                self.instance,
                displays.len(),
                displays.as_mut_ptr(),
                &mut count,
            )
        };
        if status != WGUIStatus_Success {
            return VecDeque::new();
        }
        displays[..count.min(displays.len())].iter().map(|display| MonitorHandle(*display)).collect()
    }

    pub(crate) fn primary_monitor(&self) -> Option<MonitorHandle> {
        let monitors = self.monitors();
        monitors
            .iter()
            .find(|monitor| {
                let mut info = WGUIDisplayInfo::INIT;
                unsafe { wguiDisplayGetInfo(monitor.0, &mut info) };
                info.flags & WGUIDisplayFlags_Primary != 0
            })
            .or(monitors.front())
            .cloned()
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        // With everything created on it: windows a program forgot, too.
        unsafe { wguiInstanceDestroy(self.instance) };
    }
}

pub struct EventLoop<T: 'static> {
    window_target: RootActiveEventLoop,
    user_events_sender: mpsc::Sender<T>,
    user_events_receiver: mpsc::Receiver<T>,
}

impl<T: 'static> EventLoop<T> {
    pub(crate) fn new(
        _attributes: &PlatformSpecificEventLoopAttributes,
    ) -> Result<Self, EventLoopError> {
        // A host without this API answers 0 to every call of it.
        if unsafe { wguiGetVersion() } == 0 || !has(WGUIFeatureName_Windows) {
            return Err(EventLoopError::NotSupported(NotSupportedError::new()));
        }
        super::signals::install();
        let descriptor =
            WGUIInstanceDescriptor { eventMask: event_mask(true), ..WGUIInstanceDescriptor::INIT };
        let mut instance = 0;
        let status = unsafe { wguiCreateInstance(&descriptor, &mut instance) };
        if status != WGUIStatus_Success {
            return Err(EventLoopError::Os(os_error!(super::OsError::Host {
                call: "wguiCreateInstance",
                status
            })));
        }
        let (user_events_sender, user_events_receiver) = mpsc::channel();

        Ok(Self {
            window_target: RootActiveEventLoop {
                p: ActiveEventLoop {
                    control_flow: Cell::new(ControlFlow::default()),
                    exit: Cell::new(false),
                    shared: Arc::new(Shared {
                        instance,
                        windows: Mutex::new(HashMap::new()),
                        destroyed: Mutex::new(Vec::new()),
                        next_tag: AtomicU64::new(1),
                    }),
                },
                _marker: PhantomData,
            },
            user_events_sender,
            user_events_receiver,
        })
    }

    /// Backs both `EventLoop::run` and `EventLoop::run_app`, in the order
    /// documented at the top of `src/event.rs`.
    pub fn run<F>(self, mut event_handler: F) -> Result<(), EventLoopError>
    where
        F: FnMut(Event<T>, &RootActiveEventLoop),
    {
        let target = &self.window_target;
        let shared = &target.p.shared;
        let mut records = vec![0u64; BUFFER_WORDS];
        let mut redraws: Vec<WGUIWindow> = Vec::new();
        let mut start_cause = StartCause::Init;

        loop {
            event_handler(Event::NewEvents(start_cause), target);

            // Nothing suspends a program here: it is resumed once, at the
            // start, which is when applications create their windows.
            if start_cause == StartCause::Init {
                event_handler(Event::Resumed, target);
            }

            // What the host has to tell, oldest first. One buffer of it a
            // turn: a program that is slow to answer still gets to its
            // redraws and to `AboutToWait`, and the wait below returns at
            // once while there is more. Asking is also where signals reach
            // the program, however its loop runs (see `signals`).
            let mut written = 0;
            let status = unsafe {
                wguiInstancePollEvents(
                    shared.instance,
                    records.as_mut_ptr().cast(),
                    records.len() * 8,
                    &mut written,
                )
            };
            if status == WGUIStatus_Success {
                let bytes = records.as_ptr().cast::<u8>();
                let mut record = unsafe { wguiEventFirst(bytes, written) };
                while !record.is_null() {
                    events::dispatch(shared, record, &mut redraws, &mut |event| {
                        event_handler(event, target)
                    });
                    record = unsafe { wguiEventNext(bytes, written, record) };
                }
            }

            for window in std::mem::take(&mut *shared.destroyed.lock().unwrap()) {
                redraws.retain(|other| *other != window);
                event_handler(
                    Event::WindowEvent {
                        window_id: RootWindowId(WindowId(window)),
                        event: WindowEvent::Destroyed,
                    },
                    target,
                );
            }

            while let Ok(event) = self.user_events_receiver.try_recv() {
                event_handler(Event::UserEvent(event), target);
            }

            // Redraws last, each window once, and only those still open.
            for window in redraws.drain(..) {
                if shared.windows.lock().unwrap().contains_key(&window) {
                    event_handler(
                        Event::WindowEvent {
                            window_id: RootWindowId(WindowId(window)),
                            event: WindowEvent::RedrawRequested,
                        },
                        target,
                    );
                }
            }

            event_handler(Event::AboutToWait, target);

            if target.p.exiting() {
                break;
            }

            start_cause = match target.p.control_flow() {
                ControlFlow::Poll => StartCause::Poll,
                ControlFlow::Wait => {
                    let start = Instant::now();
                    wait(shared, WGUI_WAIT_FOREVER);
                    StartCause::WaitCancelled { start, requested_resume: None }
                },
                ControlFlow::WaitUntil(requested_resume) => {
                    let start = Instant::now();
                    let timeout = requested_resume.saturating_duration_since(start).as_nanos();
                    if wait(shared, u64::try_from(timeout).unwrap_or(u64::MAX - 1)) {
                        StartCause::WaitCancelled {
                            start,
                            requested_resume: Some(requested_resume),
                        }
                    } else {
                        StartCause::ResumeTimeReached { start, requested_resume }
                    }
                },
            };
        }

        event_handler(Event::LoopExiting, target);

        Ok(())
    }

    pub fn window_target(&self) -> &RootActiveEventLoop {
        &self.window_target
    }

    pub fn create_proxy(&self) -> EventLoopProxy<T> {
        EventLoopProxy {
            user_events_sender: self.user_events_sender.clone(),
            shared: self.window_target.p.shared.clone(),
        }
    }
}

/// Sleeps in the host until it has a record, the loop is woken, or `timeout`
/// nanoseconds have passed. `false` when it was the time that passed.
fn wait(shared: &Shared, timeout: u64) -> bool {
    let mut reasons = 0;
    let status = unsafe { wguiInstanceWaitEvents(shared.instance, timeout, &mut reasons) };
    if status != WGUIStatus_Success {
        // The instance is gone, which only a host shutting the program down
        // does: the loop goes around, and finds out.
        return true;
    }
    // A wait the host cut short (`WGUIWaitReasons_Interrupted`) counts as one
    // that something ended: the loop goes around, and waits again.
    reasons != 0
}

/// The classes of records the loop reads: the default ones, and what
/// devices report besides where the host tells of that.
fn event_mask(device_events: bool) -> WGUIEventClass {
    let mut mask = WGUIEventClass_Default;
    if device_events && has(WGUIFeatureName_RawInput) {
        mask |= WGUIEventClass_Raw;
    }
    mask
}

pub struct EventLoopProxy<T: 'static> {
    user_events_sender: mpsc::Sender<T>,
    shared: Arc<Shared>,
}

impl<T: 'static> EventLoopProxy<T> {
    pub fn send_event(&self, event: T) -> Result<(), EventLoopClosed<T>> {
        // Fails once the `EventLoop`, which owns the receiver, is gone.
        self.user_events_sender
            .send(event)
            .map_err(|mpsc::SendError(event)| EventLoopClosed(event))?;
        self.shared.wake();
        Ok(())
    }
}

impl<T: 'static> Clone for EventLoopProxy<T> {
    fn clone(&self) -> Self {
        Self { user_events_sender: self.user_events_sender.clone(), shared: self.shared.clone() }
    }
}

pub struct ActiveEventLoop {
    control_flow: Cell<ControlFlow>,
    exit: Cell<bool>,
    pub(crate) shared: Arc<Shared>,
}

impl ActiveEventLoop {
    pub fn create_custom_cursor(&self, source: CustomCursorSource) -> RootCustomCursor {
        let image = &source.inner.0;
        let mut handle = 0;
        if has(WGUIFeatureName_CustomCursors) {
            let descriptor = WGUICursorDescriptor {
                width: image.width.into(),
                height: image.height.into(),
                hotspotX: image.hotspot_x.into(),
                hotspotY: image.hotspot_y.into(),
                pixels: image.rgba.as_ptr().cast(),
                pixelsSize: image.rgba.len(),
                ..WGUICursorDescriptor::INIT
            };
            // A cursor the host would not make shows as the default one.
            unsafe { wguiInstanceCreateCursor(self.shared.instance, &descriptor, &mut handle) };
        }
        RootCustomCursor { inner: PlatformCustomCursor(Arc::new(HostCursor { handle })) }
    }

    pub fn available_monitors(&self) -> VecDeque<MonitorHandle> {
        self.shared.monitors()
    }

    pub fn primary_monitor(&self) -> Option<MonitorHandle> {
        self.shared.primary_monitor()
    }

    pub fn listen_device_events(&self, allowed: DeviceEvents) {
        let mask = event_mask(!matches!(allowed, DeviceEvents::Never));
        unsafe { wguiInstanceSetEventMask(self.shared.instance, mask) };
    }

    pub fn system_theme(&self) -> Option<Theme> {
        let mut info = WGUIInstanceInfo::INIT;
        unsafe { wguiInstanceGetInfo(self.shared.instance, &mut info) };
        events::theme(info.systemTheme)
    }

    pub fn set_control_flow(&self, control_flow: ControlFlow) {
        self.control_flow.set(control_flow)
    }

    pub fn control_flow(&self) -> ControlFlow {
        self.control_flow.get()
    }

    pub(crate) fn exit(&self) {
        self.exit.set(true)
    }

    pub(crate) fn exiting(&self) -> bool {
        self.exit.get()
    }

    pub(crate) fn owned_display_handle(&self) -> OwnedDisplayHandle {
        OwnedDisplayHandle
    }

    #[cfg(feature = "rwh_05")]
    #[inline]
    pub fn raw_display_handle_rwh_05(&self) -> rwh_05::RawDisplayHandle {
        rwh_05::RawDisplayHandle::Web(rwh_05::WebDisplayHandle::empty())
    }

    #[cfg(feature = "rwh_06")]
    #[inline]
    pub fn raw_display_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
        Ok(rwh_06::RawDisplayHandle::Web(rwh_06::WebDisplayHandle::new()))
    }
}

/// There is one host, and nothing to hold of it.
///
/// raw-window-handle has no handles for WASIX. The web's are borrowed: they
/// are the only pair that is plain data, a display that is nothing and a
/// window that is a number, which here is the host's handle of the window
/// (a `WGUIWindow`).
#[derive(Clone)]
pub(crate) struct OwnedDisplayHandle;

impl OwnedDisplayHandle {
    #[cfg(feature = "rwh_05")]
    #[inline]
    pub fn raw_display_handle_rwh_05(&self) -> rwh_05::RawDisplayHandle {
        rwh_05::WebDisplayHandle::empty().into()
    }

    #[cfg(feature = "rwh_06")]
    #[inline]
    pub fn raw_display_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
        Ok(rwh_06::WebDisplayHandle::new().into())
    }
}
