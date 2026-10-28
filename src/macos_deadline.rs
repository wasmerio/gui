//! A timer macOS keeps to, for waits that end at a frame's deadline.
//!
//! macOS coalesces timers: a thread that sleeps until the display's next
//! refresh, or a loop told to wait until then, can be woken milliseconds
//! late, up to half a frame at 60 Hz. A libdispatch timer made with
//! `DISPATCH_TIMER_STRICT` and no leeway fires within a fraction of a
//! millisecond of its time. It only wakes somebody: it is not a display link,
//! and it says nothing of when a frame reaches the screen. It fires once for
//! each time it is armed, and nothing is left armed while nobody waits.

use std::ffi::{c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{self, NonNull};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

/// A libdispatch object (`dispatch_object_t` and its kinds), reference
/// counted.
type Object = *mut c_void;

/// `struct dispatch_source_type_s`: only its address is used.
#[repr(C)]
struct SourceType {
    _opaque: [u8; 0],
}

const DISPATCH_TIME_NOW: u64 = 0;
const DISPATCH_TIME_FOREVER: u64 = !0;
const DISPATCH_TIMER_STRICT: usize = 0x1;

// libdispatch is part of libSystem, which every program on macOS links.
#[link(name = "System")]
unsafe extern "C" {
    /// What `DISPATCH_SOURCE_TYPE_TIMER` is the address of.
    static _dispatch_source_type_timer: SourceType;
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> Object;
    fn dispatch_source_create(
        kind: *const SourceType,
        handle: usize,
        mask: usize,
        queue: Object,
    ) -> Object;
    fn dispatch_set_context(object: Object, context: *mut c_void);
    fn dispatch_source_set_event_handler_f(
        source: Object,
        handler: unsafe extern "C" fn(*mut c_void),
    );
    fn dispatch_source_set_cancel_handler_f(
        source: Object,
        handler: unsafe extern "C" fn(*mut c_void),
    );
    fn dispatch_source_set_timer(source: Object, start: u64, interval: u64, leeway: u64);
    fn dispatch_source_cancel(source: Object);
    fn dispatch_time(when: u64, delta: i64) -> u64;
    fn dispatch_activate(object: Object);
    fn dispatch_release(object: Object);
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What the timer's handlers share with its [`Deadline`]. The cancel handler
/// frees it, and libdispatch runs that once, after the last event handler
/// has returned: while the context is there, so is the source, which
/// libdispatch lets go of only after that.
///
/// Threads share it through a pointer, which the compiler does not check: it
/// is `Sync` all the same (a mutex, a `Sync` function, and a source whose
/// functions may be called from any thread).
struct Context {
    source: NonNull<c_void>,
    /// The time the timer is armed for. Kept locked while the timer is set,
    /// so that what is set is always the time stored last.
    deadline: Mutex<Option<Instant>>,
    /// Runs on a libdispatch thread: it may wake somebody, and do no more.
    /// No guest code, and nothing of AppKit.
    notify: Box<dyn Fn() + Send + Sync>,
}

impl Context {
    /// Sets the timer to fire once at `deadline`, or not at all. Called with
    /// `deadline` locked, or before the source is activated; libdispatch does
    /// not wait for a handler here.
    fn set_timer(&self, deadline: Option<Instant>) {
        let start = match deadline {
            None => DISPATCH_TIME_FOREVER,
            // Both count the same uptime: `Instant` is CLOCK_UPTIME_RAW, and
            // `dispatch_time` from now is mach_absolute_time(). Converting
            // nanoseconds to ticks rounds down, so the timer may fire a tick
            // before `deadline`: `fired` sets it again then.
            Some(due) => {
                let delta = due.saturating_duration_since(Instant::now()).as_nanos();
                let delta = i64::try_from(delta).unwrap_or(i64::MAX);
                // SAFETY: a plain function of two numbers.
                unsafe { dispatch_time(DISPATCH_TIME_NOW, delta) }
            }
        };
        // SAFETY: the source outlives its context (see `Context`). An
        // interval of DISPATCH_TIME_FOREVER makes the timer fire once.
        unsafe {
            dispatch_source_set_timer(self.source.as_ptr(), start, DISPATCH_TIME_FOREVER, 0);
        }
    }
}

/// The timer's event handler, on a libdispatch thread.
///
/// # Safety
///
/// `context` is the context of a live timer (see `Context`).
unsafe extern "C" fn fired(context: *mut c_void) {
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
/// event handler has returned, and none run after it.
///
/// # Safety
///
/// `context` is a timer's context, which nothing uses any more.
unsafe extern "C" fn cancelled(context: *mut c_void) {
    // SAFETY: leaked from a box in `Deadline::new`, and freed only here.
    let context = unsafe { Box::from_raw(context.cast::<Context>()) };
    // What `notify` captured is dropped here: it must not unwind either.
    let _ = catch_unwind(AssertUnwindSafe(move || drop(context)));
}

/// A one-shot timer that macOS fires on time, and calls a function when it
/// does.
pub(crate) struct Deadline {
    context: NonNull<Context>,
}

impl Deadline {
    /// A timer that calls `notify` on a thread of libdispatch's when it
    /// fires. `notify` may wake a thread or a loop, and do no more; it may
    /// still be running when the timer is dropped, so it holds nothing it
    /// borrows (a `Weak`, a `Thread`). `None` if libdispatch makes no timer,
    /// which it does only for arguments it rejects.
    pub(crate) fn new(notify: impl Fn() + Send + Sync + 'static) -> Option<Self> {
        // SAFETY: a label that lives for the program, and serial attributes.
        let queue = unsafe { dispatch_queue_create(c"wasmer-gui-deadline".as_ptr(), ptr::null()) };
        let queue = NonNull::new(queue)?;
        // SAFETY: the timer type, no handle, a timer flag, a queue of ours.
        let source = unsafe {
            dispatch_source_create(
                &raw const _dispatch_source_type_timer,
                0,
                DISPATCH_TIMER_STRICT,
                queue.as_ptr(),
            )
        };
        // The source keeps its queue: this reference is not needed.
        // SAFETY: the reference `dispatch_queue_create` returned.
        unsafe { dispatch_release(queue.as_ptr()) };
        let source = NonNull::new(source)?;
        let context = NonNull::from(Box::leak(Box::new(Context {
            source,
            deadline: Mutex::new(None),
            notify: Box::new(notify),
        })));
        // SAFETY: an inactive source of ours, and the context just made for
        // it. Nothing runs before the source is activated.
        unsafe {
            // Not before `arm` sets it.
            context.as_ref().set_timer(None);
            dispatch_set_context(source.as_ptr(), context.as_ptr().cast());
            dispatch_source_set_event_handler_f(source.as_ptr(), fired);
            dispatch_source_set_cancel_handler_f(source.as_ptr(), cancelled);
            dispatch_activate(source.as_ptr());
        }
        Some(Self { context })
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
        let source = {
            let context = self.context();
            // A handler that has not looked yet notifies nobody.
            *lock(&context.deadline) = None;
            context.source.as_ptr()
        };
        // Not waited for: a notification on its way may be waiting for a lock
        // whoever drops this holds. The cancel handler frees the context once
        // the last event handler has returned; it may do so from here on.
        // SAFETY: the source is alive until this releases the reference
        // `dispatch_source_create` returned.
        unsafe {
            dispatch_source_cancel(source);
            dispatch_release(source);
        }
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
        // SAFETY: the context of a live timer.
        unsafe { super::fired(timer.context.as_ptr().cast()) };
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
