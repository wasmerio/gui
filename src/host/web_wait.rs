//! `wguiInstanceWaitEvents` that suspends instead of parking, for a guest
//! running in a browser Worker ([`GuiCtxBuilder::suspending_waits`]).
//!
//! Parking blocks the Worker in `memory.atomic.wait`, so nothing else that
//! lives in it runs until the wait ends: WebGPU completions (pipelines,
//! `mapAsync`, submitted work), message ports, timers. A program that idles
//! in the wait then sees each of them only when its timeout runs out. Here
//! the guest suspends through JSPI on `Atomics.waitAsync` over its instance's
//! notification counter (`WaitSet::notify` bumps it), so the Worker's own
//! event loop keeps running. Each look decides as the parking wait does
//! (`wait_step`). Work that completes in the Worker ends the wait through
//! [`GuiCtx::event_waker`]: that wakes the instance as `wguiInstanceWake`
//! does, which notifies the counter.
//!
//! It is installed when the embedder asks for it and the JavaScript engine
//! has JSPI and `Atomics.waitAsync`, with the module's memory shared;
//! otherwise the parking wait stays. A guest that waits here must have been
//! entered through `Function::call_async`: one entered synchronously cannot
//! suspend, and the wait traps.
//!
//! While the guest is suspended here, its Worker runs other tasks, as it
//! already does when the guest's WebGPU imports suspend. No lock is held
//! meanwhile, only the instance's shared state, where the counter is.
//!
//! One thing comes out differently from the parking wait: a scope closed
//! while the guest sleeps stops it at the next look even if input or a wake
//! came at the same moment, where the parking wait returns those first and
//! stops the guest at its next call. What ends the program as the wait
//! returns (a WASIX exit, a signal without a handler, from `let_signals_in`)
//! reaches the runtime with its type, as from the parking wait, given a
//! Wasmer whose asynchronous host functions in JavaScript keep it
//! (wasmerio/wasmer#7122; the revision this repository pins has it).
//!
//! Notifications reach the counter through JavaScript's `Atomics.notify`,
//! which the specification guarantees wakes `Atomics.waitAsync` on the same
//! word, in this Worker as in any other (the wasm instruction for it is not
//! available on stable Rust).
//!
//! [`GuiCtxBuilder::suspending_waits`]: crate::GuiCtxBuilder::suspending_waits
//! [`GuiCtx::event_waker`]: crate::GuiCtx::event_waker

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use js_sys::Promise;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use wasmer::{AsStoreMut, AsyncFunctionEnvMut, Exports, Function, FunctionEnv, RuntimeError};

use super::instance::{WaitStep, wait_step};
use super::shared::InstanceShared;
use super::{enter, let_signals_in};
use crate::abi::enums::*;
use crate::abi::{AbiError, OutPtr};
use crate::env::GuiEnv;

/// Shorter sleeps still park: a suspension costs a turn of the event loop,
/// and timers are not finer than a millisecond.
const MIN_SUSPEND: Duration = Duration::from_millis(1);

#[wasm_bindgen(inline_js = "
// Captured once: page code shares this Worker's global and may replace any of it.
const waitAsync = typeof Atomics.waitAsync === 'function' ? Atomics.waitAsync.bind(Atomics) : undefined;
const notifyAll = Atomics.notify.bind(Atomics);
const jspi = typeof WebAssembly.Suspending === 'function' && typeof WebAssembly.promising === 'function';
const SharedBuffer = typeof SharedArrayBuffer === 'function' ? SharedArrayBuffer : undefined;
const PromiseClass = Promise;
const resolved = Promise.resolve.bind(Promise);
const then = Function.prototype.call.bind(Promise.prototype.then);
const Int32ArrayClass = Int32Array;
const setTimer = setTimeout;
const clearTimer = clearTimeout;
let view = null;
const words = memory => {
  const buffer = memory.buffer;
  if (view === null || view.buffer !== buffer) view = new Int32ArrayClass(buffer);
  return view;
};
export function wasmer_gui_wait_can_suspend(memory) {
  return jspi && waitAsync !== undefined && SharedBuffer !== undefined && memory.buffer instanceof SharedBuffer;
}
export function wasmer_gui_wait_notify(memory, address) {
  notifyAll(words(memory), address >>> 2);
}
// Settles once the word at `address` is notified or no longer holds `seen`,
// or `ms` have passed. Never rejects.
export function wasmer_gui_wait(memory, address, seen, ms) {
  const result = waitAsync(words(memory), address >>> 2, seen | 0, ms);
  if (!result.async) return resolved();
  return new PromiseClass(resolve => {
    let guard;
    const finish = () => {
      clearTimer(guard);
      resolve();
    };
    then(result.value, finish, finish);
    // A bound even if the waitAsync is never settled, which has been seen
    // to happen: the guest looks again and waits anew.
    guard = setTimer(finish, ms + 4);
  });
}
")]
extern "C" {
    #[wasm_bindgen(js_name = wasmer_gui_wait_can_suspend)]
    fn can_suspend(memory: &JsValue) -> bool;
    #[wasm_bindgen(js_name = wasmer_gui_wait_notify)]
    fn notify_word(memory: &JsValue, address: u32);
    #[wasm_bindgen(js_name = wasmer_gui_wait)]
    fn wait_word(memory: &JsValue, address: u32, seen: u32, ms: f64) -> Promise;
}

/// Whether any guest of this module waits by suspending. Until one does,
/// notifications leave JavaScript alone: parking waits are woken by
/// unparking. Set while a guest is instantiated, before it can make an
/// instance to wait on; `SeqCst` like the counter, so that a notification
/// that finds it unset comes before any wait could read the counter.
static INSTALLED: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// The module's memory as this thread's JavaScript sees it.
    static MEMORY: JsValue = wasm_bindgen::memory();
}

/// Wakes the waits suspended on a counter (`WaitSet::notify`), from any
/// thread: the page's own, which produces input, or a guest's.
pub(crate) fn notify(address: u32) {
    if !INSTALLED.load(Ordering::SeqCst) {
        return;
    }
    // A thread that is going away may have lost its copy already.
    if MEMORY
        .try_with(|memory| notify_word(memory, address))
        .is_err()
    {
        notify_word(&wasm_bindgen::memory(), address);
    }
}

/// Replaces the parking `wguiInstanceWaitEvents` with the suspending one,
/// where that can work; leaves `exports` as they are otherwise.
pub(crate) fn install(
    store: &mut impl AsStoreMut,
    env: &FunctionEnv<GuiEnv>,
    exports: &mut Exports,
) {
    if !MEMORY.with(can_suspend) {
        return;
    }
    INSTALLED.store(true, Ordering::SeqCst);
    exports.insert(
        "wguiInstanceWaitEvents",
        Function::new_typed_with_env_async(store, env, wait_events),
    );
}

/// Keeps `InstanceShared::waiting` counted for the whole call, as the parking
/// wait does, however the call ends: also when a guest abandoned while
/// suspended takes this future with it.
struct Waiting(Arc<InstanceShared>);

impl Drop for Waiting {
    fn drop(&mut self) {
        self.0.waiting.fetch_sub(1, Ordering::AcqRel);
    }
}

async fn wait_events(
    env: AsyncFunctionEnvMut<GuiEnv>,
    instance: u32,
    timeout: u64,
    reasons: u32,
) -> Result<u32, RuntimeError> {
    let limit = (timeout != WAIT_FOREVER).then(|| Duration::from_nanos(timeout));
    let started = crate::clock::now();
    let mut waiting: Option<Waiting> = None;
    loop {
        // One look, with the store held; nothing is held while suspended.
        // The counter is read before anything else: whatever would end the
        // wait after that read notifies it, and the wait below then ends at
        // once instead of sleeping through it.
        let mut sleep = None;
        let returned = {
            let mut store = env.write().await;
            enter(
                &mut store.as_function_env_mut(),
                "wguiInstanceWaitEvents",
                |host, cx| {
                    if reasons == 0 {
                        return Err(AbiError::NullPointer("reasons"));
                    }
                    // Resolved once, like the parking wait: an instance
                    // destroyed under this one ends the wait as interrupted
                    // (`is_closed`), and a handle reused meanwhile never
                    // redirects it.
                    if waiting.is_none() {
                        let Some(live) = host.instance(instance) else {
                            return Ok(Status::InvalidHandle);
                        };
                        // A wait for events, as far as `while_drawing` goes
                        // (`InstanceShared::drawing`), as in the parking wait.
                        live.shared.waits.fetch_add(1, Ordering::AcqRel);
                        live.shared.waiting.fetch_add(1, Ordering::AcqRel);
                        waiting = Some(Waiting(Arc::clone(&live.shared)));
                    }
                    let shared = &waiting.as_ref().expect("resolved above").0;
                    let seen = shared.wait.epoch();
                    match wait_step(shared, cx.stopped, started, limit) {
                        WaitStep::Done(outcome) => {
                            OutPtr::<u32>::new(reasons).write(&cx.mem, &outcome?)?;
                        }
                        WaitStep::Sleep(slice) => sleep = Some((slice, seen)),
                    }
                    Ok(Status::Success)
                },
            )
        };
        let returned = returned?;
        let Some((slice, seen)) = sleep else {
            drop(waiting);
            let mut store = env.write().await;
            // A failure here (an exit, a signal) is the runtime's own, and
            // reaches it with its type (see above).
            let_signals_in(&mut store.as_function_env_mut(), true)?;
            return Ok(returned);
        };
        let shared = &waiting.as_ref().expect("resolved by the look").0;
        if slice < MIN_SUSPEND {
            shared.wait.park(slice, || shared.has_events());
            continue;
        }
        let ms = slice.as_secs_f64() * 1000.0;
        let address = shared.wait.epoch_address();
        let settled = MEMORY.with(|memory| wait_word(memory, address, seen, ms));
        // It never rejects, and the next look tells what it settled for.
        let _ = JsFuture::from(settled).await;
    }
}
