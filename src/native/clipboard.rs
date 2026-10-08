//! The system clipboard, for native windows.
//!
//! The clipboard libraries of the platforms differ in everything but this:
//! text can be put there and taken from there, and neither says when someone
//! else changed it. So change is noticed by looking: the content is hashed
//! whenever the guest asks about it, and when one of its windows gains focus,
//! which is when a user who copied elsewhere comes back to paste.
//!
//! Reads and writes happen on the thread that asks. On X11 they can take a
//! while (the content is fetched from whichever program owns it), which is
//! why none of this runs on the window system's own thread.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Mutex;

use super::lock;
use crate::system::*;

const TEXT: &str = "text/plain";

struct State {
    /// `None` when the platform's clipboard cannot be reached.
    board: Option<arboard::Clipboard>,
    /// A fingerprint of what was last seen there.
    seen: Option<u64>,
    sequence: u32,
}

pub(super) struct Clipboard {
    state: Mutex<State>,
}

fn fingerprint(text: Option<&str>) -> Option<u64> {
    text.map(|text| {
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        hasher.finish()
    })
}

impl Clipboard {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(State {
                board: None,
                seen: None,
                sequence: 0,
            }),
        }
    }

    /// The text on the clipboard now, noting whether it changed.
    fn look(state: &mut State) -> Option<String> {
        if state.board.is_none() {
            // Opened on first use, and again after a failure: the connection
            // to a display server can come and go.
            state.board = arboard::Clipboard::new().ok();
        }
        let text = match state.board.as_mut()?.get_text() {
            Ok(text) => Some(text),
            Err(arboard::Error::ContentNotAvailable) => None,
            Err(_) => {
                state.board = None;
                None
            }
        };
        let now = fingerprint(text.as_deref());
        if now != state.seen {
            state.seen = now;
            state.sequence = state.sequence.wrapping_add(1);
        }
        text
    }

    pub(super) fn state(&self) -> ClipboardState {
        let mut state = lock(&self.state);
        let text = Self::look(&mut state);
        ClipboardState {
            sequence: state.sequence,
            flags: if text.is_some_and(|text| !text.is_empty()) {
                clipboard_flags::HAS_TEXT
            } else {
                0
            },
        }
    }

    pub(super) fn read(&self, mime_type: &str) -> Result<Vec<u8>, Status> {
        if mime_type != TEXT {
            return Err(Status::NotFound);
        }
        Self::look(&mut lock(&self.state))
            .map(String::into_bytes)
            .ok_or(Status::NotFound)
    }

    pub(super) fn write(&self, items: &[ClipboardData]) -> Status {
        let mut state = lock(&self.state);
        if state.board.is_none() {
            state.board = arboard::Clipboard::new().ok();
        }
        let Some(board) = state.board.as_mut() else {
            return Status::Unsupported;
        };
        let text = items
            .iter()
            .find(|item| item.mime_type == TEXT)
            .map(|item| String::from_utf8_lossy(&item.data).into_owned());
        let written = match &text {
            Some(text) => board.set_text(text.clone()),
            None => board.clear(),
        };
        if written.is_err() {
            state.board = None;
            return Status::Error;
        }
        // The one change the write itself makes.
        state.seen = fingerprint(text.as_deref());
        state.sequence = state.sequence.wrapping_add(1);
        Status::Success
    }
}
