//! Integration with the WASIX runtime's instantiation hooks.

use anyhow::{Context, Result};
use wasmer::{Imports, Instance, Memory, Module, StoreMut};
use wasmer_wasix::runtime::{InstantiationHook, InstantiationState};

use crate::{GuiInstantiationState, GuiRuntimeHooks};

/// Lets WASIX embedders register the hooks directly, e.g. through
/// `PluggableRuntime::with_instantiation_hook`. WASIX invokes them for every
/// instance of a process: the main module, each thread, each side module.
impl InstantiationHook for GuiRuntimeHooks {
    fn additional_imports(
        &self,
        module: &Module,
        store: &mut StoreMut,
    ) -> Result<(Imports, InstantiationState)> {
        let (imports, state) = GuiRuntimeHooks::additional_imports(self, module, store)?;
        Ok((imports, InstantiationState::new(state)))
    }

    fn prepare_imports(
        &self,
        module: &Module,
        store: &mut StoreMut,
        imports: &mut Imports,
    ) -> Result<InstantiationState> {
        let state = GuiRuntimeHooks::add_imports(self, module, store, imports)?;
        Ok(InstantiationState::new(state))
    }

    fn configure_new_instance(
        &self,
        module: &Module,
        store: &mut StoreMut,
        instance: &Instance,
        imported_memory: Option<&Memory>,
        state: InstantiationState,
    ) -> Result<()> {
        let state = state
            .take::<GuiInstantiationState>()
            .context("invalid GUI instance setup state")?;
        GuiRuntimeHooks::configure_instance(self, module, store, instance, imported_memory, state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_implement_the_wasix_hook_contract() {
        fn assert_hook<T: InstantiationHook>() {}
        assert_hook::<GuiRuntimeHooks>();
    }
}
