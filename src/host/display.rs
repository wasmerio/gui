//! `WGUIDisplay`.

use std::sync::Arc;

use super::shared::InstanceShared;
use super::{Display, Host, Object, lock};
use crate::abi::api::DisplayApi;
use crate::abi::enums::*;
use crate::abi::structs::DisplayInfo;
use crate::abi::{AbiResult, Cx, Handle, OutPtr};
use crate::system::DisplayData;

impl Host {
    /// The handle an instance knows a display of the window system by, made
    /// on first use. 0 for no display, or when the table is full.
    pub(super) fn display_handle(
        &self,
        instance: Handle,
        shared: &InstanceShared,
        id: u64,
    ) -> Handle {
        if id == 0 {
            return 0;
        }
        let mut displays = lock(&shared.displays);
        if let Some((_, handle)) = displays.iter().find(|(known, _)| *known == id) {
            return *handle;
        }
        let Some(handle) = self.insert(Object::Display(Arc::new(Display { id })), instance) else {
            return 0;
        };
        displays.push((id, handle));
        handle
    }

    /// What the window system says about the display behind a handle now.
    fn display_data(&self, display: Handle) -> Result<DisplayData, Status> {
        let display = self.display(display).ok_or(Status::InvalidHandle)?;
        self.ctx
            .system
            .displays()
            .into_iter()
            .find(|data| data.id == display.id)
            // It was unplugged since the guest listed it.
            .ok_or(Status::NotFound)
    }
}

impl DisplayApi for Host {
    fn display_get_info(
        &self,
        cx: &mut Cx<'_>,
        display: Handle,
        info: OutPtr<DisplayInfo>,
    ) -> AbiResult<Status> {
        let data = match self.display_data(display) {
            Ok(data) => data,
            Err(status) => return Ok(status),
        };
        let (work_x, work_y, work_width, work_height) = data.work_area;
        info.write(
            &cx.mem,
            &DisplayInfo {
                x: data.x,
                y: data.y,
                width: data.width,
                height: data.height,
                work_x,
                work_y,
                work_width,
                work_height,
                scale_factor: data.scale_factor,
                refresh_rate: data.refresh_rate,
                flags: if data.primary {
                    display_flags::PRIMARY
                } else {
                    0
                },
            },
        )?;
        Ok(Status::Success)
    }

    fn display_get_name(
        &self,
        cx: &mut Cx<'_>,
        display: Handle,
        buffer: u32,
        capacity: u32,
        length: OutPtr<u32>,
    ) -> AbiResult<Status> {
        let data = match self.display_data(display) {
            Ok(data) => data,
            Err(status) => return Ok(status),
        };
        cx.mem
            .write_truncated(buffer, capacity, length, data.name.as_bytes())?;
        Ok(Status::Success)
    }
}
