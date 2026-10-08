//! What Ctrl-C does to a program that has not said what it should.
//!
//! On WASIX the default action of a signal is carried out by the C library
//! the program is linked with, and wasix-libc does not manage it. Until the
//! program has called `sigaction` once, the library calls through a null
//! pointer for any signal that arrives, which ends the program without a
//! word: a terminal that was resized sends one. After such a call, ending
//! the program goes through `abort`, which raises a signal of its own and
//! prints a line about it, again and again, until the stack is used up.
//!
//! A program with a window is ended by SIGINT (Ctrl-C in the terminal it was
//! started from) and by SIGTERM, so these two get a handler here that does
//! what the default action is meant to: it ends the program at once, with
//! the exit status shells give a program that a signal ended. A handler the
//! program installed itself, before the event loop was made or after, is
//! left alone. Having installed one, the library's other default actions
//! work too, and a resized terminal is ignored as it should be.
//!
//! Signals reach the program where it asks the host for events and where a
//! wait for them returns, so this works however the loop runs.

use std::os::raw::c_int;
use std::sync::Once;

const SIGINT: c_int = 2;
const SIGTERM: c_int = 15;
/// `SIG_DFL` and `SIG_ERR`, as `signal` returns them.
const DEFAULT: usize = 0;
const FAILED: usize = usize::MAX;

extern "C" {
    fn signal(number: c_int, handler: usize) -> usize;
    fn _exit(status: c_int) -> !;
}

extern "C" fn end(number: c_int) {
    // Not `exit`: a program a signal ends runs nothing more of its own.
    unsafe { _exit(128 + number) }
}

/// Called when an event loop is made. Once is enough for a process.
pub(crate) fn install() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        for number in [SIGINT, SIGTERM] {
            // SAFETY: `end` is a `void (*)(int)`, and a WASIX signal handler
            // runs on the thread it interrupts, between two of its calls
            // into the system.
            unsafe {
                let handler: extern "C" fn(c_int) = end;
                let previous = signal(number, handler as usize);
                if previous != DEFAULT && previous != FAILED {
                    // The program decided before this: its choice stands.
                    signal(number, previous);
                }
            }
        }
    });
}
