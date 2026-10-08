//! The clipboard functions of `WGUIInstance`.
//!
//! The window system has the clipboard; the host adds two things. What an
//! instance wrote itself it can always read back at once, so a program that
//! copies and then pastes its own text never waits. And reading what others
//! wrote is subject to the embedder's policy: by default only right after
//! the user pressed the paste shortcut.

use std::sync::atomic::Ordering;

use super::request::PendingRequest;
use super::{Host, Instance, lock};
use crate::abi::enums::*;
use crate::abi::structs::{ClipboardInfo, ClipboardItem};
use crate::abi::{AbiResult, Cx, Handle, OutPtr};
use crate::ctx::ClipboardRead;
use crate::system::ClipboardData;

/// The MIME type of text. It is UTF-8.
pub(crate) const TEXT: &str = "text/plain";
/// How long after a paste shortcut a guest may read the clipboard.
const PASTE_WINDOW_NS: u64 = 2_000_000_000;
/// The most one write may put on the clipboard.
const MAX_CLIPBOARD_BYTES: u64 = 64 << 20;
/// The most representations one write may offer.
const MAX_CLIPBOARD_ITEMS: usize = 32;

/// What an instance last wrote to one clipboard.
pub(crate) struct Own {
    items: Vec<ClipboardData>,
    /// The window system's sequence number of the clipboard just before.
    sequence: u32,
}

/// What an instance wrote to the clipboards.
#[derive(Default)]
pub(crate) struct Written {
    clipboard: Option<Own>,
    primary: Option<Own>,
}

impl Written {
    fn slot(&mut self, selection: ClipboardSelection) -> &mut Option<Own> {
        match selection {
            ClipboardSelection::Clipboard => &mut self.clipboard,
            ClipboardSelection::Primary => &mut self.primary,
        }
    }

    /// Whether the clipboard, at this sequence number of the window system,
    /// still holds what the instance wrote: nothing has changed it since
    /// except, at most, that write landing.
    pub(crate) fn owns(&self, selection: ClipboardSelection, sequence: u32) -> bool {
        let own = match selection {
            ClipboardSelection::Clipboard => &self.clipboard,
            ClipboardSelection::Primary => &self.primary,
        };
        own.as_ref()
            .is_some_and(|own| sequence.wrapping_sub(own.sequence) <= 1)
    }
}

/// A MIME type as the host compares them: the bare type in lower case. Text
/// in any charset parameter is UTF-8 here, so parameters are dropped.
fn normalise(mime_type: &str) -> Option<String> {
    let bare = mime_type.split(';').next()?.trim().to_ascii_lowercase();
    let valid = bare.len() <= 255
        && bare.split('/').count() == 2
        && bare.split('/').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"!#$&-^_.+".contains(&b))
        });
    valid.then_some(bare)
}

impl Host {
    /// Whether `selection` exists here at all.
    fn selection_supported(&self, selection: ClipboardSelection) -> bool {
        match selection {
            ClipboardSelection::Clipboard => self.has(FeatureName::ClipboardText),
            ClipboardSelection::Primary => self.has(FeatureName::PrimarySelection),
        }
    }

    /// Whether the instance may read what others put on the clipboard now.
    fn may_read(&self, instance: &Instance) -> bool {
        match self.ctx.policy.clipboard_read {
            ClipboardRead::Deny => false,
            ClipboardRead::Allow => true,
            ClipboardRead::OnPaste => {
                let pasted = instance.shared.paste_stamp.load(Ordering::Acquire);
                pasted != u64::MAX
                    && self.ctx.system.now().saturating_sub(pasted) <= PASTE_WINDOW_NS
            }
        }
    }

    pub(super) fn clipboard_info(
        &self,
        cx: &mut Cx<'_>,
        instance: Handle,
        selection: ClipboardSelection,
        info: OutPtr<ClipboardInfo>,
    ) -> AbiResult<Status> {
        let Some(instance) = self.instance(instance) else {
            return Ok(Status::InvalidHandle);
        };
        if !self.selection_supported(selection) {
            return Ok(Status::Unsupported);
        }
        let state = self.ctx.system.clipboard_state(selection);
        let owned = lock(&instance.shared.written).owns(selection, state.sequence);
        let mut flags = state.flags & !clipboard_flags::OWNED;
        if owned {
            flags |= clipboard_flags::OWNED;
        } else if self.ctx.policy.clipboard_read == ClipboardRead::Deny {
            // What it may never read, it is not told about either.
            flags &= !clipboard_flags::HAS_TEXT;
        }
        info.write(
            &cx.mem,
            &ClipboardInfo {
                sequence: state.sequence,
                flags,
            },
        )?;
        Ok(Status::Success)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn clipboard_read(
        &self,
        cx: &mut Cx<'_>,
        instance: Handle,
        selection: ClipboardSelection,
        mime_type: &str,
        buffer: u32,
        capacity: u32,
        size: OutPtr<u32>,
    ) -> AbiResult<Status> {
        let Some(instance) = self.instance(instance) else {
            return Ok(Status::InvalidHandle);
        };
        let Some(mime_type) = normalise(mime_type) else {
            return Ok(Status::InvalidArgument);
        };
        if !self.selection_supported(selection) {
            return Ok(Status::Unsupported);
        }
        let sequence = self.ctx.system.clipboard_state(selection).sequence;
        let own = {
            let written = lock(&instance.shared.written);
            written
                .owns(selection, sequence)
                .then(|| match selection {
                    ClipboardSelection::Clipboard => written.clipboard.as_ref(),
                    ClipboardSelection::Primary => written.primary.as_ref(),
                })
                .flatten()
                .map(|own| {
                    own.items
                        .iter()
                        .find(|item| item.mime_type == mime_type)
                        .map(|item| item.data.clone())
                })
        };
        let data = match own {
            Some(Some(data)) => data,
            Some(None) => return Ok(Status::NotFound),
            None => {
                if !self.may_read(&instance) {
                    return Ok(Status::Denied);
                }
                match self.ctx.system.clipboard_read(selection, &mime_type) {
                    Ok(data) => data,
                    Err(status) => return Ok(status),
                }
            }
        };
        size.write(&cx.mem, &(data.len() as u32))?;
        if data.len() > capacity as usize {
            return Ok(Status::BufferTooSmall);
        }
        if !data.is_empty() {
            if buffer == 0 {
                return Ok(Status::InvalidArgument);
            }
            cx.mem.write_bytes(buffer, &data)?;
        }
        Ok(Status::Success)
    }

    pub(super) fn clipboard_request(
        &self,
        instance: Handle,
        selection: ClipboardSelection,
        mime_type: &str,
        tag: u64,
    ) -> Status {
        let Some(instance) = self.instance(instance) else {
            return Status::InvalidHandle;
        };
        let Some(mime_type) = normalise(mime_type) else {
            return Status::InvalidArgument;
        };
        if !self.selection_supported(selection) {
            return Status::Unsupported;
        }
        if !self.may_read(&instance) {
            return Status::Denied;
        }
        if !instance.shared.owe_completion() {
            return Status::LimitExceeded;
        }
        let request = PendingRequest::new(&instance.shared, 0, RequestKind::ClipboardRequest, tag);
        self.ctx
            .system
            .clipboard_request(selection, &mime_type, request);
        Status::Success
    }

    pub(super) fn clipboard_write(
        &self,
        cx: &mut Cx<'_>,
        instance: Handle,
        selection: ClipboardSelection,
        items: &[ClipboardItem],
        tag: u64,
    ) -> AbiResult<Status> {
        let Some(instance) = self.instance(instance) else {
            return Ok(Status::InvalidHandle);
        };
        if !self.selection_supported(selection) {
            return Ok(Status::Unsupported);
        }
        if !self.ctx.policy.clipboard_write {
            return Ok(Status::Denied);
        }
        if items.len() > MAX_CLIPBOARD_ITEMS {
            return Ok(Status::LimitExceeded);
        }
        let other_formats = self.has(FeatureName::ClipboardData);
        let mut data = Vec::with_capacity(items.len());
        let mut total = 0u64;
        for item in items {
            let Some(mime_type) = normalise(&item.mime_type) else {
                return Ok(Status::InvalidArgument);
            };
            if data
                .iter()
                .any(|other: &ClipboardData| other.mime_type == mime_type)
            {
                return Ok(Status::InvalidArgument);
            }
            if mime_type != TEXT && !other_formats {
                return Ok(Status::Unsupported);
            }
            total += u64::from(item.size);
            if total > MAX_CLIPBOARD_BYTES {
                return Ok(Status::LimitExceeded);
            }
            if item.size != 0 && item.data == 0 {
                return Ok(Status::InvalidArgument);
            }
            let bytes = cx.mem.read_vec(item.data, u64::from(item.size))?;
            if mime_type == TEXT && std::str::from_utf8(&bytes).is_err() {
                return Ok(Status::InvalidArgument);
            }
            data.push(ClipboardData {
                mime_type,
                data: bytes,
            });
        }
        if !instance.shared.owe_completion() {
            return Ok(Status::LimitExceeded);
        }
        let sequence = self.ctx.system.clipboard_state(selection).sequence;
        *lock(&instance.shared.written).slot(selection) = Some(Own {
            items: data.clone(),
            sequence,
        });
        let request = PendingRequest::new(&instance.shared, 0, RequestKind::ClipboardWrite, tag);
        self.ctx.system.clipboard_write(selection, data, request);
        Ok(Status::Success)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_types_are_compared_bare_and_in_lower_case() {
        assert_eq!(normalise("text/plain").as_deref(), Some("text/plain"));
        assert_eq!(
            normalise("Text/Plain; charset=utf-8").as_deref(),
            Some("text/plain")
        );
        assert_eq!(normalise("image/svg+xml").as_deref(), Some("image/svg+xml"));
        for bad in [
            "",
            "text",
            "text/",
            "/plain",
            "a/b/c",
            "te xt/plain",
            "text/pl\0ain",
        ] {
            assert_eq!(normalise(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn what_was_written_is_owned_until_someone_else_writes() {
        let mut written = Written::default();
        assert!(!written.owns(ClipboardSelection::Clipboard, 7));
        *written.slot(ClipboardSelection::Clipboard) = Some(Own {
            items: Vec::new(),
            sequence: 7,
        });
        // Before the write lands, and once it has.
        assert!(written.owns(ClipboardSelection::Clipboard, 7));
        assert!(written.owns(ClipboardSelection::Clipboard, 8));
        // Another change: someone else's.
        assert!(!written.owns(ClipboardSelection::Clipboard, 9));
        assert!(!written.owns(ClipboardSelection::Primary, 8));
    }
}
