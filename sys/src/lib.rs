//! The declarations of `<wasmer/gui.h>` for Rust: windows, the keyboard,
//! pointers, text input, the clipboard and frames for WASIX programs running
//! on the Wasmer runtime, natively or in a browser.
//!
//! Every function here is a WebAssembly import of the module
//! `wasmer_gui_v0`: there is no library to link. A program that calls them
//! runs where that module is provided (`wasmer run --experimental-gui`, a
//! sandbox of the Wasmer SDK made with `gui`), and a host that does not have
//! one of the functions answers `WGUIStatus_Unimplemented` for it, so a
//! program built against a newer version of this crate still starts on an
//! older host.
//!
//! This is the API as it is: names, types and layout are the header's, and
//! the header's documentation is on every item. Most programs want what is
//! built on it instead (winit, SDL). What is here is for the ones that
//! build such things.
//!
//! ```no_run
//! use wasmer_gui_sys::*;
//!
//! unsafe {
//!     let mut instance = 0;
//!     assert_eq!(wguiCreateInstance(core::ptr::null(), &mut instance), WGUIStatus_Success);
//!     let mut window = 0;
//!     let descriptor = WGUIWindowDescriptor {
//!         title: WGUIStringView::new("Hello"),
//!         ..WGUIWindowDescriptor::INIT
//!     };
//!     assert_eq!(wguiInstanceCreateWindow(instance, &descriptor, &mut window), WGUIStatus_Success);
//!
//!     // Records are read into memory the program supplies: 8-byte aligned,
//!     // and at least one record long.
//!     let mut buffer = [0u64; WGUI_MAX_EVENT_SIZE / 8];
//!     loop {
//!         let mut reasons = 0;
//!         wguiInstanceWaitEvents(instance, WGUI_WAIT_FOREVER, &mut reasons);
//!         let mut written = 0;
//!         wguiInstancePollEvents(instance, buffer.as_mut_ptr().cast(), size_of_val(&buffer), &mut written);
//!         let bytes = buffer.as_ptr().cast::<u8>();
//!         let mut event = wguiEventFirst(bytes, written);
//!         while !event.is_null() {
//!             if (*event).r#type == WGUIEventType_WindowCloseRequested {
//!                 wguiInstanceDestroy(instance);
//!                 return;
//!             }
//!             event = wguiEventNext(bytes, written, event);
//!         }
//!     }
//! }
//! ```

#![no_std]
#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]
// The documentation is the header's, which speaks of C: of NULL, and of
// functions by their C names.
#![allow(clippy::doc_lazy_continuation, clippy::missing_safety_doc)]

include!("bindings.gen.rs");
