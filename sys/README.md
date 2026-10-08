# wasmer-gui-sys

The declarations of `<wasmer/gui.h>` for Rust: windows, the keyboard,
pointers, text input, the clipboard and frames for WASIX programs running on
the Wasmer runtime, natively or in a browser.

Every function is a WebAssembly import of the module `wasmer_gui_v0`: there
is nothing to link. A program that uses them runs where that module is
provided (`wasmer run --experimental-gui`, or a sandbox of the Wasmer SDK
made with `gui`).

Names, types and layout are the header's, and its documentation is on every
item. The crate is generated from the same description as the header
(`spec/gui.json` of [wasmer-gui](https://github.com/wasmerio/gui)), has no
dependencies and needs no `std`. Compiled for a 32-bit target, it asserts
every size and offset of the layout the host decodes.

Most programs want what is built on this instead: winit and softbuffer, or
SDL. Their backends for this API are in wasmer-gui's `integrations/`.
