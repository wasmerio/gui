//! A window system whose windows are views of the embedder's own user
//! interface: the canvases of a web page, the views of an app.
//!
//! Such a user interface has a thread of its own that must never wait. A
//! browser does not let its page block at all, and an app that blocks its
//! main thread is hung. So nothing here makes that thread wait, for a lock
//! or for a guest:
//!
//! - What guests ask for is queued as [`ViewCommand`]s, and the embedder is
//!   told that the queue has something in it. On its interface's thread it
//!   takes them with [`ViewSystem::next_command`] and carries them out.
//! - What the user does there, it reports through the sink that came with
//!   each window, which is lock-free.
//!
//! ```ignore
//! let views = ViewSystem::new(ViewOptions::default(), || post_to_ui_thread(serve));
//! let gui = GuiCtx::builder().window_system(Arc::new(views.clone())).build();
//!
//! // On the interface's thread, whenever told:
//! fn serve() {
//!     while let Some(command) = views.next_command() {
//!         match command {
//!             ViewCommand::Create { view, attributes, mut sink, answer } => {
//!                 let (width, height, scale) = my_view_for(&attributes);
//!                 sink.resized(width, height, scale);
//!                 keep(view, sink);
//!                 let _ = answer.created(my_selector_for(view));
//!             }
//!             ViewCommand::Change { view, change } => apply(view, change),
//!             ViewCommand::Destroy { view } => forget(view),
//!             _ => {}
//!         }
//!     }
//! }
//! ```
//!
//! Only two things make a guest's thread wait for the interface's: a window
//! being made, which is not a window until the interface has said how big it
//! is, and nothing else. Everything else a guest asks for is on its way when
//! its call returns.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::Thread;
use std::time::Duration;

use crossbeam_queue::{ArrayQueue, SegQueue};

use crate::abi::AbiEnum;
use crate::host::lock;
use crate::system::*;

/// The embedder's name for a view that is, or is to be, a guest's window.
pub type ViewId = u64;

/// How long a guest's thread sleeps between looks at whether the interface
/// has answered, or the guest was stopped meanwhile.
const ANSWER_SLICE: Duration = Duration::from_millis(20);

/// What the views are like, and how patient guests are with them.
#[derive(Clone, Debug)]
pub struct ViewOptions {
    /// What guests are told of the window system.
    pub info: SystemInfo,
    /// How long a guest waits for its window to be made before the call
    /// fails. An interface that is busy, or gone, must not hang a guest.
    pub answer_timeout: Duration,
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            info: SystemInfo {
                host_kind: HostKind::Embedded,
                features: vec![FeatureName::Windows],
                shortcut_modifier: modifiers::CONTROL,
                input_devices: 0,
                theme: Theme::Unknown,
                max_cursor_size: 0,
            },
            answer_timeout: Duration::from_secs(10),
        }
    }
}

/// Something a guest asked for, for the interface's thread to carry out.
pub enum ViewCommand {
    /// A guest connected. What concerns no window in particular (the theme,
    /// the displays, the clipboard changing) is reported to it; it says when
    /// the guest has gone, and is then to be dropped.
    Attach(InstanceSink),
    /// A guest wants a window. Choose or make a view for it, report its size
    /// and state through `sink`, keep the sink to report input with, and
    /// answer. Dropping `answer` refuses.
    Create {
        view: ViewId,
        attributes: WindowAttributes,
        sink: Box<WindowSink>,
        answer: ViewAnswer,
    },
    /// A guest asks something of its window. Never a frame to show: those
    /// come as [`ViewCommand::Frame`].
    Change { view: ViewId, change: WindowChange },
    /// A guest drew a frame for its window. Take it, and show it. Frames a
    /// guest presents faster than this is carried out replace each other,
    /// so what is taken is always the newest, and there may be none left to
    /// take.
    Frame { view: ViewId, frame: ViewFrame },
    /// The window is gone: let go of the view, and drop its sink.
    Destroy { view: ViewId },
    /// A guest wants to read a clipboard whose content is not known here
    /// yet. Find it out, say so with [`ViewSystem::clipboard_fetched`], and
    /// complete the request.
    ClipboardRequest {
        selection: ClipboardSelection,
        mime_type: String,
        request: PendingRequest,
    },
    /// A guest replaced a clipboard's content. Put it on the system's, and
    /// complete the request.
    ClipboardWrite {
        selection: ClipboardSelection,
        items: Vec<ClipboardData>,
        request: PendingRequest,
    },
}

/// Where a window's newest frame waits to be shown.
#[derive(Clone)]
pub struct ViewFrame {
    slot: Arc<Mutex<Option<PixelFrame>>>,
}

impl ViewFrame {
    /// The frame, unless it was taken already.
    pub fn take(&self) -> Option<PixelFrame> {
        lock(&self.slot).take()
    }
}

/// The guest stopped waiting for its window (it was stopped itself, or the
/// answer took too long): the view is nobody's window, and is the
/// interface's to let go of. Its sink reports to nobody.
#[derive(Debug, PartialEq, Eq)]
pub struct Abandoned;

struct Answer {
    /// The answer, once there is one: the window's surface selector.
    value: ArrayQueue<Result<String, Status>>,
    /// The guest's thread, parked while there is none.
    waiter: Thread,
    abandoned: AtomicBool,
}

/// The interface's answer to [`ViewCommand::Create`].
pub struct ViewAnswer {
    answer: Arc<Answer>,
    given: bool,
}

impl ViewAnswer {
    /// The view is the guest's window. `selector` is what a surface for it is
    /// created with (`wguiWindowGetSurfaceSelector`). The window's size has
    /// to have been reported through its sink by now.
    pub fn created(mut self, selector: impl Into<String>) -> Result<(), Abandoned> {
        self.give(Ok(selector.into()))
    }

    /// There is no window: `NotFound` for a target nobody knows, `Busy` for
    /// one that is another window already, `Denied`, `Unsupported`, `Error`.
    pub fn refused(mut self, status: Status) {
        let _ = self.give(Err(status));
    }

    fn give(&mut self, value: Result<String, Status>) -> Result<(), Abandoned> {
        self.given = true;
        if self.answer.abandoned.load(Ordering::Acquire) {
            return Err(Abandoned);
        }
        let _ = self.answer.value.push(value);
        self.answer.waiter.unpark();
        // The guest may have given up between the look and the answer. It
        // then finds the answer when it looks one last time, and undoes it.
        Ok(())
    }
}

impl Drop for ViewAnswer {
    fn drop(&mut self) {
        if !self.given {
            let _ = self.give(Err(Status::Error));
        }
    }
}

#[derive(Default)]
struct Clipboard {
    items: Vec<ClipboardData>,
    /// Whether there is text, as far as the interface could tell without
    /// reading it.
    has_text: bool,
    sequence: u32,
    /// The content is known here, so a read answers.
    fetched: bool,
}

#[derive(Default)]
struct Shared {
    displays: Vec<DisplayData>,
    clipboard: Clipboard,
    primary: Clipboard,
}

impl Shared {
    fn clipboard(&mut self, selection: ClipboardSelection) -> &mut Clipboard {
        match selection {
            ClipboardSelection::Clipboard => &mut self.clipboard,
            ClipboardSelection::Primary => &mut self.primary,
        }
    }
}

struct Inner {
    options: ViewOptions,
    /// Tells the interface's thread that `commands` has something in it.
    /// Called on guests' threads.
    notify: Box<dyn Fn() + Send + Sync>,
    commands: SegQueue<ViewCommand>,
    next_view: AtomicU64,
    started: Duration,
    theme: AtomicU32,
    /// The interface is gone: nothing is queued for it any more.
    closed: AtomicBool,
    /// Small things both sides read and write. Never held while anything
    /// else happens, so that the interface's thread can take it too.
    shared: Mutex<Shared>,
}

impl Inner {
    /// Queues a command for the interface. `false` when there is none any
    /// more: the command is dropped, and with it whatever it carried.
    fn send(&self, command: ViewCommand) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        self.commands.push(command);
        (self.notify)();
        true
    }
}

/// The window system. Clones are the same system: one goes to the
/// [`GuiCtxBuilder::window_system`](crate::GuiCtxBuilder::window_system),
/// one stays with the interface's thread.
#[derive(Clone)]
pub struct ViewSystem {
    inner: Arc<Inner>,
}

impl ViewSystem {
    /// `notify` is called, on a guest's thread, each time a command is
    /// queued. It has to get the interface's thread to call
    /// [`ViewSystem::next_command`] soon, and must not wait for it.
    pub fn new(options: ViewOptions, notify: impl Fn() + Send + Sync + 'static) -> Self {
        let theme = options.info.theme;
        Self {
            inner: Arc::new(Inner {
                options,
                notify: Box::new(notify),
                commands: SegQueue::new(),
                next_view: AtomicU64::new(1),
                started: crate::clock::now(),
                theme: AtomicU32::new(theme as u32),
                closed: AtomicBool::new(false),
                shared: Mutex::new(Shared::default()),
            }),
        }
    }

    // -- for the interface's thread ---------------------------------------------

    /// The next thing a guest asked for, if there is any. Never waits.
    pub fn next_command(&self) -> Option<ViewCommand> {
        self.inner.commands.pop()
    }

    /// The interface is going away. Guests waiting for it are answered that
    /// there is no window, what is queued is dropped (which answers what was
    /// pending in it), and nothing more is queued.
    pub fn shut_down(&self) {
        self.inner.closed.store(true, Ordering::Release);
        while self.inner.commands.pop().is_some() {}
    }

    /// What the displays are now. Tell the guests that they changed with
    /// [`InstanceSink::displays_changed`].
    pub fn set_displays(&self, displays: Vec<DisplayData>) {
        lock(&self.inner.shared).displays = displays;
    }

    /// The theme the interface is in now. Tell the guests with
    /// [`InstanceSink::theme_changed`], and each window with its sink.
    pub fn set_theme(&self, theme: Theme) {
        self.inner.theme.store(theme as u32, Ordering::Release);
    }

    /// A clipboard has something else on it, which is not known here yet
    /// (see [`ViewSystem::clipboard_fetched`]). `has_text` is what could be
    /// told without reading it. Returns what to tell the guests with
    /// [`InstanceSink::clipboard_changed`].
    pub fn clipboard_changed(
        &self,
        selection: ClipboardSelection,
        has_text: bool,
    ) -> ClipboardState {
        let mut shared = lock(&self.inner.shared);
        let clipboard = shared.clipboard(selection);
        clipboard.items.clear();
        clipboard.has_text = has_text;
        clipboard.fetched = false;
        clipboard.sequence = clipboard.sequence.wrapping_add(1);
        state_of(clipboard)
    }

    /// What a clipboard holds, read at last. `changed` when this is also the
    /// first the interface hears of the content (a user pasting something
    /// copied elsewhere): the guests are then to be told, with what is
    /// returned.
    pub fn clipboard_fetched(
        &self,
        selection: ClipboardSelection,
        items: Vec<ClipboardData>,
        changed: bool,
    ) -> ClipboardState {
        let mut shared = lock(&self.inner.shared);
        let clipboard = shared.clipboard(selection);
        clipboard.has_text = items.iter().any(|item| item.mime_type == "text/plain");
        clipboard.items = items;
        clipboard.fetched = true;
        if changed {
            clipboard.sequence = clipboard.sequence.wrapping_add(1);
        }
        state_of(clipboard)
    }

    /// The time on the clock guests see, for stamping what cannot be
    /// reported through a sink.
    pub fn time(&self) -> u64 {
        crate::clock::now()
            .saturating_sub(self.inner.started)
            .as_nanos() as u64
    }
}

fn state_of(clipboard: &Clipboard) -> ClipboardState {
    let has_text = clipboard.has_text
        || clipboard
            .items
            .iter()
            .any(|item| item.mime_type == "text/plain");
    let mut flags = 0;
    if has_text {
        flags |= clipboard_flags::HAS_TEXT;
    }
    if !clipboard.fetched {
        // A read answers `Pending` until the interface has been asked for
        // the content: a guest is told so, and that there may be text.
        flags |= clipboard_flags::NEEDS_REQUEST;
    }
    ClipboardState {
        sequence: clipboard.sequence,
        flags,
    }
}

/// The guest's side of a view.
struct ViewWindow {
    inner: Arc<Inner>,
    view: ViewId,
    selector: String,
    /// The frame the interface has been told of and has not taken yet.
    frame: ViewFrame,
}

impl HostWindow for ViewWindow {
    fn apply(&self, change: WindowChange) {
        let change = match change {
            WindowChange::Pixels(frame) => {
                // A guest can draw faster than an interface shows. Frames
                // weigh megabytes: they are not queued, the newest takes the
                // place of the one that waits, and the interface is told
                // once that there is one.
                let mut slot = lock(&self.frame.slot);
                let waiting = slot.is_some();
                *slot = Some(if waiting {
                    frame.replacing_an_unseen_frame()
                } else {
                    frame
                });
                drop(slot);
                if !waiting {
                    self.inner.send(ViewCommand::Frame {
                        view: self.view,
                        frame: self.frame.clone(),
                    });
                }
                return;
            }
            change => change,
        };
        // A change the interface is no longer there for is dropped, and a
        // request in it is answered by that.
        self.inner.send(ViewCommand::Change {
            view: self.view,
            change,
        });
    }

    fn surface_selector(&self) -> String {
        self.selector.clone()
    }
}

impl Drop for ViewWindow {
    fn drop(&mut self) {
        self.inner.send(ViewCommand::Destroy { view: self.view });
    }
}

impl WindowSystem for ViewSystem {
    fn info(&self) -> SystemInfo {
        let mut info = self.inner.options.info.clone();
        info.theme =
            Theme::from_u32(self.inner.theme.load(Ordering::Acquire)).unwrap_or(Theme::Unknown);
        info
    }

    fn now(&self) -> u64 {
        self.time()
    }

    fn displays(&self) -> Vec<DisplayData> {
        lock(&self.inner.shared).displays.clone()
    }

    fn attach(&self, instance: InstanceSink) {
        self.inner.send(ViewCommand::Attach(instance));
    }

    fn create_window(
        &self,
        attributes: WindowAttributes,
        sink: WindowSink,
    ) -> Result<Box<dyn HostWindow>, Status> {
        let inner = &self.inner;
        let view = inner.next_view.fetch_add(1, Ordering::AcqRel);
        let watch: SinkWatch = sink.watch();
        let answer = Arc::new(Answer {
            value: ArrayQueue::new(1),
            waiter: std::thread::current(),
            abandoned: AtomicBool::new(false),
        });
        let sent = inner.send(ViewCommand::Create {
            view,
            attributes,
            sink: Box::new(sink),
            answer: ViewAnswer {
                answer: Arc::clone(&answer),
                given: false,
            },
        });
        if !sent {
            return Err(Status::Unsupported);
        }

        let deadline = crate::clock::now() + inner.options.answer_timeout;
        let answered = loop {
            if let Some(answered) = answer.value.pop() {
                break answered;
            }
            let gone = inner.closed.load(Ordering::Acquire);
            if gone || watch.is_closed() || crate::clock::now() >= deadline {
                // From here on an answer is refused. One that was given just
                // before is found by looking once more.
                answer.abandoned.store(true, Ordering::Release);
                match answer.value.pop() {
                    Some(Ok(_)) => {
                        inner.send(ViewCommand::Destroy { view });
                    }
                    Some(Err(_)) => {}
                    // Whatever the interface made of the request so far, it
                    // is to forget.
                    None => {
                        inner.send(ViewCommand::Destroy { view });
                    }
                }
                return Err(if gone {
                    Status::Unsupported
                } else {
                    Status::Error
                });
            }
            std::thread::park_timeout(ANSWER_SLICE);
        };
        let selector = answered?;
        Ok(Box::new(ViewWindow {
            inner: Arc::clone(inner),
            view,
            selector,
            frame: ViewFrame {
                slot: Arc::new(Mutex::new(None)),
            },
        }))
    }

    fn clipboard_state(&self, selection: ClipboardSelection) -> ClipboardState {
        state_of(lock(&self.inner.shared).clipboard(selection))
    }

    fn clipboard_read(
        &self,
        selection: ClipboardSelection,
        mime_type: &str,
    ) -> Result<Vec<u8>, Status> {
        let mut shared = lock(&self.inner.shared);
        let clipboard = shared.clipboard(selection);
        if !clipboard.fetched {
            return Err(Status::Pending);
        }
        clipboard
            .items
            .iter()
            .find(|item| item.mime_type == mime_type)
            .map(|item| item.data.clone())
            .ok_or(Status::NotFound)
    }

    fn clipboard_request(
        &self,
        selection: ClipboardSelection,
        mime_type: &str,
        request: PendingRequest,
    ) {
        // Dropped with the command when the interface is gone, which is the
        // answer: cancelled.
        self.inner.send(ViewCommand::ClipboardRequest {
            selection,
            mime_type: mime_type.to_owned(),
            request,
        });
    }

    fn clipboard_write(
        &self,
        selection: ClipboardSelection,
        items: Vec<ClipboardData>,
        request: PendingRequest,
    ) {
        {
            // What a guest put there is known here at once, whatever the
            // system's clipboard makes of it.
            let mut shared = lock(&self.inner.shared);
            let clipboard = shared.clipboard(selection);
            clipboard.items.clone_from(&items);
            clipboard.has_text = false;
            clipboard.fetched = true;
            clipboard.sequence = clipboard.sequence.wrapping_add(1);
        }
        self.inner.send(ViewCommand::ClipboardWrite {
            selection,
            items,
            request,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::mpsc;
    use std::time::Instant;

    use super::*;
    use crate::abi::api::{InstanceApi, WindowApi};
    use crate::abi::events::Event;
    use crate::abi::{Handle, OutPtr};
    use crate::headless::HeadlessSystem;
    use crate::host::tests::{Rig, completions, copy, key, paste};
    use crate::{GuiLimits, GuiPolicy};

    /// How the interface answers a guest that wants a window.
    #[derive(Clone, Copy)]
    enum Answering {
        /// With a view of this size.
        Views(u32, u32),
        Refusing(Status),
        /// Not at all: the answer is kept, unanswered.
        Silent,
        /// By dropping the request.
        Dropping,
    }

    #[derive(Default)]
    struct Kept {
        sinks: HashMap<ViewId, Box<WindowSink>>,
        instances: Vec<InstanceSink>,
        /// What was asked, in the order it was asked.
        asked: Vec<String>,
        unanswered: Vec<ViewAnswer>,
        requests: Vec<PendingRequest>,
        /// Frames are not shown when told of, but when the test says.
        slow_frames: bool,
        frames_waiting: Vec<ViewFrame>,
    }

    /// The interface, played by a thread that serves what it is told of and
    /// never waits for a guest.
    struct Interface {
        views: ViewSystem,
        kept: Arc<Mutex<Kept>>,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Interface {
        fn new(answering: Answering, options: ViewOptions) -> Self {
            let (told, announced) = mpsc::channel::<()>();
            let views = ViewSystem::new(options, move || {
                let _ = told.send(());
            });
            let kept = Arc::new(Mutex::new(Kept::default()));
            let stop = Arc::new(AtomicBool::new(false));
            let thread = {
                let (views, kept, stop) = (views.clone(), Arc::clone(&kept), Arc::clone(&stop));
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Acquire) {
                        let _ = announced.recv_timeout(Duration::from_millis(5));
                        while let Some(command) = views.next_command() {
                            serve(&views, &kept, answering, command);
                        }
                    }
                })
            };
            Self {
                views,
                kept,
                stop,
                thread: Some(thread),
            }
        }

        fn rig(&self) -> Rig {
            Rig::on(
                Arc::new(self.views.clone()),
                HeadlessSystem::default(),
                GuiPolicy::default(),
                GuiLimits::default(),
            )
        }

        fn asked(&self) -> Vec<String> {
            lock(&self.kept).asked.clone()
        }

        /// Waits until the interface has been asked `count` things.
        fn until_asked(&self, count: usize) -> Vec<String> {
            let started = Instant::now();
            loop {
                let asked = self.asked();
                if asked.len() >= count {
                    return asked;
                }
                assert!(
                    started.elapsed() < Duration::from_secs(5),
                    "asked only {asked:?}"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }

        /// Reports something of a view, as the interface's thread would.
        fn report(&self, view: ViewId, report: impl FnOnce(&mut WindowSink)) {
            report(lock(&self.kept).sinks.get_mut(&view).expect("a kept view"));
        }
    }

    impl Drop for Interface {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn serve(views: &ViewSystem, kept: &Mutex<Kept>, answering: Answering, command: ViewCommand) {
        let mut kept = lock(kept);
        match command {
            ViewCommand::Attach(instance) => {
                kept.asked.push("attach".to_owned());
                kept.instances.push(instance);
            }
            ViewCommand::Create {
                view,
                attributes,
                mut sink,
                answer,
            } => {
                kept.asked
                    .push(format!("create {view} {:?}", attributes.target));
                match answering {
                    Answering::Views(width, height) => {
                        sink.resized(width, height, 2.0);
                        kept.sinks.insert(view, sink);
                        if answer.created(format!("view:{view}")).is_err() {
                            kept.sinks.remove(&view);
                            kept.asked.push(format!("abandoned {view}"));
                        }
                    }
                    Answering::Refusing(status) => answer.refused(status),
                    Answering::Silent => {
                        kept.sinks.insert(view, sink);
                        kept.unanswered.push(answer);
                    }
                    Answering::Dropping => {}
                }
            }
            ViewCommand::Change { view, change } => {
                let what = match change {
                    WindowChange::Title(title) => format!("title {title:?}"),
                    WindowChange::Redraw { while_drawing } => {
                        format!(
                            "redraw{}",
                            if while_drawing { " while drawing" } else { "" }
                        )
                    }
                    WindowChange::Fullscreen { options, request } => {
                        kept.requests.push(request);
                        format!("fullscreen {}", options.is_some())
                    }
                    _ => "something else".to_owned(),
                };
                kept.asked.push(format!("change {view}: {what}"));
            }
            ViewCommand::Frame { view, frame } => {
                if kept.slow_frames {
                    kept.frames_waiting.push(frame);
                } else if let Some(frame) = frame.take() {
                    kept.asked.push(format!(
                        "frame {view}: {}x{} damage {}",
                        frame.width,
                        frame.height,
                        frame.damage.len()
                    ));
                }
            }
            ViewCommand::Destroy { view } => {
                kept.sinks.remove(&view);
                kept.asked.push(format!("destroy {view}"));
            }
            ViewCommand::ClipboardRequest {
                selection,
                mime_type,
                request,
            } => {
                kept.asked.push(format!("fetch {mime_type}"));
                // What the user copied elsewhere, read at last.
                views.clipboard_fetched(
                    selection,
                    vec![ClipboardData {
                        mime_type: "text/plain".to_owned(),
                        data: b"from elsewhere".to_vec(),
                    }],
                    false,
                );
                request.complete(Status::Success);
            }
            ViewCommand::ClipboardWrite {
                selection,
                items,
                request,
            } => {
                kept.asked.push(format!("write {} items", items.len()));
                request.complete(Status::Success);
                let state = views.clipboard_state(selection);
                for instance in &kept.instances {
                    instance.clipboard_changed(selection, state);
                }
            }
        }
    }

    const OUT: u32 = 64;

    fn open(rig: &Rig) -> Result<Handle, Status> {
        let status = rig
            .call(|host, cx| host.instance_create_window(cx, rig.instance, None, OutPtr::new(OUT)))
            .expect("no fault");
        if status == Status::Success {
            Ok(rig.read_u32(OUT))
        } else {
            Err(status)
        }
    }

    fn selector(rig: &Rig, window: Handle) -> String {
        let status = rig
            .call(|host, cx| {
                host.window_get_surface_selector(cx, window, 256, 128, OutPtr::new(OUT))
            })
            .expect("no fault");
        assert_eq!(status, Status::Success);
        String::from_utf8(rig.read(256, rig.read_u32(OUT) as usize)).expect("UTF-8")
    }

    #[test]
    fn a_window_is_a_view_the_interface_chose() {
        let interface = Interface::new(Answering::Views(640, 480), ViewOptions::default());
        let rig = interface.rig();
        let window = open(&rig).expect("a window");
        assert_eq!(interface.until_asked(2), ["attach", "create 1 \"\""]);
        // The view's size is the window's first word, and its selector is
        // the one the interface gave.
        let events = rig.events();
        let [(header, Event::WindowResized(size))] = &events[..] else {
            panic!("{events:?}");
        };
        assert_eq!(header.window, window);
        assert_eq!(
            (size.width, size.height, size.scale_factor),
            (640, 480, 2.0)
        );
        assert_eq!(selector(&rig, window), "view:1");
    }

    #[test]
    fn what_happens_on_the_interface_reaches_the_guest() {
        let interface = Interface::new(Answering::Views(100, 100), ViewOptions::default());
        let rig = interface.rig();
        open(&rig).expect("a window");
        rig.events();
        interface.report(1, |sink| {
            sink.focused(true);
            sink.key(key(PhysicalKey::KeyA, true));
            sink.key(key(PhysicalKey::KeyA, false));
        });
        assert_eq!(rig.names(), ["WindowFocusGained", "KeyDown", "KeyUp"]);
    }

    #[test]
    fn a_redraw_asked_for_while_drawing_the_last_one_says_so() {
        let interface = Interface::new(Answering::Views(100, 100), ViewOptions::default());
        let rig = interface.rig();
        let window = open(&rig).expect("a window");
        let ask = || {
            let status = rig.call(|host, cx| host.window_request_redraw(cx, window));
            assert_eq!(status, Ok(Status::Success));
        };
        let wait = || {
            let status = rig
                .call(|host, cx| host.instance_wait_events(cx, rig.instance, 0, OutPtr::new(OUT)));
            assert_eq!(status, Ok(Status::Success));
        };
        let told = |interface: &Interface| {
            interface.report(1, |sink| sink.redraw_requested());
        };
        let redrawn = || {
            rig.names()
                .iter()
                .any(|name| name == "WindowRedrawRequested")
        };

        // Never told to draw.
        ask();
        // Told to draw, and asking again before it waits for events.
        told(&interface);
        assert!(redrawn());
        ask();
        // Once it has waited, even without blocking, it is done drawing.
        wait();
        ask();
        // A redraw it was told of but has not read yet does not count...
        told(&interface);
        ask();
        // ...until it reads it.
        assert!(redrawn());
        ask();
        assert_eq!(
            interface.until_asked(7)[2..],
            [
                "change 1: redraw",
                "change 1: redraw while drawing",
                "change 1: redraw",
                "change 1: redraw",
                "change 1: redraw while drawing",
            ]
        );
    }

    #[test]
    fn what_a_guest_asks_arrives_in_order_and_its_window_goes_last() {
        let interface = Interface::new(Answering::Views(100, 100), ViewOptions::default());
        let rig = interface.rig();
        let window = open(&rig).expect("a window");
        for title in ["one", "two"] {
            let status = rig.call(|host, cx| host.window_set_title(cx, window, title));
            assert_eq!(status, Ok(Status::Success));
        }
        assert_eq!(
            rig.call(|host, cx| host.window_request_redraw(cx, window)),
            Ok(Status::Success)
        );
        rig.call(|host, cx| host.window_destroy(cx, window));
        assert_eq!(
            interface.until_asked(6),
            [
                "attach",
                "create 1 \"\"",
                "change 1: title \"one\"",
                "change 1: title \"two\"",
                "change 1: redraw",
                "destroy 1",
            ]
        );
        // The view's sink went with it.
        assert!(lock(&interface.kept).sinks.is_empty());
    }

    #[test]
    fn a_view_the_interface_will_not_give_is_a_status() {
        let interface = Interface::new(Answering::Refusing(Status::Busy), ViewOptions::default());
        let rig = interface.rig();
        assert_eq!(open(&rig), Err(Status::Busy));
        assert_eq!(rig.scope.host().usage(), (1, 0, 0));
        assert!(rig.events().is_empty());
    }

    #[test]
    fn a_request_the_interface_drops_is_refused() {
        let interface = Interface::new(Answering::Dropping, ViewOptions::default());
        let rig = interface.rig();
        let started = Instant::now();
        assert_eq!(open(&rig), Err(Status::Error));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn an_interface_that_does_not_answer_does_not_hang_a_guest() {
        let interface = Interface::new(
            Answering::Silent,
            ViewOptions {
                answer_timeout: Duration::from_millis(150),
                ..ViewOptions::default()
            },
        );
        let rig = interface.rig();
        let started = Instant::now();
        assert_eq!(open(&rig), Err(Status::Error));
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(150), "{waited:?}");
        assert!(waited < Duration::from_secs(3), "{waited:?}");
        assert_eq!(rig.scope.host().usage(), (1, 0, 0));
        // The interface is told to forget the view, and an answer that comes
        // after all is not taken.
        assert_eq!(
            interface.until_asked(3),
            ["attach", "create 1 \"\"", "destroy 1"]
        );
        let answer = lock(&interface.kept).unanswered.pop().expect("kept");
        assert_eq!(answer.created("too late"), Err(Abandoned));
    }

    #[test]
    fn a_guest_stopped_while_it_waits_for_its_window_is_let_go() {
        let interface = Interface::new(Answering::Silent, ViewOptions::default());
        let rig = interface.rig();
        let scope = rig.scope.clone();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            scope.close();
        });
        let started = Instant::now();
        assert!(open(&rig).is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
        stopper.join().expect("joins");
    }

    #[test]
    fn an_interface_that_is_gone_is_asked_nothing() {
        let interface = Interface::new(Answering::Views(100, 100), ViewOptions::default());
        let rig = interface.rig();
        let window = open(&rig).expect("a window");
        rig.events();
        interface.until_asked(2);
        interface.views.shut_down();
        assert_eq!(open(&rig), Err(Status::Unsupported));
        // A request that would have gone to it is answered all the same.
        let status = rig.call(|host, cx| host.window_set_fullscreen(cx, window, None, 7));
        assert_eq!(status, Ok(Status::Success));
        assert_eq!(completions(&rig), [(7, Status::Cancelled)]);
        assert_eq!(interface.asked().len(), 2);
    }

    #[test]
    fn a_request_is_the_interface_s_to_answer() {
        let interface = Interface::new(Answering::Views(100, 100), ViewOptions::default());
        let rig = interface.rig();
        let window = open(&rig).expect("a window");
        rig.events();
        let status = rig.call(|host, cx| host.window_set_fullscreen(cx, window, None, 7));
        assert_eq!(status, Ok(Status::Success));
        interface.until_asked(3);
        assert!(completions(&rig).is_empty());
        let request = lock(&interface.kept).requests.pop().expect("kept");
        request.complete(Status::Denied);
        assert_eq!(completions(&rig), [(7, Status::Denied)]);
    }

    #[test]
    fn the_clipboard_is_known_here_or_asked_for() {
        let mut options = ViewOptions::default();
        options.info.features.push(FeatureName::ClipboardText);
        let interface = Interface::new(Answering::Views(100, 100), options);
        let rig = interface.rig();
        open(&rig).expect("a window");
        rig.events();

        // What the guest copies is its own to read back at once, and goes to
        // the interface to be put on the system's clipboard.
        assert_eq!(copy(&rig, "from the guest", 1), Status::Success);
        assert_eq!(
            paste(&rig, 64),
            (Status::Success, Some("from the guest".to_owned()))
        );
        assert_eq!(interface.until_asked(3)[2], "write 1 items");
        let started = Instant::now();
        while completions(&rig).is_empty() {
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(1));
        }

        // The user copies something elsewhere: all that is known is that the
        // clipboard changed. The user then pastes, which is what lets a
        // guest read it; reading has to ask the interface first.
        let state = interface
            .views
            .clipboard_changed(ClipboardSelection::Clipboard, true);
        assert_eq!(
            state.flags,
            clipboard_flags::HAS_TEXT | clipboard_flags::NEEDS_REQUEST
        );
        for instance in &lock(&interface.kept).instances {
            instance.clipboard_changed(ClipboardSelection::Clipboard, state);
        }
        interface.report(1, |sink| {
            sink.focused(true);
            sink.key(KeyInput {
                modifiers: Some(modifiers::CONTROL),
                ..key(PhysicalKey::ControlLeft, true)
            });
            sink.key(key(PhysicalKey::KeyV, true));
        });
        rig.events();
        assert_eq!(paste(&rig, 64), (Status::Pending, None));
        let status = rig.call(|host, cx| {
            host.instance_request_clipboard(
                cx,
                rig.instance,
                ClipboardSelection::Clipboard,
                "text/plain",
                2,
            )
        });
        assert_eq!(status, Ok(Status::Success));
        let started = Instant::now();
        while completions(&rig) != [(2, Status::Success)] {
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            interface.asked().last().map(String::as_str),
            Some("fetch text/plain")
        );
        assert_eq!(
            paste(&rig, 64),
            (Status::Success, Some("from elsewhere".to_owned()))
        );
    }

    fn draw(rig: &Rig, window: Handle, width: u32, height: u32, damage: &[Rect]) -> Status {
        const AT: u32 = 8192;
        let size = width * height * 4;
        rig.write(AT, &vec![0x7F; size as usize]);
        let frame = crate::abi::structs::PixelFrame {
            width,
            height,
            stride: 0,
            format: PixelFormat::Rgba8,
            alpha: PixelAlpha::Opaque,
            pixels: AT,
            pixels_size: size,
        };
        rig.call(|host, cx| host.window_present_pixels(cx, window, &frame, damage))
            .expect("no fault")
    }

    #[test]
    fn frames_drawn_faster_than_they_are_shown_replace_each_other() {
        let mut options = ViewOptions::default();
        options.info.features.push(FeatureName::PixelPresent);
        let interface = Interface::new(Answering::Views(100, 100), options);
        let rig = interface.rig();
        let window = open(&rig).expect("a window");
        let part = [Rect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        }];

        // An interface that shows each frame as it comes sees each, with
        // what its guest said changed.
        assert_eq!(draw(&rig, window, 4, 4, &part), Status::Success);
        assert_eq!(interface.until_asked(3)[2], "frame 1: 4x4 damage 1");

        // One that is busy is told once, however many frames come meanwhile,
        // and finds the newest when it looks. What that one says changed was
        // said of a frame nobody saw: all of it is to be shown.
        lock(&interface.kept).slow_frames = true;
        for size in [5, 6, 7] {
            assert_eq!(draw(&rig, window, size, size, &part), Status::Success);
        }
        let started = Instant::now();
        while lock(&interface.kept).frames_waiting.is_empty() {
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(1));
        }
        std::thread::sleep(Duration::from_millis(30));
        let waiting = std::mem::take(&mut lock(&interface.kept).frames_waiting);
        assert_eq!(waiting.len(), 1);
        let newest = waiting[0].take().expect("a frame");
        assert_eq!((newest.width, newest.height), (7, 7));
        assert!(newest.damage.is_empty());
        assert!(waiting[0].take().is_none());

        // After which the next frame is news again.
        assert_eq!(draw(&rig, window, 8, 8, &part), Status::Success);
        let started = Instant::now();
        while lock(&interface.kept).frames_waiting.is_empty() {
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(1));
        }
        let frame = lock(&interface.kept).frames_waiting[0]
            .take()
            .expect("a frame");
        assert_eq!((frame.width, frame.damage.len()), (8, 1));
    }

    #[test]
    fn the_displays_are_what_the_interface_last_said() {
        let mut options = ViewOptions::default();
        options.info.features.push(FeatureName::Displays);
        let interface = Interface::new(Answering::Views(100, 100), options);
        let rig = interface.rig();
        let count = |rig: &Rig| {
            let status = rig.call(|host, cx| {
                host.instance_get_displays(cx, rig.instance, 4, OutPtr::new(512), OutPtr::new(OUT))
            });
            assert_eq!(status, Ok(Status::Success));
            rig.read_u32(OUT)
        };
        assert_eq!(count(&rig), 0);
        interface.views.set_displays(vec![DisplayData {
            id: 7,
            name: "The page".to_owned(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            work_area: (0, 0, 1920, 1080),
            scale_factor: 2.0,
            refresh_rate: 60_000,
            primary: true,
        }]);
        assert_eq!(count(&rig), 1);
    }
}
