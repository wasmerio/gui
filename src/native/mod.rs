//! Native windows, through [winit](https://docs.rs/winit).
//!
//! A window system's event loop wants the process's main thread, and guests
//! run on threads of their own. [`GuiMainThread`] is how an embedder gives
//! the loop what it wants:
//!
//! ```ignore
//! fn main() -> anyhow::Result<()> {
//!     let (main_thread, windows) = wasmer_gui::native::GuiMainThread::new();
//!     let gui = wasmer_gui::GuiCtx::builder()
//!         .window_system(std::sync::Arc::new(windows))
//!         .build();
//!     // Everything else the program does happens in here, on another
//!     // thread, while this one serves the windows.
//!     main_thread.run(move || run_guests(gui))?;
//!     Ok(())
//! }
//! ```
//!
//! Nothing touches the display until a guest opens a window, so a program
//! that never does runs without one.
//!
//! Guest threads never touch a winit window. What they ask of one is a
//! command for the loop's thread; what they want to know they read from what
//! the loop last reported. winit itself is used in this module only.

mod app;
#[cfg(feature = "clipboard")]
mod clipboard;
mod convert;
#[rustfmt::skip]
mod keys;
#[cfg(feature = "pixels")]
mod pixels;
#[cfg(feature = "webgpu")]
mod surface;

use std::collections::VecDeque;
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use winit::event_loop::{EventLoop, EventLoopProxy};

use crate::system::*;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What happens when the user asks to close a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseBehavior {
    /// The guest is told, and decides. Right for an embedder that has its own
    /// way of ending a guest.
    Request,
    /// The guest is told. If it has not read the request after this long, it
    /// is taken to be stuck and the run ends ([`GuiExit::Closed`]). A guest
    /// that did read it and chose to stay open (to ask about unsaved work)
    /// is left alone.
    RequestThenEnd(Duration),
}

/// How the native window system behaves.
#[derive(Clone, Debug)]
pub struct NativeOptions {
    pub close: CloseBehavior,
    /// Whether a new window comes to the front and takes keyboard focus, as
    /// windows do. Off, it opens behind what the user is working in: for
    /// tests and tools that must not interrupt.
    pub activate: bool,
}

impl Default for NativeOptions {
    fn default() -> Self {
        Self {
            close: CloseBehavior::Request,
            activate: true,
        }
    }
}

/// How a [`GuiMainThread::run`] ended.
#[derive(Debug)]
pub enum GuiExit<T> {
    /// The guest's thread finished with this.
    Finished(T),
    /// The user closed a window and the guest did not answer (see
    /// [`CloseBehavior::RequestThenEnd`]). The guest's thread is still
    /// running: stop it, or exit the process.
    Closed,
}

/// The loop's side of a window, for the guest's side to name it by.
type WindowKey = u64;

/// What a guest thread asks of the loop's thread.
enum Command {
    Create {
        attributes: Box<WindowAttributes>,
        sink: Box<WindowSink>,
        reply: SyncSender<Result<Created, Status>>,
    },
    Change {
        window: WindowKey,
        change: WindowChange,
    },
    Destroy(WindowKey),
    Displays(SyncSender<Vec<DisplayData>>),
    /// The window a surface selector names, for a surface to present to.
    #[cfg(feature = "webgpu")]
    Surface {
        selector: String,
        reply: SyncSender<Option<surface::SurfaceParts>>,
    },
    /// Something that has to happen on the loop's thread.
    #[cfg(feature = "webgpu")]
    Run(Box<dyn FnOnce() + Send>),
    /// The GPU found nothing of a window to be seen, at a time when the
    /// window system had spoken of that `said` times.
    #[cfg(feature = "webgpu")]
    Unseen {
        window: WindowKey,
        said: u64,
    },
    /// A window nobody draws on any more, to be let go of where it was made.
    #[cfg(feature = "webgpu")]
    Release(Arc<winit::window::Window>),
    /// A guest drew a frame for a window: it waits in the slot.
    #[cfg(feature = "pixels")]
    Pixels {
        window: WindowKey,
        frame: FrameSlot,
    },
    /// The guest's thread is done: the loop ends.
    Exit,
}

struct Created {
    key: WindowKey,
    selector: String,
}

/// Where a window's newest frame waits for the loop to show it. Frames that
/// come faster than that replace each other: they are not queued.
#[cfg(feature = "pixels")]
type FrameSlot = Arc<Mutex<Option<PixelFrame>>>;

enum Link {
    /// Nobody has needed a window system yet.
    Waiting,
    /// Somebody does: the main thread is about to start the loop.
    Starting,
    Running(EventLoopProxy<()>),
    /// The loop has ended, or could not start, or was never needed.
    Closed,
}

struct Shared {
    options: NativeOptions,
    started: Instant,
    link: Mutex<Link>,
    /// Signalled when `link` changes.
    changed: Condvar,
    commands: Mutex<VecDeque<Command>>,
    /// The guests connected, for what concerns no window in particular.
    instances: Mutex<Vec<InstanceSink>>,
    #[cfg(feature = "clipboard")]
    clipboard: clipboard::Clipboard,
    /// The clipboard sequence number guests were last told of.
    #[cfg(feature = "clipboard")]
    clipboard_seen: std::sync::atomic::AtomicU32,
}

impl Shared {
    /// Hands a command to the loop, starting the loop if this is the first.
    /// `false` when there is no loop any more: the command is dropped, and
    /// with it whatever was waiting for its answer.
    fn send(&self, command: Command) -> bool {
        let mut link = lock(&self.link);
        match &*link {
            Link::Closed => return false,
            Link::Waiting => {
                *link = Link::Starting;
                self.changed.notify_all();
            }
            Link::Starting | Link::Running(_) => {}
        }
        lock(&self.commands).push_back(command);
        if let Link::Running(proxy) = &*link {
            // Fails only when the loop has just ended; `close` deals with
            // what is left in the queue.
            let _ = proxy.send_event(());
        }
        true
    }

    /// Sends a window back to the loop's thread to be dropped there. Hands
    /// it back when there is no loop any more.
    #[cfg(feature = "webgpu")]
    fn send_window_home(
        &self,
        window: Arc<winit::window::Window>,
    ) -> Result<(), Arc<winit::window::Window>> {
        let mut link = lock(&self.link);
        match &*link {
            Link::Closed | Link::Waiting => return Err(window),
            Link::Starting | Link::Running(_) => {}
        }
        lock(&self.commands).push_back(Command::Release(window));
        if let Link::Running(proxy) = &mut *link {
            let _ = proxy.send_event(());
        }
        Ok(())
    }

    /// The loop is over: nothing more can be asked of it.
    fn close(&self) {
        *lock(&self.link) = Link::Closed;
        self.changed.notify_all();
        // Dropped outside the lock: a command's drop may answer a request.
        let left: Vec<Command> = lock(&self.commands).drain(..).collect();
        drop(left);
    }

    /// Tells every guest what the clipboard holds now, if that is news.
    #[cfg(feature = "clipboard")]
    fn announce_clipboard(&self) {
        let state = self.clipboard.state();
        let known = &self.clipboard_seen;
        if known.swap(state.sequence, std::sync::atomic::Ordering::AcqRel) == state.sequence {
            return;
        }
        let instances = lock(&self.instances).clone();
        for instance in instances {
            instance.clipboard_changed(ClipboardSelection::Clipboard, state);
        }
    }

    /// The guest's thread has finished.
    fn guest_finished(&self) {
        let waiting = matches!(*lock(&self.link), Link::Waiting);
        if waiting {
            self.close();
        } else {
            self.send(Command::Exit);
        }
    }
}

/// The native window system. Give it to a
/// [`GuiCtxBuilder::window_system`](crate::GuiCtxBuilder::window_system);
/// its windows appear while its [`GuiMainThread`] runs.
#[derive(Clone)]
pub struct WinitSystem {
    shared: Arc<Shared>,
}

/// The main thread's part of the native window system.
pub struct GuiMainThread {
    shared: Arc<Shared>,
}

impl GuiMainThread {
    /// A window system, and the handle with which the main thread serves it.
    pub fn new() -> (GuiMainThread, WinitSystem) {
        Self::with_options(NativeOptions::default())
    }

    pub fn with_options(options: NativeOptions) -> (GuiMainThread, WinitSystem) {
        let shared = Arc::new(Shared {
            options,
            started: Instant::now(),
            link: Mutex::new(Link::Waiting),
            changed: Condvar::new(),
            commands: Mutex::new(VecDeque::new()),
            instances: Mutex::new(Vec::new()),
            #[cfg(feature = "clipboard")]
            clipboard: clipboard::Clipboard::new(),
            #[cfg(feature = "clipboard")]
            clipboard_seen: std::sync::atomic::AtomicU32::new(0),
        });
        (
            GuiMainThread {
                shared: Arc::clone(&shared),
            },
            WinitSystem { shared },
        )
    }

    /// Runs `guest` on a thread of its own, and on this thread the window
    /// system's loop, from the moment a guest first needs it. Call it on the
    /// process's main thread; it returns when `guest` has.
    pub fn run<T: Send + 'static>(
        self,
        guest: impl FnOnce() -> T + Send + 'static,
    ) -> Result<GuiExit<T>> {
        let shared = self.shared;
        let guest_thread = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("gui-guest".to_owned())
                .spawn(move || {
                    let result = guest();
                    shared.guest_finished();
                    result
                })
                .context("failed to start the guest's thread")?
        };
        let join = |thread: std::thread::JoinHandle<T>| {
            thread
                .join()
                .map(GuiExit::Finished)
                .map_err(|_| anyhow!("the guest's thread panicked"))
        };

        // Until a guest needs a window system, there is none.
        {
            let mut link = lock(&shared.link);
            loop {
                match &*link {
                    Link::Waiting => {
                        link = shared
                            .changed
                            .wait(link)
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                    }
                    Link::Closed => {
                        drop(link);
                        return join(guest_thread);
                    }
                    Link::Starting | Link::Running(_) => break,
                }
            }
        }

        let event_loop = match EventLoop::<()>::with_user_event().build() {
            Ok(event_loop) => event_loop,
            Err(error) => {
                // No display, or a second loop in this process. The guest
                // goes on without windows: creating one fails.
                crate::host::log(|| format!("no window system: {error}"));
                shared.close();
                return join(guest_thread);
            }
        };
        *lock(&shared.link) = Link::Running(event_loop.create_proxy());
        let mut app = app::App::new(Arc::clone(&shared));
        let ran = event_loop.run_app(&mut app);
        let closed_by_user = app.closed_by_user();
        // The windows go here, on the thread that made them.
        drop(app);
        shared.close();
        ran.context("the window system's loop failed")?;
        if closed_by_user {
            return Ok(GuiExit::Closed);
        }
        join(guest_thread)
    }
}

/// The guest's side of a native window.
struct NativeWindow {
    shared: Arc<Shared>,
    key: WindowKey,
    selector: String,
    #[cfg(feature = "pixels")]
    frame: FrameSlot,
}

impl HostWindow for NativeWindow {
    fn apply(&self, change: WindowChange) {
        #[cfg(feature = "pixels")]
        let change = match change {
            WindowChange::Pixels(frame) => {
                // The newest frame takes the place of one the loop has not
                // shown yet, and the loop is told once that there is one.
                let mut slot = lock(&self.frame);
                let waiting = slot.is_some();
                *slot = Some(if waiting {
                    frame.replacing_an_unseen_frame()
                } else {
                    frame
                });
                drop(slot);
                if !waiting {
                    self.shared.send(Command::Pixels {
                        window: self.key,
                        frame: Arc::clone(&self.frame),
                    });
                }
                return;
            }
            change => change,
        };
        self.shared.send(Command::Change {
            window: self.key,
            change,
        });
    }

    fn surface_selector(&self) -> String {
        self.selector.clone()
    }
}

impl Drop for NativeWindow {
    fn drop(&mut self) {
        self.shared.send(Command::Destroy(self.key));
    }
}

/// Whether winit will talk to a Wayland compositor here, as far as can be
/// told before it does: by the variables it goes by itself.
fn on_wayland() -> bool {
    cfg!(all(unix, not(target_os = "macos")))
        && ["WAYLAND_DISPLAY", "WAYLAND_SOCKET"]
            .iter()
            .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

impl WindowSystem for WinitSystem {
    fn info(&self) -> SystemInfo {
        use FeatureName::*;
        let mac = cfg!(target_os = "macos");
        let wayland = on_wayland();
        let x11 = cfg!(all(unix, not(target_os = "macos"))) && !wayland;
        let mut features = vec![
            Windows,
            TopLevelWindows,
            WindowStates,
            FullscreenBorderless,
            Displays,
            TextInput,
            Touch,
            RawInput,
            CursorShapes,
            CustomCursors,
            Theme,
            AttentionRequest,
        ];
        // What a platform cannot do is left out, not imitated.
        if !wayland {
            features.extend([WindowPosition, CursorWarp]);
        }
        if !mac {
            features.push(CursorConfine);
        }
        if !x11 {
            features.push(CursorLock);
        }
        if cfg!(feature = "clipboard") {
            features.extend([ClipboardText, ClipboardSyncRead]);
        }
        if cfg!(feature = "pixels") {
            features.push(PixelPresent);
        }
        SystemInfo {
            host_kind: HostKind::Desktop,
            features,
            shortcut_modifier: if mac {
                modifiers::SUPER
            } else {
                modifiers::CONTROL
            },
            input_devices: input_devices::KEYBOARD | input_devices::MOUSE,
            theme: crate::system::Theme::Unknown,
            max_cursor_size: 0,
        }
    }

    fn now(&self) -> u64 {
        self.shared.started.elapsed().as_nanos() as u64
    }

    fn displays(&self) -> Vec<DisplayData> {
        let (reply, answer) = mpsc::sync_channel(1);
        if !self.shared.send(Command::Displays(reply)) {
            return Vec::new();
        }
        answer.recv().unwrap_or_default()
    }

    fn attach(&self, instance: InstanceSink) {
        let mut instances = lock(&self.shared.instances);
        instances.retain(|instance| !instance.is_closed());
        instances.push(instance);
    }

    fn create_window(
        &self,
        attributes: WindowAttributes,
        sink: WindowSink,
    ) -> Result<Box<dyn HostWindow>, Status> {
        if !attributes.target.is_empty() {
            // There are no targets here: every window is one of its own.
            return Err(Status::NotFound);
        }
        if !attributes.top_level {
            return Err(Status::Denied);
        }
        let (reply, answer) = mpsc::sync_channel(1);
        if !self.shared.send(Command::Create {
            attributes: Box::new(attributes),
            sink: Box::new(sink),
            reply,
        }) {
            return Err(Status::Unsupported);
        }
        // A command nobody takes is dropped, and its sender with it.
        let created = answer.recv().map_err(|_| Status::Unsupported)??;
        Ok(Box::new(NativeWindow {
            shared: Arc::clone(&self.shared),
            key: created.key,
            selector: created.selector,
            #[cfg(feature = "pixels")]
            frame: FrameSlot::default(),
        }))
    }

    #[cfg(feature = "clipboard")]
    fn clipboard_state(&self, selection: ClipboardSelection) -> ClipboardState {
        match selection {
            ClipboardSelection::Clipboard => self.shared.clipboard.state(),
            ClipboardSelection::Primary => ClipboardState::default(),
        }
    }

    #[cfg(feature = "clipboard")]
    fn clipboard_read(
        &self,
        selection: ClipboardSelection,
        mime_type: &str,
    ) -> Result<Vec<u8>, Status> {
        match selection {
            ClipboardSelection::Clipboard => self.shared.clipboard.read(mime_type),
            ClipboardSelection::Primary => Err(Status::Unsupported),
        }
    }

    #[cfg(feature = "clipboard")]
    fn clipboard_request(
        &self,
        selection: ClipboardSelection,
        mime_type: &str,
        request: PendingRequest,
    ) {
        // Reads answer at once here, so there is nothing to fetch.
        request.complete(match self.clipboard_read(selection, mime_type) {
            Ok(_) => Status::Success,
            Err(status) => status,
        });
    }

    #[cfg(feature = "clipboard")]
    fn clipboard_write(
        &self,
        selection: ClipboardSelection,
        items: Vec<ClipboardData>,
        request: PendingRequest,
    ) {
        if selection != ClipboardSelection::Clipboard {
            return request.complete(Status::Unsupported);
        }
        let status = self.shared.clipboard.write(&items);
        request.complete(status);
        if status == Status::Success {
            let state = self.shared.clipboard.state();
            self.shared
                .clipboard_seen
                .store(state.sequence, std::sync::atomic::Ordering::Release);
            let instances = lock(&self.shared.instances).clone();
            for instance in instances {
                instance.clipboard_changed(selection, state);
            }
        }
    }
}
