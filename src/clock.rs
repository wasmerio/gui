//! A clock that also runs where the standard library has none.
//!
//! `std::time::Instant` panics on `wasm32-unknown-unknown`, which is where
//! this crate runs in a browser. There the time is the JavaScript realm's:
//! its time origin plus `performance.now()`, which is the same moment on
//! every thread of a page, as far as their clocks agree.

use std::time::Duration;

/// The time since some moment in the past that is the same for every thread
/// of the process. It never goes back.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn now() -> Duration {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed()
}

#[cfg(all(target_arch = "wasm32", feature = "js"))]
pub(crate) fn now() -> Duration {
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = performance, js_name = now)]
        fn performance_now() -> f64;
        // A constant of the thread's realm: read once for each.
        #[wasm_bindgen(thread_local_v2, js_namespace = performance, js_name = timeOrigin)]
        static TIME_ORIGIN: f64;
    }
    let milliseconds = TIME_ORIGIN.with(|origin| *origin) + performance_now();
    Duration::from_secs_f64(milliseconds.max(0.0) / 1000.0)
}

/// Without JavaScript there is no clock to read on this target. Time stands
/// still: waits end when they are woken, and never by themselves.
#[cfg(all(target_arch = "wasm32", not(feature = "js")))]
pub(crate) fn now() -> Duration {
    Duration::ZERO
}
