//! Waking the threads that wait for an instance's events.
//!
//! Whoever produces input must never wait for a lock: in a browser that is
//! the page's own thread, which is not allowed to block and would freeze the
//! tab if it spun on a lock whose holder was terminated. So nothing here
//! takes one. Waiters register in a lock-free list and park; a producer
//! empties the list and unparks.
//!
//! A guest in a browser may instead wait by suspending (`super::web_wait`):
//! then it waits on a counter that every notification bumps.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};
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
    /// Bumped by every notification, before anyone is woken. A wait that
    /// suspends instead of parking (`super::web_wait`) reads it before it
    /// looks for what would end it, then waits for it to change: a
    /// notification after that read has changed it, so the wait cannot sleep
    /// through it.
    epoch: AtomicU32,
}

impl Default for WaitSet {
    fn default() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            waiters: SegQueue::new(),
            woken: AtomicBool::new(false),
            epoch: AtomicU32::new(0),
        }
    }
}

impl WaitSet {
    /// Wakes every thread parked here, and every wait suspended here.
    /// Lock-free.
    pub(crate) fn notify(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        #[cfg(all(target_arch = "wasm32", feature = "js"))]
        super::web_wait::notify(self.epoch_address());
        // Pairs with the fence in `park`: either the waiter's look sees what
        // this was called for, or this sees the waiter in the list.
        fence(Ordering::SeqCst);
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

    /// The notification counter (see `epoch`) as it is now.
    #[cfg(any(test, all(target_arch = "wasm32", feature = "js")))]
    pub(crate) fn epoch(&self) -> u32 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// Where the notification counter is in this module's memory.
    #[cfg(all(target_arch = "wasm32", feature = "js"))]
    pub(crate) fn epoch_address(&self) -> u32 {
        self.epoch.as_ptr() as u32
    }

    /// Parks the calling thread for at most `slice`, unless a wake is pending
    /// or `ready` holds.
    ///
    /// Both are checked after the thread is in the list, so a notification
    /// that arrives between the caller's own check and the park is not lost:
    /// either the check sees what it was for, or the notifier finds the thread
    /// and unparks it. A pending wake is only looked at here; the caller takes
    /// it. May return early for no reason; callers loop.
    pub(crate) fn park(&self, slice: Duration, ready: impl FnOnce() -> bool) {
        WAITER.with(|waiter| {
            if waiter.queued.swap(self.id, Ordering::AcqRel) != self.id {
                self.waiters.push(Arc::clone(waiter));
            }
        });
        #[cfg(test)]
        tests::in_the_list();
        // Pairs with the fence in `notify`, which comes after whatever it
        // was called for (a wake, an event) and before it looks at the list.
        fence(Ordering::SeqCst);
        if self.woken.load(Ordering::Acquire) || ready() {
            return;
        }
        #[cfg(test)]
        parking();
        std::thread::park_timeout(slice);
    }
}

#[cfg(test)]
thread_local! {
    /// Told each time `park` on this thread is done looking and parks: a test
    /// that wakes the thread then knows that only an unpark can end the park.
    pub(crate) static PARKING: std::cell::RefCell<Option<std::sync::mpsc::Sender<()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn parking() {
    PARKING.with_borrow(|parking| {
        if let Some(parking) = parking {
            // The test may have stopped listening.
            let _ = parking.send(());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::atomic::AtomicU32;
    use std::time::Instant;

    thread_local! {
        /// Run by `park` on this thread once the thread is in the list,
        /// before it looks at anything.
        static IN_THE_LIST: Cell<Option<Box<dyn FnOnce()>>> = const { Cell::new(None) };
    }

    pub(super) fn in_the_list() {
        if let Some(hook) = IN_THE_LIST.take() {
            hook();
        }
    }

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

    fn assert_wake_at_park_boundary_returns(before_registration: bool) {
        // A fresh thread has no old waiter registration or unpark token.
        let set = Arc::new(WaitSet::default());
        let (done, completion) = std::sync::mpsc::channel();
        let worker = {
            let set = Arc::clone(&set);
            std::thread::spawn(move || {
                // Mirrors instance_wait_events' outer-loop check.
                assert!(!set.take_wake());
                if before_registration {
                    assert!(set.waiters.is_empty());
                    set.wake();
                }
                set.park(Duration::from_secs(30), || {
                    if !before_registration {
                        // The waiter exists, but park_timeout has not run.
                        set.wake();
                    }
                    false
                });
                done.send(set.take_wake()).expect("completion receiver");
            })
        };
        let result = completion.recv_timeout(Duration::from_secs(1));
        // Release a regressed implementation without waiting for its slice.
        set.notify();
        worker.join().expect("joins");
        assert_eq!(result, Ok(true), "pending wake slept through park");
    }

    #[test]
    fn a_wake_between_outer_check_and_registration_does_not_park() {
        assert_wake_at_park_boundary_returns(true);
    }

    #[test]
    fn a_wake_just_before_the_park_ends_it_at_once() {
        assert_wake_at_park_boundary_returns(false);
    }

    /// A flag one thread raises and another waits for. It spins rather than
    /// parks: a park there could use up the unpark a test is about.
    #[derive(Clone, Default)]
    struct Gate(Arc<AtomicBool>);

    impl Gate {
        fn open(&self) {
            self.0.store(true, Ordering::Release);
        }

        fn wait(&self) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !self.0.load(Ordering::Acquire) {
                assert!(Instant::now() < deadline, "the other thread never came");
                std::thread::yield_now();
            }
        }
    }

    /// Where a waiting thread is held while another thread wakes the set.
    #[derive(Clone, Copy)]
    enum Hold {
        /// Past its own look at the wake, not yet in the list. The waker
        /// finds nobody to unpark: only the look `park` takes once the
        /// thread is in the list can end the wait.
        BeforeTheList,
        /// In the list, before `park` looks. The waker finds the thread:
        /// the look sees the wake, or the unpark ends the park.
        InTheList,
        /// In the list, its look at the wake taken and missed. Only the
        /// unpark can end the park.
        AfterTheLook,
    }

    /// The waking thread acts while the waiting one is held, and the gates
    /// order the two: this checks what each side does at the boundary, not
    /// how their accesses may be reordered (the loom model below does).
    fn assert_a_wake_from_another_thread_ends_the_wait(hold: Hold) {
        // A fresh thread has no old waiter registration or unpark token.
        let set = Arc::new(WaitSet::default());
        let (held, woken) = (Gate::default(), Gate::default());
        let (done, completion) = std::sync::mpsc::channel();
        let waiter = {
            let (set, held, woken) = (Arc::clone(&set), held.clone(), woken.clone());
            std::thread::spawn(move || {
                // Mirrors instance_wait_events' outer-loop check.
                assert!(!set.take_wake());
                let wait_for_the_wake = move || {
                    held.open();
                    woken.wait();
                };
                let mut ready = None;
                match hold {
                    Hold::BeforeTheList => wait_for_the_wake(),
                    Hold::InTheList => IN_THE_LIST.set(Some(Box::new(wait_for_the_wake))),
                    Hold::AfterTheLook => ready = Some(wait_for_the_wake),
                }
                set.park(Duration::from_secs(30), || {
                    if let Some(wait_for_the_wake) = ready {
                        wait_for_the_wake();
                    }
                    false
                });
                done.send(set.take_wake()).expect("completion receiver");
            })
        };
        held.wait();
        set.wake();
        woken.open();
        let result = completion.recv_timeout(Duration::from_secs(5));
        // Release a regressed implementation without waiting for its slice.
        set.notify();
        waiter.join().expect("joins");
        assert_eq!(
            result,
            Ok(true),
            "a wake from another thread slept through park"
        );
    }

    #[test]
    fn a_wake_from_another_thread_before_registration_does_not_park() {
        assert_a_wake_from_another_thread_ends_the_wait(Hold::BeforeTheList);
    }

    #[test]
    fn a_wake_from_another_thread_after_registration_ends_the_park() {
        assert_a_wake_from_another_thread_ends_the_wait(Hold::InTheList);
    }

    #[test]
    fn a_wake_from_another_thread_after_the_look_ends_the_park() {
        assert_a_wake_from_another_thread_ends_the_wait(Hold::AfterTheLook);
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

/// The handshake between `park` and `notify` under loom (`make test-loom`),
/// which runs the model in every interleaving and lets each load read any
/// store the memory model allows it to. The tests above can only order the
/// two threads; this is about how each one's accesses look to the other.
///
/// The waiter registers, fences and looks; the notifier publishes what it
/// was called for, fences and looks for waiters. Either the waiter's look
/// sees what was published, or the notifier finds the waiter and unparks
/// it: std's park returns at once for an unpark that came before it. Being
/// in the list is one flag, written and read Relaxed: the fences have to be
/// enough, whatever order crossbeam's queue gives its own accesses. It is a
/// model of the handshake, not of this code: the queue, a waiter's `queued`
/// mark and parking itself are left out.
///
/// loom 0.7 implements `fence(SeqCst)`, but not SeqCst loads and stores (it
/// takes them for AcqRel). It gives such a fence a happens-before edge to
/// the next one, which is more than C11 does; for this pattern, two threads
/// and atomics only, the two agree. It does not explore load buffering,
/// which this pattern has no part in.
#[cfg(all(test, loom))]
mod model {
    use loom::sync::Arc;
    use loom::sync::atomic::{AtomicBool, Ordering, fence};

    #[test]
    fn a_waiter_parks_only_where_its_notifier_finds_it() {
        loom::model(|| {
            // The waiter is in the list.
            let listed = Arc::new(AtomicBool::new(false));
            // What the notifier was called for: the sticky wake, or an event
            // `ready` looks for.
            let woken = Arc::new(AtomicBool::new(false));
            let notifier = {
                let (listed, woken) = (Arc::clone(&listed), Arc::clone(&woken));
                loom::thread::spawn(move || {
                    // `wake`, then `notify`.
                    woken.store(true, Ordering::Release);
                    fence(Ordering::SeqCst);
                    listed.load(Ordering::Relaxed)
                })
            };
            // `instance_wait_events` takes a pending wake before it parks.
            // `take_wake` swaps false in; this returns the same and leaves
            // the flag the same, but doesn't write when there is nothing to
            // take. With the swap, loom reports a lost wake that C11 rules
            // out: the swap read the initial false, so it comes before the
            // notifier's store in the flag's modification order, but loom
            // orders a store only after those its thread has seen, and lets
            // the look after the fence read the swap's false.
            let taken = woken
                .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
                .is_ok();
            // `park`.
            let returned = taken || {
                listed.store(true, Ordering::Relaxed);
                fence(Ordering::SeqCst);
                woken.load(Ordering::Acquire)
            };
            let found = notifier.join().expect("joins");
            assert!(
                returned || found,
                "the waiter parked where its notifier won't find it"
            );
        });
    }
}
