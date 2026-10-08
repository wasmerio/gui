//! Embedder-facing configuration and the runtime hooks that hand the GUI
//! imports to guest instances.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use anyhow::{Context, Result, bail};
use wasmer::{
    Exports, Extern, ExternType, Function, FunctionEnv, FunctionType, Imports, Instance, Module,
    StoreMut, Type, Value,
};

use crate::env::GuiEnv;
#[cfg(not(target_arch = "wasm32"))]
use crate::headless::HeadlessSystem;
use crate::host::{Host, lock, log};
use crate::system::WindowSystem;
use crate::{GuiAbi, module_needs_gui};

/// Ceilings on what the guests of one scope may hold. `None` means unlimited.
#[derive(Debug, Clone)]
pub struct GuiLimits {
    /// Connections to the window system (`WGUIInstance`) alive at once.
    pub max_instances: Option<usize>,
    /// Windows alive at once.
    pub max_windows: Option<usize>,
    /// Custom cursors alive at once.
    pub max_cursors: Option<usize>,
    /// Event records an instance may leave unread. Beyond it, the oldest
    /// records that can be recovered from are dropped and the guest is told.
    pub max_pending_events: Option<usize>,
    /// Bytes of one frame a guest presents itself
    /// (`wguiWindowPresentPixels`). A window holds one such frame at most.
    pub max_frame_bytes: Option<usize>,
}

impl Default for GuiLimits {
    fn default() -> Self {
        Self {
            max_instances: Some(16),
            max_windows: Some(64),
            max_cursors: Some(256),
            max_pending_events: Some(1024),
            // 7680 by 4320, four bytes a pixel.
            max_frame_bytes: Some(128 << 20),
        }
    }
}

/// Whether guests may read the clipboard.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ClipboardRead {
    /// Never. A guest can still read back what it wrote itself.
    Deny,
    /// Only right after the user pressed the platform's paste shortcut in
    /// one of the guest's focused windows: what a user means by pasting,
    /// and all a web page gets without asking.
    #[default]
    OnPaste,
    /// Whenever the guest likes.
    Allow,
}

/// What guests are allowed to do with the window system. Everything here is
/// also subject to the window system being able to do it; the two together
/// are what `wguiHasFeature` answers.
#[derive(Debug, Clone)]
pub struct GuiPolicy {
    /// Open windows of their own, beyond the targets the embedder provides.
    pub top_level_windows: bool,
    /// Know and set window positions.
    pub window_position: bool,
    /// Show, hide, minimize, maximize, change borders and level.
    pub window_states: bool,
    pub fullscreen: bool,
    /// Confine or lock the cursor.
    pub cursor_grab: bool,
    /// Move the cursor.
    pub cursor_warp: bool,
    pub custom_cursors: bool,
    /// Input methods and on-screen keyboards.
    pub text_input: bool,
    pub clipboard_write: bool,
    pub clipboard_read: ClipboardRead,
    /// Device input as the hardware reports it.
    pub raw_input: bool,
    /// The same while none of the guest's windows has focus. This lets a
    /// guest watch all keyboard and mouse use: off unless you mean it.
    pub raw_input_unfocused: bool,
    /// Ask for the user's attention.
    pub attention: bool,
}

impl Default for GuiPolicy {
    fn default() -> Self {
        Self {
            top_level_windows: true,
            window_position: true,
            window_states: true,
            fullscreen: true,
            cursor_grab: true,
            cursor_warp: true,
            custom_cursors: true,
            text_input: true,
            clipboard_write: true,
            clipboard_read: ClipboardRead::default(),
            raw_input: true,
            raw_input_unfocused: false,
            attention: true,
        }
    }
}

/// A snapshot of what the guests of a [`GuiCtx`] hold.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GuiUsage {
    /// Scopes that are still open.
    pub scopes: usize,
    pub instances: usize,
    pub windows: usize,
    pub cursors: usize,
}

#[cfg(not(target_arch = "wasm32"))]
fn default_system() -> Arc<dyn WindowSystem> {
    Arc::new(HeadlessSystem::default())
}

#[cfg(target_arch = "wasm32")]
fn default_system() -> Arc<dyn WindowSystem> {
    Arc::new(crate::system::NoWindows)
}

/// Builds a [`GuiCtx`].
#[derive(Default)]
pub struct GuiCtxBuilder {
    limits: GuiLimits,
    policy: GuiPolicy,
    system: Option<Arc<dyn WindowSystem>>,
}

impl GuiCtxBuilder {
    pub fn limits(mut self, limits: GuiLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn policy(mut self, policy: GuiPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Where the guests' windows are. The default is headless: windows work
    /// and can be drawn to, and nobody sees them. (A build for a browser has
    /// no headless windows: there the default is that there are none, until
    /// the page's are given.)
    pub fn window_system(mut self, system: Arc<dyn WindowSystem>) -> Self {
        self.system = Some(system);
        self
    }

    pub fn max_windows(mut self, max: usize) -> Self {
        self.limits.max_windows = Some(max);
        self
    }

    pub fn build(self) -> GuiCtx {
        GuiCtx {
            inner: Arc::new(CtxInner {
                limits: self.limits,
                policy: self.policy,
                system: self.system.unwrap_or_else(default_system),
                stopped: AtomicBool::new(false),
                scopes: Mutex::new(Vec::new()),
                implicit: OnceLock::new(),
            }),
        }
    }
}

pub(crate) struct CtxInner {
    pub(crate) limits: GuiLimits,
    pub(crate) policy: GuiPolicy,
    pub(crate) system: Arc<dyn WindowSystem>,
    pub(crate) stopped: AtomicBool,
    /// Every scope made from this context, for [`GuiCtx::usage`] and
    /// [`GuiRuntimeControl::terminate_all`].
    scopes: Mutex<Vec<Weak<Host>>>,
    /// The scope behind [`GuiCtx::runtime_hooks`].
    implicit: OnceLock<GuiScope>,
}

impl CtxInner {
    fn live_scopes(&self) -> Vec<Arc<Host>> {
        let mut scopes = lock(&self.scopes);
        scopes.retain(|scope| scope.strong_count() > 0);
        scopes.iter().filter_map(Weak::upgrade).collect()
    }
}

impl std::fmt::Debug for CtxInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuiCtx")
            .field("limits", &self.limits)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

/// Shared GUI configuration for the guests of one embedder (one app, one
/// sandbox). Cheap to clone.
#[derive(Clone, Debug)]
pub struct GuiCtx {
    pub(crate) inner: Arc<CtxInner>,
}

impl Default for GuiCtx {
    fn default() -> Self {
        Self::builder().build()
    }
}

impl GuiCtx {
    pub fn builder() -> GuiCtxBuilder {
        GuiCtxBuilder::default()
    }

    pub fn limits(&self) -> &GuiLimits {
        &self.inner.limits
    }

    pub fn policy(&self) -> &GuiPolicy {
        &self.inner.policy
    }

    /// A new scope: the unit an embedder stops as one.
    ///
    /// The guests started with one scope's hooks share handles, so make one
    /// per command an embedder runs (a process and whatever it spawns). When
    /// the embedder kills that command it closes the scope, which is what
    /// frees a guest that sits waiting for input.
    pub fn scope(&self) -> GuiScope {
        let host = Arc::new(Host::new(Arc::clone(&self.inner)));
        lock(&self.inner.scopes).push(Arc::downgrade(&host));
        GuiScope {
            inner: Arc::new(ScopeGuard { host }),
        }
    }

    /// Hooks to register on the WASIX runtime (they implement
    /// `wasmer_wasix::runtime::InstantiationHook` with the `wasix` feature).
    ///
    /// These belong to one scope that lives as long as the context: right for
    /// an embedder that runs one program. One that runs several, and stops
    /// them one by one, uses [`GuiCtx::scope`] for each.
    pub fn runtime_hooks(&self) -> GuiRuntimeHooks {
        self.inner.implicit.get_or_init(|| self.scope()).hooks()
    }

    /// What the guests of this context hold right now.
    pub fn usage(&self) -> GuiUsage {
        let scopes = self.inner.live_scopes();
        let mut usage = GuiUsage::default();
        for scope in scopes {
            if scope.is_closed() {
                continue;
            }
            let (instances, windows, cursors) = scope.usage();
            usage.scopes += 1;
            usage.instances += instances;
            usage.windows += windows;
            usage.cursors += cursors;
        }
        usage
    }

    /// A handle for stopping every guest of this context.
    pub fn runtime_control(&self) -> GuiRuntimeControl {
        GuiRuntimeControl {
            inner: Arc::clone(&self.inner),
        }
    }
}

/// Stops the GUI work of every guest of a context.
#[derive(Clone, Debug)]
pub struct GuiRuntimeControl {
    inner: Arc<CtxInner>,
}

impl GuiRuntimeControl {
    /// Closes every scope, and every scope made later: guests waiting for
    /// input are woken and their windows go away. Sticky.
    pub fn terminate_all(&self) {
        self.inner.stopped.store(true, Ordering::Release);
        for scope in self.inner.live_scopes() {
            scope.close();
        }
    }
}

struct ScopeGuard {
    host: Arc<Host>,
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        self.host.close();
    }
}

/// The guests an embedder starts and stops as one: a command and what it
/// spawns. Cloning shares the scope; it closes when [`GuiScope::close`] is
/// called or the last clone is dropped.
#[derive(Clone)]
pub struct GuiScope {
    inner: Arc<ScopeGuard>,
}

impl std::fmt::Debug for GuiScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuiScope")
            .field("closed", &self.inner.host.is_closed())
            .finish_non_exhaustive()
    }
}

impl GuiScope {
    /// Hooks that give this scope's guests their GUI imports.
    pub fn hooks(&self) -> GuiRuntimeHooks {
        GuiRuntimeHooks {
            host: Arc::clone(&self.inner.host),
        }
    }

    /// Ends the scope: its windows are destroyed, and a guest of it that
    /// calls into the GUI again, or sits waiting for input, is stopped.
    pub fn close(&self) {
        self.inner.host.close();
    }

    pub fn is_closed(&self) -> bool {
        self.inner.host.is_closed()
    }

    #[cfg(test)]
    pub(crate) fn host(&self) -> &Arc<Host> {
        &self.inner.host
    }
}

/// Opaque per-instantiation state: returned by
/// [`GuiRuntimeHooks::add_imports`], handed back unmodified to
/// [`GuiRuntimeHooks::configure_instance`] for the instance created with
/// those imports.
pub struct GuiInstantiationState {
    env: Option<FunctionEnv<GuiEnv>>,
}

/// Runtime hooks that provide the GUI imports to WASIX guests.
///
/// Everything an instantiation needs travels in its
/// [`GuiInstantiationState`], so concurrent instantiations cannot receive each
/// other's state.
#[derive(Clone)]
pub struct GuiRuntimeHooks {
    host: Arc<Host>,
}

impl std::fmt::Debug for GuiRuntimeHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuiRuntimeHooks").finish_non_exhaustive()
    }
}

/// A zero of each numeric type, for the imports this build does not know.
fn zero(ty: &Type) -> Option<Value> {
    Some(match ty {
        Type::I32 => Value::I32(0),
        Type::I64 => Value::I64(0),
        Type::F32 => Value::F32(0.0),
        Type::F64 => Value::F64(0.0),
        _ => return None,
    })
}

/// The import a newer guest finds on this host for a function this build does
/// not have: it does nothing and answers 0, `WGUIStatus_Unimplemented`.
fn stub(store: &mut StoreMut<'_>, name: &str, ty: &FunctionType) -> Result<Function> {
    let results: Option<Vec<Value>> = ty.results().iter().map(zero).collect();
    let results = results.with_context(|| {
        format!("`{name}` is not a function this build of wasmer-gui could stand in for")
    })?;
    let name = name.to_owned();
    let reported = AtomicBool::new(false);
    Ok(Function::new(store, ty.clone(), move |_arguments| {
        if !reported.swap(true, Ordering::Relaxed) {
            log(|| format!("{name} is not implemented by this host; answering 0"));
        }
        Ok(results.clone())
    }))
}

/// The runtime's own `sched_yield` among `imports`, when the program is given
/// the system's imports beside these. Each system interface has one, made for
/// the kind of memory its programs have: the program's own interface is asked
/// first, and any of them serves a program that imports nothing of the system.
fn system_yield(module: &Module, store: &StoreMut<'_>, imports: &Imports) -> Option<Function> {
    const INTERFACES: [&str; 4] = [
        "wasix_32v1",
        "wasi_snapshot_preview1",
        "wasi_unstable",
        "wasix_64v1",
    ];
    let expected = FunctionType::new([], [Type::I32]);
    let find = |interface: &str| match imports.get_export(interface, "sched_yield") {
        Some(Extern::Function(function)) if function.ty(store) == expected => Some(function),
        _ => None,
    };
    let used = |interface: &str| module.imports().any(|import| import.module() == interface);
    INTERFACES
        .iter()
        .copied()
        .filter(|interface| used(interface))
        .find_map(find)
        .or_else(|| INTERFACES.iter().copied().find_map(find))
}

impl GuiRuntimeHooks {
    /// Creates the GUI imports when `module` asks for them.
    pub fn additional_imports(
        &self,
        module: &Module,
        store: &mut StoreMut<'_>,
    ) -> Result<(Imports, GuiInstantiationState)> {
        let mut imports = Imports::new();
        let state = self.add_imports(module, store, &mut imports)?;
        Ok((imports, state))
    }

    /// Adds the GUI imports to an existing import object when `module` asks
    /// for them. Does nothing for every other module.
    pub fn add_imports(
        &self,
        module: &Module,
        store: &mut StoreMut<'_>,
        imports: &mut Imports,
    ) -> Result<GuiInstantiationState> {
        match module_needs_gui(module) {
            None => return Ok(GuiInstantiationState { env: None }),
            Some(GuiAbi::V0) => {}
            Some(GuiAbi::Unknown) => bail!(
                "this module imports a GUI ABI this build of wasmer-gui does not provide (it \
                 supports `{}`); rebuild the guest against the matching gui.h, or update the \
                 runtime",
                crate::GUI_MODULE_NAME
            ),
        }

        let mut data = GuiEnv::new(Arc::clone(&self.host));
        data.system_yield = system_yield(module, store, imports);
        let env = FunctionEnv::new(store, data);
        let mut exports = Exports::new();
        crate::abi::api::register(store, &env, &mut exports);
        // A guest built against a newer header may import functions this
        // build does not have. It still starts: each gets a stub.
        for import in module.imports() {
            if import.module() != crate::GUI_MODULE_NAME
                || crate::abi::import_sig(import.name()).is_some()
            {
                continue;
            }
            if let ExternType::Function(ty) = import.ty() {
                exports.insert(import.name(), stub(store, import.name(), ty)?);
            }
        }
        imports.register_namespace(crate::GUI_MODULE_NAME, exports);
        Ok(GuiInstantiationState { env: Some(env) })
    }

    /// Binds the new instance's memory to the imports created for it.
    pub fn configure_instance(
        &self,
        module: &Module,
        store: &mut StoreMut<'_>,
        instance: &Instance,
        imported_memory: Option<&wasmer::Memory>,
        state: GuiInstantiationState,
    ) -> Result<()> {
        if module_needs_gui(module).is_none() {
            return Ok(());
        }
        let env = state.env.context(
            "missing GUI state for module instance setup \
             (the state was not created for this module's imports)",
        )?;
        let memory = match imported_memory {
            Some(memory) => memory.clone(),
            None => instance
                .exports
                .get_memory("memory")
                .context("GUI imports require the guest to have linear memory")?
                .clone(),
        };
        // Where the runtime lets a kill be seen on the memory, remember how.
        let kill_probe = memory.as_shared(store).map(|shared| shared.ops());
        let data = env.as_mut(store);
        data.memory = Some(memory);
        data.kill_probe = kill_probe;
        Ok(())
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use wasmer::{AsStoreMut, Store};

    fn compile(store: &Store, wat: &str) -> Module {
        Module::new(store, wat::parse_str(wat).expect("wat parses")).expect("module compiles")
    }

    fn instantiate(store: &mut Store, hooks: &GuiRuntimeHooks, module: &Module) -> Instance {
        let (imports, state) = hooks
            .additional_imports(module, &mut store.as_store_mut())
            .expect("imports register");
        let instance = Instance::new(store, module, &imports).expect("instantiates");
        hooks
            .configure_instance(module, &mut store.as_store_mut(), &instance, None, state)
            .expect("configures");
        instance
    }

    #[test]
    fn modules_without_gui_get_no_imports() {
        let mut store = Store::default();
        let module = compile(&store, r#"(module (memory (export "memory") 1))"#);
        let hooks = GuiCtx::default().runtime_hooks();
        let (imports, state) = hooks
            .additional_imports(&module, &mut store.as_store_mut())
            .expect("hook runs");
        assert_eq!(imports.iter().count(), 0);
        assert!(state.env.is_none());
    }

    #[test]
    fn every_declared_import_is_registered_with_its_signature() {
        // A module importing the entire ABI must instantiate: this is what
        // catches a signature drifting between the import table and the
        // generated glue.
        let mut wat = String::from("(module\n");
        let ty = |t: &crate::abi::ValType| match t {
            crate::abi::ValType::I32 => "i32",
            crate::abi::ValType::I64 => "i64",
            crate::abi::ValType::F32 => "f32",
            crate::abi::ValType::F64 => "f64",
        };
        for sig in crate::abi::IMPORTS {
            let params: Vec<_> = sig.params.iter().map(ty).collect();
            let results: Vec<_> = sig.results.iter().map(ty).collect();
            wat.push_str(&format!(
                "(import \"{}\" \"{}\" (func (param {}) (result {})))\n",
                crate::GUI_MODULE_NAME,
                sig.name,
                params.join(" "),
                results.join(" ")
            ));
        }
        wat.push_str("(memory (export \"memory\") 1))");

        let mut store = Store::default();
        let module = compile(&store, &wat);
        instantiate(&mut store, &GuiCtx::default().runtime_hooks(), &module);
    }

    #[test]
    fn imports_this_build_does_not_know_answer_zero() {
        let mut store = Store::default();
        let module = compile(
            &store,
            r#"(module
                (import "wasmer_gui_v0" "wguiGetVersion" (func $version (result i32)))
                (import "wasmer_gui_v0" "wguiWindowDoSomethingNew"
                    (func $new (param i32 i64 f64 i32) (result i32)))
                (import "wasmer_gui_v0" "wguiCursorForget" (func $forget (param i32)))
                (memory (export "memory") 1)
                (func (export "version") (result i32) (call $version))
                (func (export "new") (result i32)
                    (call $new (i32.const 7) (i64.const 1) (f64.const 2) (i32.const 64)))
                (func (export "forget") (call $forget (i32.const 7))))"#,
        );
        let instance = instantiate(&mut store, &GuiCtx::default().runtime_hooks(), &module);
        let call = |store: &mut Store, name: &str| {
            instance
                .exports
                .get_function(name)
                .expect("exported")
                .call(store, &[])
                .expect("does not trap")
        };
        assert_eq!(
            call(&mut store, "version")[..],
            [Value::I32(crate::GUI_ABI_VERSION as i32)]
        );
        // 0 is WGUIStatus_Unimplemented.
        assert_eq!(call(&mut store, "new")[..], [Value::I32(0)]);
        assert!(call(&mut store, "forget").is_empty());
    }

    #[test]
    fn a_known_import_with_another_signature_does_not_link() {
        let mut store = Store::default();
        let module = compile(
            &store,
            r#"(module
                (import "wasmer_gui_v0" "wguiGetVersion" (func (param i32) (result i64)))
                (memory (export "memory") 1))"#,
        );
        let hooks = GuiCtx::default().runtime_hooks();
        let (imports, _state) = hooks
            .additional_imports(&module, &mut store.as_store_mut())
            .expect("imports register");
        assert!(Instance::new(&mut store, &module, &imports).is_err());
    }

    #[test]
    fn unknown_abi_revisions_are_refused() {
        let mut store = Store::default();
        let module = compile(
            &store,
            r#"(module (import "wasmer_gui_v9" "wguiGetVersion" (func (result i32))))"#,
        );
        let hooks = GuiCtx::default().runtime_hooks();
        let error = hooks
            .additional_imports(&module, &mut store.as_store_mut())
            .err()
            .expect("unknown revision is refused");
        assert!(error.to_string().contains("wasmer_gui_v0"));
    }

    #[test]
    fn configure_instance_rejects_missing_state() {
        let mut store = Store::default();
        let module = compile(
            &store,
            r#"(module
                (import "wasmer_gui_v0" "wguiGetVersion" (func (result i32)))
                (memory (export "memory") 1))"#,
        );
        let hooks = GuiCtx::default().runtime_hooks();
        let (imports, _state) = hooks
            .additional_imports(&module, &mut store.as_store_mut())
            .expect("imports register");
        let instance = Instance::new(&mut store, &module, &imports).expect("instantiates");
        let error = hooks
            .configure_instance(
                &module,
                &mut store.as_store_mut(),
                &instance,
                None,
                GuiInstantiationState { env: None },
            )
            .expect_err("missing state is refused");
        assert!(error.to_string().contains("missing GUI state"));
    }

    /// A program that makes an instance, asks it for events and waits for
    /// them without sleeping. Its handle is kept at 8, what the calls write
    /// goes to 16, and records to 1024 onwards.
    const EVENT_LOOP: &str = r#"(module
        (import "wasmer_gui_v0" "wguiCreateInstance"
            (func $create (param i32 i32) (result i32)))
        (import "wasmer_gui_v0" "wguiInstancePollEvents"
            (func $poll (param i32 i32 i32 i32) (result i32)))
        (import "wasmer_gui_v0" "wguiInstanceWaitEvents"
            (func $wait (param i32 i64 i32) (result i32)))
        (memory (export "memory") 1)
        (func (export "create") (result i32) (call $create (i32.const 0) (i32.const 8)))
        (func (export "poll") (result i32)
            (call $poll (i32.load (i32.const 8)) (i32.const 1024) (i32.const 8192) (i32.const 16)))
        (func (export "wait") (result i32)
            (call $wait (i32.load (i32.const 8)) (i64.const 0) (i32.const 16))))"#;

    /// Instantiates [`EVENT_LOOP`] the way a WASIX runtime does: with the
    /// system's imports, here only `yields` for its `sched_yield`, in the
    /// object the hooks add theirs to.
    fn event_loop(store: &mut Store, yields: Function) -> Instance {
        let module = compile(store, EVENT_LOOP);
        let hooks = GuiCtx::default().runtime_hooks();
        let mut imports = Imports::new();
        imports.define("wasi_snapshot_preview1", "sched_yield", yields);
        let state = hooks
            .add_imports(&module, &mut store.as_store_mut(), &mut imports)
            .expect("imports register");
        let instance = Instance::new(store, &module, &imports).expect("instantiates");
        hooks
            .configure_instance(&module, &mut store.as_store_mut(), &instance, None, state)
            .expect("configures");
        instance
    }

    #[test]
    fn asking_and_waiting_for_events_is_where_the_system_reaches_a_program() {
        let mut store = Store::default();
        let yielded = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&yielded);
        let yields = Function::new_typed(&mut store, move || -> i32 {
            counted.fetch_add(1, Ordering::Relaxed);
            0
        });
        let instance = event_loop(&mut store, yields);
        let call = |store: &mut Store, name: &str| {
            let function = instance.exports.get_function(name).expect("exported");
            function.call(store, &[]).expect("does not trap")
        };
        // 1 is WGUIStatus_Success.
        assert_eq!(call(&mut store, "create")[..], [Value::I32(1)]);
        assert_eq!(yielded.load(Ordering::Relaxed), 0);

        // A program that asks again and again yields now and then...
        for _ in 0..1000 {
            assert_eq!(call(&mut store, "poll")[..], [Value::I32(1)]);
        }
        let asking = yielded.load(Ordering::Relaxed);
        assert!((1..100).contains(&asking), "{asking} yields in 1000 calls");
        // ...and one that waits, each time it has.
        for _ in 0..3 {
            assert_eq!(call(&mut store, "wait")[..], [Value::I32(1)]);
        }
        assert_eq!(yielded.load(Ordering::Relaxed), asking + 3);
    }

    #[test]
    fn what_ends_a_program_at_its_yield_ends_it_where_it_asks_for_events() {
        #[derive(Debug)]
        struct Ended;
        impl std::fmt::Display for Ended {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("ended by the system")
            }
        }
        impl std::error::Error for Ended {}

        let mut store = Store::default();
        let yields = Function::new_typed(&mut store, || -> Result<i32, wasmer::RuntimeError> {
            Err(wasmer::RuntimeError::user(Box::new(Ended)))
        });
        let instance = event_loop(&mut store, yields);
        let create = instance.exports.get_function("create").expect("exported");
        assert!(create.call(&mut store, &[]).is_ok());
        for name in ["poll", "wait"] {
            let function = instance.exports.get_function(name).expect("exported");
            let error = function
                .call(&mut store, &[])
                .expect_err("the program is ended");
            // As the runtime ended it: an embedder finds its own error.
            assert!(error.downcast::<Ended>().is_ok(), "{name}");
        }
    }

    #[test]
    fn a_program_without_the_system_beside_it_asks_for_events_all_the_same() {
        let mut store = Store::default();
        let module = compile(&store, EVENT_LOOP);
        let instance = instantiate(&mut store, &GuiCtx::default().runtime_hooks(), &module);
        for name in ["create", "poll", "wait"] {
            let function = instance.exports.get_function(name).expect("exported");
            let status = function.call(&mut store, &[]).expect("does not trap");
            assert_eq!(status[..], [Value::I32(1)], "{name}");
        }
    }

    #[test]
    fn a_closed_scope_stops_its_guests() {
        let mut store = Store::default();
        let module = compile(
            &store,
            r#"(module
                (import "wasmer_gui_v0" "wguiGetVersion" (func $version (result i32)))
                (memory (export "memory") 1)
                (func (export "version") (result i32) (call $version)))"#,
        );
        let ctx = GuiCtx::default();
        let scope = ctx.scope();
        let instance = instantiate(&mut store, &scope.hooks(), &module);
        let version = instance.exports.get_function("version").expect("exported");
        assert!(version.call(&mut store, &[]).is_ok());
        scope.close();
        assert!(scope.is_closed());
        let error = version
            .call(&mut store, &[])
            .expect_err("a stopped guest traps");
        assert!(error.to_string().contains("stopped"), "{error}");
    }

    #[test]
    fn terminate_all_closes_scopes_made_before_and_after() {
        let ctx = GuiCtx::default();
        let before = ctx.scope();
        ctx.runtime_control().terminate_all();
        assert!(before.is_closed());
        assert!(ctx.scope().is_closed());
    }
}
