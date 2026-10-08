//! Calls every import with arguments no program should pass: null and wild
//! pointers, handles of the wrong kind, every bit set. Whatever a guest does,
//! the host must answer with a status or trap the guest for a pointer outside
//! its memory. It must never panic, and a function that exists must never
//! answer `WGUIStatus_Unimplemented`.

use std::sync::Arc;

use wasmer::{AsStoreMut, Function, Instance, Memory, Module, Store, Value};
use wasmer_gui::abi::{IMPORTS, ImportSig, ValType};
use wasmer_gui::headless::{HeadlessOptions, HeadlessSystem};
use wasmer_gui::{ClipboardRead, GuiCtx, GuiPolicy, GuiScope};

/// Scratch memory the baseline pointers point at: zeroed, inside the first page.
const SCRATCH: i32 = 4096;
const PAGES: u32 = 2;

struct Guest {
    store: Store,
    instance: Instance,
    memory: Memory,
    /// The instance the live handles were last made on.
    live: Option<i32>,
    // Kept so that the scope outlives the calls.
    _scope: GuiScope,
}

fn guest() -> Guest {
    let mut wat = String::from("(module\n");
    let ty = |t: &ValType| match t {
        ValType::I32 => "i32",
        ValType::I64 => "i64",
        ValType::F32 => "f32",
        ValType::F64 => "f64",
    };
    for sig in IMPORTS {
        let params: Vec<_> = sig.params.iter().map(ty).collect();
        let results: Vec<_> = sig.results.iter().map(ty).collect();
        wat.push_str(&format!(
            "(import \"wasmer_gui_v0\" \"{0}\" (func ${0} (param {1}) (result {2})))\n(export \"{0}\" (func ${0}))\n",
            sig.name,
            params.join(" "),
            results.join(" ")
        ));
    }
    wat.push_str(&format!("(memory (export \"memory\") {PAGES}))"));

    let system = HeadlessSystem::new(HeadlessOptions::default());
    let ctx = GuiCtx::builder()
        .window_system(Arc::new(system))
        .policy(GuiPolicy {
            clipboard_read: ClipboardRead::Allow,
            raw_input_unfocused: true,
            ..GuiPolicy::default()
        })
        .build();
    let scope = ctx.scope();
    let hooks = scope.hooks();
    let mut store = Store::default();
    let module = Module::new(&store, wat::parse_str(&wat).expect("wat parses")).expect("compiles");
    let (imports, state) = hooks
        .additional_imports(&module, &mut store.as_store_mut())
        .expect("imports register");
    let instance = Instance::new(&mut store, &module, &imports).expect("instantiates");
    hooks
        .configure_instance(&module, &mut store.as_store_mut(), &instance, None, state)
        .expect("configures");
    let memory = instance
        .exports
        .get_memory("memory")
        .expect("exported")
        .clone();
    Guest {
        store,
        instance,
        memory,
        live: None,
        _scope: scope,
    }
}

impl Guest {
    fn function(&self, name: &str) -> Function {
        self.instance
            .exports
            .get_function(name)
            .unwrap_or_else(|_| panic!("{name} is exported"))
            .clone()
    }

    fn write(&self, at: i32, bytes: &[u8]) {
        self.memory
            .view(&self.store)
            .write(at as u64, bytes)
            .expect("in bounds");
    }

    fn read_u32(&self, at: i32) -> u32 {
        let mut bytes = [0u8; 4];
        self.memory
            .view(&self.store)
            .read(at as u64, &mut bytes)
            .expect("in bounds");
        u32::from_le_bytes(bytes)
    }

    fn clear_scratch(&self) {
        self.write(SCRATCH, &[0u8; 2048]);
    }

    /// Calls an import. `Ok` is what it returned; `Err` the trap's message.
    fn call(&mut self, name: &str, arguments: &[Value]) -> Result<Option<i32>, String> {
        let function = self.function(name);
        match function.call(&mut self.store, arguments) {
            Ok(results) => Ok(results.first().map(|value| value.unwrap_i32())),
            Err(trap) => Err(trap.to_string()),
        }
    }

    /// Makes one live handle of each kind: instance, window, display, cursor.
    /// The ones it made before are destroyed.
    fn live_handles(&mut self) -> [i32; 4] {
        if let Some(previous) = self.live.take() {
            let _ = self.call("wguiInstanceDestroy", &[Value::I32(previous)]);
        }
        let out = SCRATCH + 1024;
        let ok = |status: Result<Option<i32>, String>| assert_eq!(status, Ok(Some(1)));
        ok(self.call("wguiCreateInstance", &[Value::I32(0), Value::I32(out)]));
        let instance = self.read_u32(out) as i32;
        ok(self.call(
            "wguiInstanceCreateWindow",
            &[Value::I32(instance), Value::I32(0), Value::I32(out)],
        ));
        let window = self.read_u32(out) as i32;
        ok(self.call(
            "wguiInstanceGetDisplays",
            &[
                Value::I32(instance),
                Value::I32(1),
                Value::I32(out),
                Value::I32(out + 8),
            ],
        ));
        let display = self.read_u32(out) as i32;
        // A 1x1 cursor: its descriptor at `out + 64`, its pixel at `out + 128`.
        let descriptor = out + 64;
        let mut bytes = [0u8; 40];
        bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
        bytes[8..12].copy_from_slice(&1u32.to_le_bytes());
        bytes[24..32].copy_from_slice(&1.0f64.to_bits().to_le_bytes());
        bytes[32..36].copy_from_slice(&((out + 128) as u32).to_le_bytes());
        bytes[36..40].copy_from_slice(&4u32.to_le_bytes());
        self.write(descriptor, &bytes);
        ok(self.call(
            "wguiInstanceCreateCursor",
            &[
                Value::I32(instance),
                Value::I32(descriptor),
                Value::I32(out),
            ],
        ));
        let cursor = self.read_u32(out) as i32;
        assert!(instance != 0 && window != 0 && display != 0 && cursor != 0);
        self.live = Some(instance);
        self.clear_scratch();
        [instance, window, display, cursor]
    }
}

fn baseline(ty: &ValType) -> Value {
    match ty {
        ValType::I32 => Value::I32(SCRATCH),
        ValType::I64 => Value::I64(0),
        ValType::F32 => Value::F32(0.0),
        ValType::F64 => Value::F64(0.0),
    }
}

fn variations(ty: &ValType, live: &[i32; 4]) -> Vec<Value> {
    let memory_end = (PAGES * 65536) as i32;
    match ty {
        ValType::I32 => [
            0,
            1,
            2,
            7,
            -1,
            i32::MIN,
            i32::MAX,
            SCRATCH,
            // Unaligned, the last byte of memory, just past it, far past it.
            SCRATCH + 3,
            memory_end - 1,
            memory_end,
            memory_end + 4096,
            live[0],
            live[1],
            live[2],
            live[3],
            // A stale handle: the right slot, another generation.
            live[1] ^ 0x0010_0000,
        ]
        .into_iter()
        .map(Value::I32)
        .collect(),
        ValType::I64 => [0, 1, -1, i64::MIN, 0x8000, 1 << 40]
            .into_iter()
            .map(Value::I64)
            .collect(),
        ValType::F32 => [0.0, -1.0, f32::NAN, f32::INFINITY]
            .into_iter()
            .map(Value::F32)
            .collect(),
        ValType::F64 => [0.0, -1.0, 1e300, f64::NAN, f64::NEG_INFINITY]
            .into_iter()
            .map(Value::F64)
            .collect(),
    }
}

fn check(sig: &ImportSig, arguments: &[Value], outcome: &Result<Option<i32>, String>) {
    match outcome {
        Ok(None) => assert!(sig.results.is_empty(), "{} returned nothing", sig.name),
        Ok(Some(value)) => {
            let known = match sig.name {
                // The two functions whose result is not a status.
                "wguiGetVersion" => true,
                "wguiHasFeature" => matches!(value, 0 | 1),
                _ => (1..=13).contains(value),
            };
            assert!(known, "{}{arguments:?} returned {value}", sig.name);
        }
        // The one reason to trap a guest: a pointer outside its memory.
        Err(trap) => assert!(
            trap.contains("outside guest memory"),
            "{}{arguments:?} trapped: {trap}",
            sig.name
        ),
    }
}

#[test]
fn no_argument_makes_the_host_misbehave() {
    let mut guest = guest();
    let mut calls = 0;
    for sig in IMPORTS {
        let base: Vec<Value> = sig.params.iter().map(baseline).collect();
        // One argument at a time takes every value, the others staying put;
        // first with the baseline, then with a live handle in the receiver's
        // place so that the checks behind the handle are reached too.
        for receiver in 0..5 {
            for position in 0..sig.params.len().max(1) {
                let mut live = guest.live_handles();
                let candidates = match sig.params.get(position) {
                    Some(ty) => variations(ty, &live),
                    None => vec![Value::I32(0)],
                };
                for candidate in candidates {
                    let mut arguments = base.clone();
                    if receiver > 0
                        && let Some(first) = arguments.first_mut()
                        && matches!(sig.params[0], ValType::I32)
                    {
                        *first = Value::I32(live[receiver - 1]);
                    }
                    if let Some(slot) = arguments.get_mut(position) {
                        *slot = candidate;
                    }
                    // A wait on a live instance would sit out its timeout.
                    if sig.blocking && sig.name == "wguiInstanceWaitEvents" {
                        arguments[1] = Value::I64(0);
                    }
                    let outcome = guest.call(sig.name, &arguments);
                    check(sig, &arguments, &outcome);
                    calls += 1;
                    // What a call destroyed is made anew for the next one.
                    if sig.name.ends_with("Destroy") {
                        live = guest.live_handles();
                    }
                    guest.clear_scratch();
                }
            }
        }
    }
    assert!(calls > 5000, "{calls} calls");
}

#[test]
fn every_import_is_swept() {
    // The sweep goes by the import table, so the table has to be the API.
    assert_eq!(IMPORTS.len(), 42);
    let blocking: Vec<_> = IMPORTS
        .iter()
        .filter(|sig| sig.blocking)
        .map(|sig| sig.name)
        .collect();
    assert_eq!(
        blocking,
        ["wguiInstanceWaitEvents", "wguiInstanceCreateWindow"]
    );
}
