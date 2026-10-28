//! The host side of the API: what every window system shares.
//!
//! One [`Host`] exists per scope (see [`crate::GuiScope`]). It owns the handle
//! table that maps the guest's `WGUI*` handles to host objects, and it
//! implements the generated `*Api` traits, one file per object of the C API.
//! Below it sits a [`WindowSystem`](crate::system::WindowSystem); between the
//! two, [`sink`] and [`shared`] carry what happens to windows to the guests
//! that poll for it.

mod clipboard;
mod cursor;
mod display;
mod global;
mod instance;
pub(crate) mod request;
pub(crate) mod shared;
pub(crate) mod sink;
mod table;
#[cfg(test)]
pub(crate) mod tests;
pub(crate) mod wait;
#[cfg(all(target_arch = "wasm32", feature = "js"))]
pub(crate) mod web_wait;
mod window;

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use wasmer::{FunctionEnvMut, MemoryOps, RuntimeError};

use crate::abi::enums::FeatureName;
use crate::abi::{AbiError, AbiResult, Cx, GuestMemory, Handle, ImportResult};
use crate::ctx::{ClipboardRead, CtxInner};
use crate::env::GuiEnv;
use crate::system::{CursorImage, HostWindow, WindowChange};

pub(crate) use table::{Object, ObjectKind};

use shared::{InstanceShared, WindowShared};

/// Locks a mutex, ignoring poisoning: a panic inside one import is turned into
/// a guest trap by [`enter`], and must not take the rest of the scope's state
/// down with it.
///
/// Every lock taken through here is held for a few instructions, and never
/// while waiting for anything: not for another lock, not for the window
/// system, not for a guest. That is what lets the browser's page thread take
/// them too (see below).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// In a browser one of the threads is the page's own, which is not allowed to
/// wait: a lock that made it wait would trap, and take the embedder down. So
/// here nobody waits for a lock. Whoever finds one taken tries again, which
/// is over in the few instructions the holder needs.
#[cfg(target_arch = "wasm32")]
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    loop {
        match mutex.try_lock() {
            Ok(guard) => return guard,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => return poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => std::hint::spin_loop(),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn logging() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("WASMER_GUI_LOG").is_some())
}

/// Diagnostics for embedders, enabled with `WASMER_GUI_LOG=1`. Guests learn
/// about their mistakes from the statuses they get, not from this.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn log(message: impl FnOnce() -> String) {
    if logging() {
        eprintln!("[wasmer-gui] {}", message());
    }
}

/// A browser has no environment to switch this on with, and no standard
/// error to write to: the messages are not even made.
#[cfg(target_arch = "wasm32")]
pub(crate) fn log(_message: impl FnOnce() -> String) {}

/// A guest's connection to the window system: `WGUIInstance`.
pub(crate) struct Instance {
    pub(crate) shared: Arc<InstanceShared>,
    pub(crate) application_id: String,
}

/// `WGUIWindow`.
pub(crate) struct Window {
    /// The instance the window was opened on.
    pub(crate) instance: Arc<InstanceShared>,
    pub(crate) shared: Arc<WindowShared>,
    /// The window system's side. Gone once the window is destroyed.
    host: Mutex<Option<Box<dyn HostWindow>>>,
    /// The name a WebGPU surface for the window is created with.
    selector: OnceLock<String>,
    /// The custom cursor it shows, if any.
    cursor: Mutex<Handle>,
}

impl Window {
    /// Asks the window system for a change. Nothing happens once the window
    /// is destroyed.
    pub(crate) fn apply(&self, change: WindowChange) {
        if let Some(host) = lock(&self.host).as_ref() {
            host.apply(change);
        }
    }

    /// Destroys the window: the window system's side goes, and nothing more
    /// is reported for it.
    pub(crate) fn destroy(&self) {
        if self.shared.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.instance.remove_window(&self.shared);
        let host = lock(&self.host).take();
        drop(host);
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        self.destroy();
    }
}

/// `WGUIDisplay`: a display of the window system, by its id there.
pub(crate) struct Display {
    pub(crate) id: u64,
}

/// `WGUICursor`.
pub(crate) struct Cursor {
    pub(crate) image: Arc<CursorImage>,
}

/// Everything the GUI imports of one scope share.
pub(crate) struct Host {
    pub(crate) ctx: Arc<CtxInner>,
    table: Mutex<table::Table>,
    closed: AtomicBool,
}

impl Host {
    pub(crate) fn new(ctx: Arc<CtxInner>) -> Self {
        Self {
            closed: AtomicBool::new(ctx.stopped.load(Ordering::Acquire)),
            table: Mutex::new(table::Table::default()),
            ctx,
        }
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire) || self.ctx.stopped.load(Ordering::Acquire)
    }

    /// `wguiInstanceWake` on every instance of the scope. The table is only
    /// held to list them, never while waking.
    pub(crate) fn wake(&self) {
        if self.is_closed() {
            return;
        }
        let instances = lock(&self.table).all(ObjectKind::Instance);
        for object in instances {
            if let Object::Instance(instance) = object {
                instance.shared.wait.wake();
            }
        }
    }

    /// Ends the scope: everything in it is destroyed, and its guests are
    /// stopped at their next call, or now if they are waiting for input.
    pub(crate) fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let objects = lock(&self.table).clear();
        for object in &objects {
            match object {
                Object::Instance(instance) => {
                    instance.shared.closed.store(true, Ordering::Release);
                    instance.shared.wait.notify();
                }
                Object::Window(window) => window.destroy(),
                Object::Display(_) | Object::Cursor(_) => {}
            }
        }
    }

    /// (instances, windows, cursors) alive in this scope.
    pub(crate) fn usage(&self) -> (usize, usize, usize) {
        let table = lock(&self.table);
        (
            table.count(ObjectKind::Instance),
            table.count(ObjectKind::Window),
            table.count(ObjectKind::Cursor),
        )
    }

    /// What a guest of this scope may use: what the window system can do, as
    /// far as the embedder allows it.
    pub(crate) fn features(&self) -> Vec<FeatureName> {
        use FeatureName::*;
        let policy = &self.ctx.policy;
        let clipboard = policy.clipboard_write || policy.clipboard_read != ClipboardRead::Deny;
        self.ctx
            .system
            .info()
            .features
            .into_iter()
            .filter(|feature| match feature {
                TopLevelWindows => policy.top_level_windows,
                WindowPosition => policy.window_position,
                WindowStates => policy.window_states,
                FullscreenBorderless | FullscreenExclusive => policy.fullscreen,
                TextInput | ScreenKeyboard | TextSurrounding => policy.text_input,
                RawInput => policy.raw_input,
                RawInputUnfocused => policy.raw_input && policy.raw_input_unfocused,
                CustomCursors => policy.custom_cursors,
                CursorWarp => policy.cursor_warp,
                CursorConfine | CursorLock => policy.cursor_grab,
                ClipboardText | ClipboardData | PrimarySelection => clipboard,
                ClipboardSyncRead => policy.clipboard_read != ClipboardRead::Deny,
                AttentionRequest => policy.attention,
                _ => true,
            })
            .collect()
    }

    pub(crate) fn has(&self, feature: FeatureName) -> bool {
        self.features().contains(&feature)
    }

    // -- handle table ---------------------------------------------------------

    fn insert(&self, object: Object, instance: Handle) -> Option<Handle> {
        lock(&self.table).insert(object, instance)
    }

    fn count(&self, kind: ObjectKind) -> usize {
        lock(&self.table).count(kind)
    }

    /// The instance behind `handle`, if it is live and one.
    fn instance(&self, handle: Handle) -> Option<Arc<Instance>> {
        match lock(&self.table).get(handle) {
            Some(Object::Instance(instance)) => Some(instance),
            _ => None,
        }
    }

    fn window(&self, handle: Handle) -> Option<Arc<Window>> {
        match lock(&self.table).get(handle) {
            Some(Object::Window(window)) => Some(window),
            _ => None,
        }
    }

    fn display(&self, handle: Handle) -> Option<Arc<Display>> {
        match lock(&self.table).get(handle) {
            Some(Object::Display(display)) => Some(display),
            _ => None,
        }
    }

    fn cursor(&self, handle: Handle) -> Option<Arc<Cursor>> {
        match lock(&self.table).get(handle) {
            Some(Object::Cursor(cursor)) => Some(cursor),
            _ => None,
        }
    }

    /// The instance `handle` was made on, as a handle.
    fn instance_of(&self, handle: Handle) -> Handle {
        lock(&self.table).instance_of(handle)
    }

    /// Destroys an instance and what was made on it. Nothing happens for a
    /// handle that is not a live instance.
    pub(crate) fn destroy_instance(&self, handle: Handle) {
        let (instance, made) = {
            let mut table = lock(&self.table);
            let Some(Object::Instance(instance)) = table.remove(handle, ObjectKind::Instance)
            else {
                return;
            };
            (instance, table.remove_made_on(handle))
        };
        // Its windows first: requests still pending on them are answered
        // while there is still a queue to answer into.
        for object in &made {
            if let Object::Window(window) = object {
                window.destroy();
            }
        }
        instance.shared.closed.store(true, Ordering::Release);
        instance.shared.wait.notify();
    }
}

fn panic_message(panic: &(dyn Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_owned())
}

/// Whether the process whose memory this is was killed. WASIX disables the
/// atomics of a killed process's memory to shake its threads out of their
/// waits; a wait of no duration on an address nobody notifies tells whether
/// that has happened, and nothing else.
fn killed(probe: &MemoryOps) -> bool {
    matches!(
        probe.wait(
            wasmer::MemoryLocation::new_32(u32::MAX),
            Some(Duration::ZERO)
        ),
        Err(wasmer::AtomicsError::AtomicsDisabled)
    )
}

/// How often, at most, asking for events is also where signals reach a
/// program. Waiting for them always is.
const SIGNALS_EVERY: Duration = Duration::from_millis(10);

/// Lets what the system holds for the calling thread reach it: its signals,
/// and being told to end.
///
/// A WASIX runtime delivers both where a program enters the system, and
/// these imports are not that. A program that draws frame after frame, or
/// waits here for its user, may enter the system nowhere else for as long as
/// it runs. So the calls such a program keeps coming back to, asking for
/// events and waiting for them, make the runtime's own yield on its behalf:
/// a handler the program installed runs before the call goes on, and what
/// ends the program ends it here.
///
/// `always` is for a call that may block, which yields once it is over: what
/// a handler changed is then what the program finds when the call returns.
/// A call that is made every frame, or many times a frame, yields first, and
/// every [`SIGNALS_EVERY`] at most.
pub(crate) fn let_signals_in(
    env: &mut FunctionEnvMut<'_, GuiEnv>,
    always: bool,
) -> Result<(), RuntimeError> {
    let data = env.data_mut();
    if data.system_yield.is_none() {
        return Ok(());
    }
    let now = crate::clock::now();
    if !always
        && data
            .yielded
            .is_some_and(|last| now.saturating_sub(last) < SIGNALS_EVERY)
    {
        return Ok(());
    }
    data.yielded = Some(now);
    let Some(system_yield) = data.system_yield.clone() else {
        return Ok(());
    };
    // What it answers is an errno nobody needs. What it fails with is the
    // runtime's own way of ending the program, passed on as it is.
    system_yield.call(env, &[]).map(drop)
}

/// Runs one import: resolves the calling instance's memory and host, decodes
/// through `f`, and turns failures into the right kind of guest-visible
/// outcome.
///
/// - A guest pointer outside linear memory traps, like the segfault it is,
///   whatever the implementation did with the failed copy.
/// - Any other decoding error is a status (or the zero of the two functions
///   that return a value).
/// - A guest that is being stopped is stopped: the call traps.
/// - A panic below traps this call instead of unwinding through the wasm
///   frames into the embedder.
pub(crate) fn enter<R: ImportResult>(
    env: &mut FunctionEnvMut<'_, GuiEnv>,
    function: &'static str,
    f: impl FnOnce(&Host, &mut Cx<'_>) -> AbiResult<R>,
) -> Result<R::Wasm, RuntimeError> {
    // Everything that is the same for every import is in `enter_any`, once.
    // What is made anew for each one is only this: running `f` and keeping
    // what it returns.
    let mut f = Some(f);
    let mut returned = None;
    let failed = enter_any(env, function, &mut |host, cx| {
        let f = f.take().expect("called once");
        returned = Some(f(host, cx)?);
        Ok(())
    })?;
    let result = match (failed, returned) {
        (None, Some(result)) => result,
        (Some(error), _) => R::from_error(&error),
        (None, None) => R::from_error(&AbiError::Stopped),
    };
    Ok(result.into_wasm())
}

/// [`enter`], for any import. `Ok(Some(error))` is a call that could not be
/// decoded, whose result is whatever the import's kind of result has for
/// that.
#[inline(never)]
fn enter_any(
    env: &mut FunctionEnvMut<'_, GuiEnv>,
    function: &'static str,
    f: &mut dyn FnMut(&Host, &mut Cx<'_>) -> AbiResult<()>,
) -> Result<Option<AbiError>, RuntimeError> {
    let stop = || RuntimeError::new(format!("{function}: the program was stopped"));
    let (data, store) = env.data_and_store_mut();
    let Some(memory) = data.memory.as_ref() else {
        return Err(RuntimeError::new(format!(
            "{function}: the GUI was called before the instance was set up"
        )));
    };
    let host = Arc::clone(&data.host);
    if host.is_closed() {
        return Err(stop());
    }
    let probe = data.kill_probe.clone();
    let stopped = || host.is_closed() || probe.as_ref().is_some_and(killed);
    let mut cx = Cx {
        mem: GuestMemory::new(memory.view(&store)),
        owned: &mut data.owned,
        stopped: &stopped,
    };
    let outcome = catch_unwind(AssertUnwindSafe(|| f(&host, &mut cx)));
    // Checked before anything else: an implementation may have turned a
    // failed copy into a status of its own, and a wild pointer still traps.
    if let Some(fault) = cx.mem.take_fault() {
        return Err(RuntimeError::new(format!("{function}: {fault}")));
    }
    match outcome {
        Ok(Ok(())) => Ok(None),
        Ok(Err(error @ AbiError::OutOfBounds { .. })) => {
            Err(RuntimeError::new(format!("{function}: {error}")))
        }
        Ok(Err(AbiError::Stopped)) => Err(stop()),
        Ok(Err(error)) => {
            log(|| format!("{function}: {error}"));
            Ok(Some(error))
        }
        Err(panic) => {
            let message = panic_message(&*panic);
            log(|| format!("{function} panicked: {message}"));
            Err(RuntimeError::new(format!(
                "{function}: the window system host failed: {message}"
            )))
        }
    }
}
