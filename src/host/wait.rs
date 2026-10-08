//! Waking the threads that wait for an instance's events.
//!
//! Whoever produces input must never wait for a lock: in a browser that is
//! the page's own thread, which is not allowed to block and would freeze the
//! tab if it spun on a lock whose holder was terminated. So nothing here
//! takes one. Waiters register in a lock-free list and park; a producer
//! empties the list and unparks.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::Thread;
use std::time::Duration;

use crossbeam_queue::SegQueue;

/// One waiting thread, for as long as the thread lives: a runtime's threads
/// outlive the programs they run, and wait for one instance after another.
pub(crate) struct Waiter {
    thread: Thread,
    /// The set whose list this waiter was last put in and not yet taken from,
    /// or 0. A set that goes away takes its list with it and leaves this as
    /// it was, which is why it names the set: being in a list that is gone,
    /// or in another instance's, is not being in this one.
    queued: AtomicU64,
}

thread_local! {
    static WAITER: Arc<Waiter> = Arc::new(Waiter {
        thread: std::thread::current(),
        queued: AtomicU64::new(0),
    });
}

pub(crate) struct WaitSet {
    /// Which set this is to the waiters in its list. Never 0, and no two
    /// sets of a process have the same.
    id: u64,
    waiters: SegQueue<Arc<Waiter>>,
    /// `wguiInstanceWake` was called and no wait has consumed it yet.
    woken: AtomicBool,
}

impl Default for WaitSet {
    fn default() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            waiters: SegQueue::new(),
            woken: AtomicBool::new(false),
        }
    }
}

impl WaitSet {
    /// Wakes every thread parked here. Lock-free.
    pub(crate) fn notify(&self) {
        while let Some(waiter) = self.waiters.pop() {
            // Unless it has gone to wait somewhere else since: then it is
            // in that list, and this only wakes it for nothing.
            let _ = waiter
                .queued
                .compare_exchange(self.id, 0, Ordering::AcqRel, Ordering::Relaxed);
            waiter.thread.unpark();
        }
    }

    /// `wguiInstanceWake`: ends the current wait, or the next one.
    pub(crate) fn wake(&self) {
        self.woken.store(true, Ordering::Release);
        self.notify();
    }

    /// Whether a wake is pending; it is consumed.
    pub(crate) fn take_wake(&self) -> bool {
        self.woken.swap(false, Ordering::AcqRel)
    }

    /// Parks the calling thread for at most `slice`, unless `ready` holds.
    ///
    /// `ready` is checked after the thread is in the list, so a notification
    /// that arrives between the caller's own check and the park is not lost:
    /// either `ready` sees what it was for, or the notifier finds the thread
    /// and unparks it. May return early for no reason; callers loop.
    pub(crate) fn park(&self, slice: Duration, ready: impl FnOnce() -> bool) {
        WAITER.with(|waiter| {
            if waiter.queued.swap(self.id, Ordering::AcqRel) != self.id {
                self.waiters.push(Arc::clone(waiter));
            }
        });
        if ready() {
            return;
        }
        std::thread::park_timeout(slice);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    use std::time::Instant;

    #[test]
    fn a_notification_ends_a_park_long_before_its_slice() {
        let set = Arc::new(WaitSet::default());
        let ready = Arc::new(AtomicBool::new(false));
        let waiter = {
            let (set, ready) = (Arc::clone(&set), Arc::clone(&ready));
            std::thread::spawn(move || {
                let started = Instant::now();
                while !ready.load(Ordering::Acquire) {
                    set.park(Duration::from_secs(30), || ready.load(Ordering::Acquire));
                }
                started.elapsed()
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        ready.store(true, Ordering::Release);
        set.notify();
        assert!(waiter.join().expect("joins") < Duration::from_secs(10));
    }

    #[test]
    fn a_thread_that_waited_for_a_set_that_is_gone_is_woken_by_the_next() {
        // A runtime's thread runs one program after another. The first one
        // here ends while the thread is still in its list.
        let waited = std::thread::spawn(|| {
            let first = WaitSet::default();
            first.park(Duration::from_millis(1), || false);
            drop(first);

            let second = Arc::new(WaitSet::default());
            let ready = Arc::new(AtomicBool::new(false));
            let producer = {
                let (second, ready) = (Arc::clone(&second), Arc::clone(&ready));
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(50));
                    ready.store(true, Ordering::Release);
                    second.notify();
                })
            };
            let started = Instant::now();
            while !ready.load(Ordering::Acquire) {
                second.park(Duration::from_secs(30), || ready.load(Ordering::Acquire));
            }
            producer.join().expect("joins");
            started.elapsed()
        });
        assert!(waited.join().expect("joins") < Duration::from_secs(10));
    }

    #[test]
    fn a_thread_waiting_for_two_sets_in_turn_is_woken_by_each() {
        let sets = [Arc::new(WaitSet::default()), Arc::new(WaitSet::default())];
        let work = Arc::new(AtomicU32::new(0));
        const ROUNDS: u32 = 200;
        let consumer = {
            let (sets, work) = (sets.clone(), Arc::clone(&work));
            std::thread::spawn(move || {
                for seen in 0..ROUNDS {
                    let set = &sets[seen as usize % 2];
                    while work.load(Ordering::Acquire) == seen {
                        set.park(Duration::from_secs(20), || {
                            work.load(Ordering::Acquire) != seen
                        });
                    }
                }
            })
        };
        let started = Instant::now();
        for round in 0..ROUNDS {
            // The consumer waits on the set of this round, sooner or later.
            std::thread::sleep(Duration::from_millis(1));
            work.fetch_add(1, Ordering::AcqRel);
            sets[round as usize % 2].notify();
        }
        consumer.join().expect("joins");
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "a wakeup was lost"
        );
    }

    #[test]
    fn a_wake_is_sticky_until_taken() {
        let set = WaitSet::default();
        assert!(!set.take_wake());
        set.wake();
        assert!(set.take_wake());
        assert!(!set.take_wake());
    }

    #[test]
    fn no_wakeup_is_lost_under_contention() {
        // Each round, the producer makes one unit of work and notifies; the
        // consumer must see every one without ever sleeping a full slice.
        let set = Arc::new(WaitSet::default());
        let work = Arc::new(AtomicU32::new(0));
        const ROUNDS: u32 = 2000;
        let consumer = {
            let (set, work) = (Arc::clone(&set), Arc::clone(&work));
            std::thread::spawn(move || {
                let mut seen = 0;
                while seen < ROUNDS {
                    while work.load(Ordering::Acquire) == seen {
                        set.park(Duration::from_secs(20), || {
                            work.load(Ordering::Acquire) != seen
                        });
                    }
                    seen = work.load(Ordering::Acquire);
                }
            })
        };
        let started = Instant::now();
        for _ in 0..ROUNDS {
            work.fetch_add(1, Ordering::AcqRel);
            set.notify();
            std::thread::yield_now();
        }
        consumer.join().expect("joins");
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "a wakeup was lost"
        );
    }
}
