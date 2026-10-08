//! Windows, input, text entry and the clipboard for WASIX guests running on
//! Wasmer.
//!
//! A guest includes `<wasmer/gui.h>` (`include/wasmer/gui.h`) and ends up
//! importing functions from the [`GUI_MODULE_NAME`] module; there is no guest
//! library. This crate provides those imports.
//!
//! Embedders install [`GuiRuntimeHooks`] on their WASIX runtime, as they do
//! with the WebGPU and N-API hooks:
//!
//! ```ignore
//! let ctx = wasmer_gui::GuiCtx::default();
//! runtime.with_instantiation_hook(ctx.runtime_hooks());
//! ```
//!
//! The host never calls into the guest. What it has to say is queued as event
//! records that the guest drains (`wguiInstancePollEvents`) or waits for
//! (`wguiInstanceWaitEvents`).

pub mod abi;
#[cfg(all(feature = "cli", not(target_arch = "wasm32")))]
pub mod cli;
mod clock;
mod ctx;
mod env;
// Windows nobody sees are for tests, servers and streams: not for a page.
#[cfg(not(target_arch = "wasm32"))]
pub mod headless;
mod host;
#[cfg(all(feature = "winit", not(target_arch = "wasm32")))]
pub mod native;
#[cfg(all(feature = "cli", not(target_arch = "wasm32")))]
pub mod script;
#[cfg(all(feature = "webgpu", not(target_arch = "wasm32")))]
mod surface;
pub mod system;
pub mod views;
#[cfg(feature = "wasix")]
mod wasix;
#[cfg(all(target_arch = "wasm32", feature = "js"))]
pub mod web;

pub use ctx::{
    ClipboardRead, GuiCtx, GuiCtxBuilder, GuiInstantiationState, GuiLimits, GuiPolicy,
    GuiRuntimeControl, GuiRuntimeHooks, GuiScope, GuiUsage,
};

/// Name of the import module every GUI import lives in.
pub const GUI_MODULE_NAME: &str = abi::IMPORT_MODULE;
/// Import modules named `wasmer_gui_v<N>` are GUI ABI revisions.
pub const GUI_MODULE_PREFIX: &str = "wasmer_gui_v";
/// The newest version of the API this build implements: `major << 16 | minor`,
/// what `wguiGetVersion` answers.
pub const GUI_ABI_VERSION: u32 = abi::enums::ABI_VERSION;

/// Which revision of the GUI import ABI a module was built against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuiAbi {
    /// `wasmer_gui_v0`. A module may import names from it that this build
    /// does not know: those are given stubs that answer 0, which the guest
    /// reads as `WGUIStatus_Unimplemented`.
    V0,
    /// A revision this build does not provide.
    Unknown,
}

/// Reports whether `module` imports the GUI ABI, and which revision.
pub fn module_needs_gui(module: &wasmer::Module) -> Option<GuiAbi> {
    let mut found = None;
    for import in module.imports() {
        let namespace = import.module();
        if !namespace.starts_with(GUI_MODULE_PREFIX) {
            continue;
        }
        found = Some(match (found, namespace == GUI_MODULE_NAME) {
            (Some(GuiAbi::Unknown), _) | (_, false) => GuiAbi::Unknown,
            (_, true) => GuiAbi::V0,
        });
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmer::{Module, Store};

    fn compile(wat: &str) -> Module {
        let store = Store::default();
        Module::new(&store, wat::parse_str(wat).expect("wat parses")).expect("module compiles")
    }

    #[test]
    fn modules_without_gui_imports_are_left_alone() {
        let module = compile(r#"(module (import "env" "f" (func)))"#);
        assert_eq!(module_needs_gui(&module), None);
    }

    #[test]
    fn imports_of_v0_select_it_whether_or_not_they_are_known() {
        let known =
            compile(r#"(module (import "wasmer_gui_v0" "wguiGetVersion" (func (result i32))))"#);
        assert_eq!(module_needs_gui(&known), Some(GuiAbi::V0));
        let newer =
            compile(r#"(module (import "wasmer_gui_v0" "wguiFromTheFuture" (func (result i32))))"#);
        assert_eq!(module_needs_gui(&newer), Some(GuiAbi::V0));
    }

    #[test]
    fn other_revisions_are_reported() {
        let module =
            compile(r#"(module (import "wasmer_gui_v9" "wguiGetVersion" (func (result i32))))"#);
        assert_eq!(module_needs_gui(&module), Some(GuiAbi::Unknown));
    }

    #[test]
    fn every_import_has_a_unique_name_and_a_status_result() {
        let mut names: Vec<_> = abi::IMPORTS.iter().map(|sig| sig.name).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total);
        // The rule that lets a host stub the imports it does not know: no
        // import returns anything but one i32, or nothing.
        for sig in abi::IMPORTS {
            assert!(
                matches!(sig.results, [] | [abi::ValType::I32]),
                "{} returns {:?}",
                sig.name,
                sig.results
            );
        }
    }
}
