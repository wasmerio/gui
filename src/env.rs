//! Per-instance state behind the GUI imports.

use std::sync::Arc;

use crate::abi::Handle;
use crate::host::Host;

/// One of these backs the imports of each guest wasm instance: the main
/// module of a process, every thread it spawns and every library it loads
/// each get their own.
pub struct GuiEnv {
    /// The instance's linear memory, known once the instance exists.
    pub(crate) memory: Option<wasmer::Memory>,
    /// What the instances of one scope share. This is what lets a handle
    /// created on one guest thread work on another.
    pub(crate) host: Arc<Host>,
    /// The GUI instances this wasm instance created and has not destroyed.
    pub(crate) owned: Vec<Handle>,
    /// Tells whether the guest's process was killed, where the runtime lets
    /// that be seen: a WASIX kill disables the atomics of the process's
    /// memory, and this handle on that memory notices.
    pub(crate) kill_probe: Option<wasmer::MemoryOps>,
    /// The runtime's own yield, where these imports sit beside the system's.
    /// A program's signals are delivered where it enters the system, and
    /// asking for events is not that: the host makes the call for it (see
    /// [`let_signals_in`](crate::host::let_signals_in)).
    pub(crate) system_yield: Option<wasmer::Function>,
    /// When the host last made that call, on [`crate::clock`].
    pub(crate) yielded: Option<std::time::Duration>,
}

impl GuiEnv {
    pub(crate) fn new(host: Arc<Host>) -> Self {
        Self {
            memory: None,
            host,
            owned: Vec::new(),
            kill_probe: None,
            system_yield: None,
            yielded: None,
        }
    }
}

impl Drop for GuiEnv {
    /// A program that ends without tidying up (most do: they just exit) must
    /// not leave its windows behind. What a wasm instance created goes when
    /// that instance goes, which for a process is when its threads end.
    fn drop(&mut self) {
        for instance in self.owned.drain(..) {
            self.host.destroy_instance(instance);
        }
    }
}
