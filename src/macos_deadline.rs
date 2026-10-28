//! A timer macOS keeps to, for waits that end at a frame's deadline.
//!
//! macOS coalesces timers: a thread that sleeps until the display's next
//! refresh, or a loop told to wait until then, can be woken milliseconds
//! late, up to half a frame at 60 Hz. A libdispatch timer made with
//! `DISPATCH_TIMER_STRICT` and no leeway fires within a fraction of a
//! millisecond of its time. It only wakes somebody: it is not a display link,
//! and it says nothing of when a frame reaches the screen. It fires once for
//! each time it is armed, and nothing is left armed while nobody waits.

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use dispatch2::{
    _dispatch_source_type_timer, DispatchObject, DispatchQueue, DispatchQueueAttr,
    DispatchRetained, DispatchSource, DispatchTime, dispatch_source_timer_flags_t,
    dispatch_source_type_t,
};

// As dispatch2 declares it. Its `DispatchSource::new` panics where
// libdispatch makes no source, which it does for arguments it rejects:
// `Deadline::new` answers `None` there, and its owners wait with a sleep.
unsafe extern "C" {
    fn dispatch_source_create(
        kind: dispatch_source_type_t,
        handle: usize,
        mask: usize,
        queue: Option<&DispatchQueue>,
    ) -> Option<NonNull<DispatchSource>>;
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What the timer's handlers share with its [`Deadline`]. The cancel handler
/// frees it, and libdispatch runs that once, after the last event handler
/// has returned.
struct Context {
    /// A reference of its own: the source is there while this is.
    source: DispatchRetained<DispatchSource>,
    /// The time the timer is armed for. Kept locked while the timer is set,
    /// so that what is set is always the time stored last.
    deadline: Mutex<Option<Instant>>,
    /// Runs on a libdispatch thread: it may wake somebody, and do no more.
    /// No guest code, and nothing of AppKit.
    notify: Box<dyn Fn() + Send + Sync>,
}

// Threads share the context through a pointer, which the compiler does not
// check: this does.
const _: () = {
    const fn shared_by_threads<T: Send + Sync>() {}
    shared_by_threads::<Context>();
};

impl Context {
    /// Sets the timer to fire once at `deadline`, or not at all. Called with
    /// `deadline` locked, or before the source is activated; libdispatch does
    /// not wait for a handler here.
    fn set_timer(&self, deadline: Option<Instant>) {
        let start = match deadline {
            None => DispatchTime::FOREVER,
            // Both count the same uptime: `Instant` is CLOCK_UPTIME_RAW, and
            // `dispatch_time` from now is mach_absolute_time(). Converting
            // nanoseconds to ticks rounds down, so the timer may fire a tick
            // before `deadline`: `fired` sets it again then.
            Some(due) => {
                let delta = due.saturating_duration_since(Instant::now()).as_nanos();
                DispatchTime::NOW.time(i64::try_from(delta).unwrap_or(i64::MAX))
            }
        };
        // An interval of DISPATCH_TIME_FOREVER makes the timer fire once.
        self.source.set_timer(start, DispatchTime::FOREVER.0, 0);
    }
}

/// The timer's event handler, on a libdispatch thread. Only libdispatch
/// calls it, with the context of a live timer (see `Context`): dispatch2
/// types handlers as safe functions.
extern "C" fn fired(context: *mut c_void) {
    // SAFETY: the context outlives every event handler (see `Context`).
    let context = unsafe { &*context.cast::<Context>() };
    // A panic must not unwind into libdispatch: it would abort the process.
    let _ = catch_unwind(AssertUnwindSafe(|| {
        {
            let mut deadline = lock(&context.deadline);
            match *deadline {
                // Disarmed since, or the handle is gone.
                None => return,
                // Early: a fire for a time replaced by a later one, or the
                // clocks' rounding. The timer is set for the time stored.
                Some(due) if due > Instant::now() => {
                    context.set_timer(Some(due));
                    return;
                }
                Some(_) => *deadline = None,
            }
        }
        (context.notify)();
    }));
}

/// The timer's cancel handler: libdispatch runs it once, after the last
/// event handler has returned, and none run after it. Only libdispatch calls
/// it, with a timer's context, which nothing uses any more.
extern "C" fn cancelled(context: *mut c_void) {
    // SAFETY: leaked from a box in `Deadline::new`, and freed only here.
    let context = unsafe { Box::from_raw(context.cast::<Context>()) };
    // What `notify` captured is dropped here: it must not unwind either.
    let _ = catch_unwind(AssertUnwindSafe(move || drop(context)));
}

/// A one-shot timer that macOS fires on time, and calls a function when it
/// does.
pub(crate) struct Deadline {
    /// The reference `dispatch_source_create` returned.
    source: DispatchRetained<DispatchSource>,
    context: NonNull<Context>,
}

impl Deadline {
    /// A timer that calls `notify` on a thread of libdispatch's when it
    /// fires. `notify` may wake a thread or a loop, and do no more; it may
    /// still be running when the timer is dropped, so it holds nothing it
    /// borrows (a `Weak`, a `Thread`). `None` if libdispatch makes no timer,
    /// which it does only for arguments it rejects.
    pub(crate) fn new(notify: impl Fn() + Send + Sync + 'static) -> Option<Self> {
        let queue = DispatchQueue::new("wasmer-gui-deadline", DispatchQueueAttr::SERIAL);
        let strict = dispatch_source_timer_flags_t::DISPATCH_TIMER_STRICT.0 as usize;
        // SAFETY: the timer type, no handle, a timer flag, a queue, which the
        // source keeps.
        let source = unsafe {
            dispatch_source_create(
                (&raw const _dispatch_source_type_timer).cast_mut(),
                0,
                strict,
                Some(&queue),
            )
        }?;
        // SAFETY: the reference `dispatch_source_create` returned.
        let source = unsafe { DispatchRetained::from_raw(source) };
        let context = Box::new(Context {
            source: source.retain(),
            deadline: Mutex::new(None),
            notify: Box::new(notify),
        });
        // Not before `arm` sets it.
        context.set_timer(None);
        let context = NonNull::from(Box::leak(context));
        // SAFETY: an inactive source of ours, and the context just made for
        // it, which its cancel handler frees. Nothing runs before the source
        // is activated.
        unsafe { source.set_context(context.as_ptr().cast()) };
        source.set_event_handler_f(fired);
        source.set_cancel_handler_f(cancelled);
        source.activate();
        Some(Self { source, context })
    }

    fn context(&self) -> &Context {
        // SAFETY: freed only once `drop` has cancelled the source.
        unsafe { self.context.as_ref() }
    }

    /// Arms the timer for `deadline`, in place of the time it was armed for;
    /// `None` disarms it. A notification already on its way still arrives.
    pub(crate) fn arm(&self, deadline: Option<Instant>) {
        let context = self.context();
        let mut armed = lock(&context.deadline);
        if *armed != deadline {
            *armed = deadline;
            context.set_timer(deadline);
        }
    }

    /// Waits until `deadline`, on a timer whose notification unparks the
    /// thread that calls this. The wait ends at `deadline` by itself too, as
    /// a thread's sleep does, and a thread woken for any other reason goes
    /// back to waiting.
    pub(crate) fn wait_until(&self, deadline: Instant) {
        if deadline <= Instant::now() {
            return;
        }
        self.arm(Some(deadline));
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            std::thread::park_timeout(left);
        }
        self.arm(None);
    }
}

impl Drop for Deadline {
    fn drop(&mut self) {
        // A handler that has not looked yet notifies nobody.
        *lock(&self.context().deadline) = None;
        // Not waited for: a notification on its way may be waiting for a lock
        // whoever drops this holds. The cancel handler frees the context once
        // the last event handler has returned; it may do so from here on.
        self.source.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    // Waits for what has to happen are long: they end when it does, and
    // CI's machines can be slow.
    const LONG: Duration = Duration::from_secs(10);

    /// Whether the test's thread was held up past `time`, which the lines
    /// before had to finish by. What follows then proves nothing, and is not
    /// checked: a slow machine is not a failure.
    fn held_up(time: Instant) -> bool {
        Instant::now() >= time
    }

    struct Probe(mpsc::Sender<()>);

    impl Drop for Probe {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }

    fn timer() -> (Deadline, mpsc::Receiver<Instant>) {
        let (send, receive) = mpsc::channel();
        let timer = Deadline::new(move || {
            let _ = send.send(Instant::now());
        })
        .expect("a strict timer");
        (timer, receive)
    }

    #[test]
    fn fires_once_for_the_time_armed_last() {
        let (timer, fired) = timer();
        let first = Instant::now() + Duration::from_secs(2);
        timer.arm(Some(first));
        let due = Instant::now() + Duration::from_millis(5);
        timer.arm(Some(due));
        if held_up(first) {
            return;
        }
        assert!(fired.recv_timeout(LONG).expect("the earlier time") >= due);
        assert!(
            fired.recv_timeout(Duration::from_millis(50)).is_err(),
            "fired twice"
        );
    }

    #[test]
    fn a_later_time_replaces_an_earlier_one() {
        let (timer, fired) = timer();
        let first = Instant::now() + Duration::from_millis(300);
        timer.arm(Some(first));
        let due = Instant::now() + Duration::from_millis(600);
        timer.arm(Some(due));
        if held_up(first) {
            return;
        }
        assert!(fired.recv_timeout(LONG).expect("the later time") >= due);
        assert!(fired.recv_timeout(Duration::from_millis(50)).is_err());
    }

    #[test]
    fn a_disarmed_timer_does_not_fire_and_can_be_armed_again() {
        let (timer, fired) = timer();
        let armed = Instant::now();
        let first = armed + Duration::from_millis(300);
        timer.arm(Some(first));
        timer.arm(None);
        if held_up(first) {
            return;
        }
        let past = (armed + Duration::from_millis(400)).saturating_duration_since(Instant::now());
        assert!(fired.recv_timeout(past).is_err(), "fired disarmed");
        timer.arm(Some(Instant::now() + Duration::from_millis(5)));
        fired.recv_timeout(LONG).expect("armed again");
    }

    #[test]
    fn the_same_time_twice_fires_once() {
        let (timer, fired) = timer();
        let due = Instant::now() + Duration::from_millis(300);
        timer.arm(Some(due));
        timer.arm(Some(due));
        if held_up(due) {
            return;
        }
        fired.recv_timeout(LONG).expect("the time");
        assert!(fired.recv_timeout(Duration::from_millis(50)).is_err());
    }

    #[test]
    fn a_fire_before_the_time_sets_the_timer_again() {
        let (timer, fired) = timer();
        let due = Instant::now() + Duration::from_millis(50);
        // The time is stored but the timer is not set: as if it had fired
        // for an earlier time, or a tick early. The early fire sets it.
        *lock(&timer.context().deadline) = Some(due);
        // As libdispatch calls it: with the context of a live timer.
        super::fired(timer.context.as_ptr().cast());
        assert!(fired.recv_timeout(LONG).expect("set again") >= due);
        assert!(fired.recv_timeout(Duration::from_millis(50)).is_err());
    }

    #[test]
    fn a_dropped_timer_does_not_fire_and_lets_go_of_its_function() {
        let (released, release) = mpsc::channel();
        let (send, fired) = mpsc::channel();
        let probe = Probe(released);
        let timer = Deadline::new(move || {
            let _ = &probe;
            let _ = send.send(());
        })
        .expect("a strict timer");
        timer.arm(Some(Instant::now() + Duration::from_secs(10)));
        drop(timer);
        release.recv_timeout(LONG).expect("let go of");
        assert!(fired.try_recv().is_err());
    }

    #[test]
    fn dropping_a_timer_while_it_notifies_neither_waits_nor_frees_early() {
        let (released, release) = mpsc::channel();
        let (entered, enter) = mpsc::channel();
        let (finish, finished) = mpsc::channel::<()>();
        let finished = Mutex::new(finished);
        let probe = Probe(released);
        let timer = Deadline::new(move || {
            let _ = &probe;
            let _ = entered.send(());
            let _ = lock(&finished).recv_timeout(LONG);
        })
        .expect("a strict timer");
        timer.arm(Some(Instant::now() + Duration::from_millis(5)));
        enter.recv_timeout(LONG).expect("notifying");
        drop(timer);
        assert!(release.try_recv().is_err(), "freed while notifying");
        finish.send(()).expect("still notifying");
        release.recv_timeout(LONG).expect("let go of once notified");
    }

    #[test]
    fn a_wait_woken_early_waits_on() {
        let (send, receive) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let thread = std::thread::current();
            let timer = Deadline::new({
                let thread = thread.clone();
                move || thread.unpark()
            })
            .expect("a strict timer");
            let due = Instant::now() + Duration::from_millis(50);
            send.send(thread).expect("sent");
            timer.wait_until(due);
            assert!(Instant::now() >= due);
        });
        receive.recv().expect("the thread").unpark();
        waiter.join().expect("waited");
    }
}
