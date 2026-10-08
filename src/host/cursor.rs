//! `WGUICursor`.

use super::{Host, Object, ObjectKind, lock};
use crate::abi::api::CursorApi;
use crate::abi::{Cx, Handle};
use crate::system::WindowChange;

impl CursorApi for Host {
    fn cursor_destroy(&self, _cx: &mut Cx<'_>, cursor: Handle) {
        let windows = {
            let mut table = lock(&self.table);
            if table.remove(cursor, ObjectKind::Cursor).is_none() {
                return;
            }
            table.all(ObjectKind::Window)
        };
        // Windows that were showing it go back to the default cursor.
        for object in windows {
            let Object::Window(window) = object else {
                continue;
            };
            let mut shown = lock(&window.cursor);
            if *shown == cursor {
                *shown = 0;
                window.apply(WindowChange::CustomCursor(None));
            }
        }
    }
}
