//! The wait that suspends (`GuiCtxBuilder::suspending_waits`), across
//! Wasmer's own import boundary: a guest that imports the GUI functions is
//! entered with `call_async`, and waits by suspending through JSPI while this
//! thread's event loop goes on.
//!
//! It needs a build whose own memory is shared, and a JavaScript engine with
//! JSPI and `Atomics.waitAsync`: `make test-web-wait` (see the README).

#![cfg(all(target_arch = "wasm32", feature = "js"))]

use std::cell::Cell;
use std::rc::Rc;

use js_sys::{SharedArrayBuffer, WebAssembly};
use wasm_bindgen::prelude::*;
use wasm_bindgen_test::wasm_bindgen_test;
use wasmer::{
    AsStoreMut, Function, HostFunction, Imports, Instance, Module, RuntimeError, Store, StoreAsync,
    TypedFunction, WasmTypeList, WithoutEnv,
};
use wasmer_gui::abi::enums::{Status, WAIT_FOREVER, wait_reasons};
use wasmer_gui::{GuiCtx, GuiScope};

/// Opens an instance and waits on it for `timeout` nanoseconds. `run`
/// answers the wait's status, with its reasons above the low byte, or -1 if
/// the instance could not be opened. `sched_yield` is the runtime's: the wait
/// calls it as it returns.
const GUEST: &str = r#"
(module
  (import "wasix_32v1" "sched_yield" (func (result i32)))
  (import "wasmer_gui_v0" "wguiCreateInstance" (func $create (param i32 i32) (result i32)))
  (import "wasmer_gui_v0" "wguiInstanceWaitEvents"
    (func $wait (param i32 i64 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "run") (param $timeout i64) (result i32)
    (local $status i32)
    (if (i32.ne (call $create (i32.const 0) (i32.const 16)) (i32.const 1))
      (then (return (i32.const -1))))
    (local.set $status (call $wait (i32.load (i32.const 16)) (local.get $timeout) (i32.const 32)))
    (i32.or (local.get $status) (i32.shl (i32.load (i32.const 32)) (i32.const 8)))))
"#;

const SUCCESS: i32 = Status::Success as i32;
const MS: u64 = 1_000_000;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = setTimeout)]
    fn set_timeout(handler: &JsValue, ms: i32) -> JsValue;
    #[wasm_bindgen(js_namespace = performance, js_name = now)]
    fn now() -> f64;
    #[wasm_bindgen(thread_local_v2, js_namespace = WebAssembly, js_name = Suspending)]
    static SUSPENDING: JsValue;
    #[wasm_bindgen(thread_local_v2, js_namespace = Atomics, js_name = waitAsync)]
    static WAIT_ASYNC: JsValue;
}

/// What the suspending wait needs of this build and engine. Without it the
/// wait would park instead, and the tests would fail less clearly.
fn assert_can_suspend() {
    let memory: WebAssembly::Memory = wasm_bindgen::memory().unchecked_into();
    assert!(
        memory.buffer().is_instance_of::<SharedArrayBuffer>(),
        "this build's memory is not shared: see `make test-web-wait`"
    );
    assert!(
        SUSPENDING.with(JsValue::is_function),
        "this JavaScript engine has no JSPI"
    );
    assert!(
        WAIT_ASYNC.with(JsValue::is_function),
        "this JavaScript engine has no Atomics.waitAsync"
    );
}

/// `sched_yield` with nothing to report.
fn yield_quietly() -> i32 {
    0
}

/// An exit, as WASIX reports one: an error of the runtime's own type.
#[derive(Debug, PartialEq)]
struct ProbeExit(i32);

impl std::fmt::Display for ProbeExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the program exited with {}", self.0)
    }
}

impl std::error::Error for ProbeExit {}

/// `sched_yield` for a program told to exit with `CODE`. (A host function in
/// JavaScript may not capture anything, hence the constant.)
fn yield_into_exit<const CODE: i32>() -> Result<i32, RuntimeError> {
    Err(RuntimeError::user(Box::new(ProbeExit(CODE))))
}

/// The guest, given `scope`'s imports and `sched_yield`, in a store ready for
/// `call_async`.
fn guest<Rets: WasmTypeList>(
    scope: &GuiScope,
    sched_yield: impl HostFunction<(), (), Rets, WithoutEnv> + Send + Sync + 'static,
) -> (StoreAsync, TypedFunction<i64, i32>) {
    let mut store = Store::default();
    let module = Module::new(&store, wat::parse_str(GUEST).expect("wat parses")).expect("compiles");
    let mut imports = Imports::new();
    let sched_yield = Function::new_typed(&mut store, sched_yield);
    imports.define("wasix_32v1", "sched_yield", sched_yield);
    let hooks = scope.hooks();
    let state = hooks
        .add_imports(&module, &mut store.as_store_mut(), &mut imports)
        .expect("GUI imports");
    let instance = Instance::new(&mut store, &module, &imports).expect("instantiates");
    hooks
        .configure_instance(&module, &mut store.as_store_mut(), &instance, None, state)
        .expect("configured");
    let run = instance
        .exports
        .get_typed_function(&store, "run")
        .expect("exports run");
    (store.into_async(), run)
}

/// Runs `f` from this thread's event loop after `ms`. What it returns tells
/// whether that happened, and whether the guest's call was pending then.
fn after(ms: i32, in_call: &Rc<Cell<bool>>, f: impl FnOnce() + 'static) -> Rc<Cell<Option<bool>>> {
    let fired = Rc::new(Cell::new(None));
    let (in_call, fired_too) = (Rc::clone(in_call), Rc::clone(&fired));
    let handler = Closure::once_into_js(move || {
        fired_too.set(Some(in_call.get()));
        f();
    });
    set_timeout(&handler, ms);
    fired
}

/// Calls the guest's `run` through `call_async`: what it returned, and how
/// long that took in milliseconds. `in_call` holds while the call is pending.
async fn call(
    store: &StoreAsync,
    run: &TypedFunction<i64, i32>,
    timeout: u64,
    in_call: &Rc<Cell<bool>>,
) -> (Result<i32, RuntimeError>, f64) {
    let started = now();
    in_call.set(true);
    let returned = run.call_async(store, timeout as i64).await;
    in_call.set(false);
    (returned, now() - started)
}

#[wasm_bindgen_test]
async fn a_wait_of_no_time_returns_at_once() {
    assert_can_suspend();
    let ctx = GuiCtx::builder().suspending_waits(true).build();
    let scope = ctx.scope();
    let (store, run) = guest(&scope, yield_quietly);
    let in_call = Rc::new(Cell::new(false));
    let (returned, took) = call(&store, &run, 0, &in_call).await;
    assert_eq!(returned.expect("the wait returns"), SUCCESS, "no reasons");
    assert!(took < 1000.0, "took {took} ms");
}

#[wasm_bindgen_test]
async fn a_wait_runs_out_while_the_event_loop_goes_on() {
    assert_can_suspend();
    let ctx = GuiCtx::builder().suspending_waits(true).build();
    let scope = ctx.scope();
    let (store, run) = guest(&scope, yield_quietly);
    let in_call = Rc::new(Cell::new(false));
    let fired = after(20, &in_call, || {});
    // Shorter than the 250 ms after which a wait is handed back as
    // interrupted.
    let (returned, took) = call(&store, &run, 150 * MS, &in_call).await;
    assert_eq!(returned.expect("the wait returns"), SUCCESS, "no reasons");
    assert!((140.0..5000.0).contains(&took), "took {took} ms");
    assert_eq!(
        fired.get(),
        Some(true),
        "the timer did not run while the guest waited"
    );
}

#[wasm_bindgen_test]
async fn the_event_waker_ends_a_wait_from_the_same_thread() {
    assert_can_suspend();
    let ctx = GuiCtx::builder().suspending_waits(true).build();
    let scope = ctx.scope();
    let (store, run) = guest(&scope, yield_quietly);
    let in_call = Rc::new(Cell::new(false));
    let wake = ctx.event_waker();
    let fired = after(30, &in_call, move || wake());
    let (returned, _) = call(&store, &run, WAIT_FOREVER, &in_call).await;
    // Not woken, it would have been handed back as interrupted.
    assert_eq!(
        returned.expect("the wait returns"),
        SUCCESS | (wait_reasons::WOKEN as i32) << 8
    );
    assert_eq!(fired.get(), Some(true), "woken before the guest waited");
}

#[wasm_bindgen_test]
async fn closing_the_scope_stops_a_waiting_guest() {
    assert_can_suspend();
    let ctx = GuiCtx::builder().suspending_waits(true).build();
    let scope = ctx.scope();
    let (store, run) = guest(&scope, yield_quietly);
    let in_call = Rc::new(Cell::new(false));
    let closing = scope.clone();
    let fired = after(30, &in_call, move || closing.close());
    let (returned, _) = call(&store, &run, WAIT_FOREVER, &in_call).await;
    // Not stopped, the wait would have been handed back as interrupted.
    let error = returned.expect_err("the guest is stopped");
    assert!(
        error.message().contains("the program was stopped"),
        "{error}"
    );
    assert_eq!(fired.get(), Some(true), "closed before the guest waited");
}

/// Waits for `timeout` where `sched_yield` exits with `CODE`: the error the
/// call ends with, and whether the guest was suspended on the way.
async fn exit_from_a_wait<const CODE: i32>(suspending: bool, timeout: u64) -> (RuntimeError, bool) {
    let ctx = GuiCtx::builder().suspending_waits(suspending).build();
    let scope = ctx.scope();
    let (store, run) = guest(&scope, yield_into_exit::<CODE>);
    let in_call = Rc::new(Cell::new(false));
    let fired = after(10, &in_call, || {});
    let (returned, _) = call(&store, &run, timeout, &in_call).await;
    let error = returned.expect_err("the program exits");
    (error, fired.get() == Some(true))
}

#[wasm_bindgen_test]
async fn an_exit_as_a_wait_of_no_time_returns_keeps_its_type() {
    assert_can_suspend();
    let (error, _) = exit_from_a_wait::<7>(true, 0).await;
    assert_eq!(
        error.downcast_ref::<ProbeExit>(),
        Some(&ProbeExit(7)),
        "{error}"
    );
}

#[wasm_bindgen_test]
async fn an_exit_as_a_suspended_wait_returns_keeps_its_type() {
    assert_can_suspend();
    let (error, suspended) = exit_from_a_wait::<8>(true, 60 * MS).await;
    assert!(suspended, "the guest did not suspend");
    assert_eq!(
        error.downcast_ref::<ProbeExit>(),
        Some(&ProbeExit(8)),
        "{error}"
    );
}

/// What the two above hold the suspending wait to: the parking one, for the
/// same guest.
#[wasm_bindgen_test]
async fn an_exit_as_a_parking_wait_returns_keeps_its_type() {
    let (error, _) = exit_from_a_wait::<9>(false, 0).await;
    assert_eq!(
        error.downcast_ref::<ProbeExit>(),
        Some(&ProbeExit(9)),
        "{error}"
    );
}
