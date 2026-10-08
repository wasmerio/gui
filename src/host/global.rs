//! The functions of `gui.h` that belong to no object.

use std::sync::Arc;

use super::shared::InstanceShared;
use super::sink::InstanceSink;
use super::{Host, Instance, Object, ObjectKind};
use crate::abi::api::GlobalApi;
use crate::abi::enums::*;
use crate::abi::structs::InstanceDescriptor;
use crate::abi::{AbiEnum, AbiResult, Cx, Handle, OutPtr};
use crate::ctx::ClipboardRead;

/// Unread records an instance may hold when the embedder set no limit.
const DEFAULT_MAX_PENDING_EVENTS: usize = 1024;

impl Host {
    /// `wguiCreateInstance`, up to handing the handle to the guest.
    pub(super) fn open_instance(&self, descriptor: &InstanceDescriptor) -> Result<Handle, Status> {
        // A guest built for another major version expects other behaviour
        // than this host has. (0 is a descriptor that was zeroed rather than
        // initialised: it gets this version's.)
        if descriptor.abi_version != 0 && descriptor.abi_version >> 16 != ABI_VERSION >> 16 {
            return Err(Status::Unsupported);
        }
        let features = self.features();
        if descriptor
            .required_features
            .iter()
            .any(|feature| !features.contains(feature))
        {
            return Err(Status::Unsupported);
        }
        let limits = &self.ctx.limits;
        if limits
            .max_instances
            .is_some_and(|max| self.count(ObjectKind::Instance) >= max)
        {
            return Err(Status::LimitExceeded);
        }

        let system = Arc::clone(&self.ctx.system);
        let shared = Arc::new(InstanceShared::new(
            Arc::clone(&system),
            limits
                .max_pending_events
                .unwrap_or(DEFAULT_MAX_PENDING_EVENTS),
            descriptor.event_mask,
            self.ctx.policy.clipboard_read == ClipboardRead::Deny,
        ));
        let state = Arc::new(Instance {
            shared: Arc::clone(&shared),
            application_id: descriptor.application_id.clone(),
        });
        let handle = self
            .insert(Object::Instance(state), 0)
            .ok_or(Status::LimitExceeded)?;
        system.attach(InstanceSink::new(
            &shared,
            self.ctx.policy.raw_input && self.ctx.policy.raw_input_unfocused,
        ));
        Ok(handle)
    }
}

impl GlobalApi for Host {
    fn get_version(&self, _cx: &mut Cx<'_>) -> u32 {
        ABI_VERSION
    }

    fn has_feature(&self, _cx: &mut Cx<'_>, feature: FeatureName) -> bool {
        self.has(feature)
    }

    fn get_features(
        &self,
        cx: &mut Cx<'_>,
        capacity: u32,
        features: OutPtr<u32>,
        count: OutPtr<u32>,
    ) -> AbiResult<Status> {
        let all = self.features();
        if !features.is_null() {
            for (index, feature) in all.iter().take(capacity as usize).enumerate() {
                features.write_at(&cx.mem, index as u32, &feature.as_u32())?;
            }
        }
        count.write(&cx.mem, &(all.len() as u32))?;
        Ok(Status::Success)
    }

    fn create_instance(
        &self,
        cx: &mut Cx<'_>,
        descriptor: Option<&InstanceDescriptor>,
        instance: OutPtr<u32>,
    ) -> AbiResult<Status> {
        let default = InstanceDescriptor {
            abi_version: ABI_VERSION,
            event_mask: event_class::DEFAULT,
            ..InstanceDescriptor::default()
        };
        let handle = match self.open_instance(descriptor.unwrap_or(&default)) {
            Ok(handle) => handle,
            Err(status) => return Ok(status),
        };
        if let Err(error) = instance.write(&cx.mem, &handle) {
            self.destroy_instance(handle);
            return Err(error);
        }
        cx.owned.push(handle);
        Ok(Status::Success)
    }
}
