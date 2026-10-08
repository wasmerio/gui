# Third-party material

This repository is MIT licensed (see `LICENSE`). It includes, or is derived
from, the following work of others.

## Key tables

`spec/keys.json` is derived by `codegen/derive_keys.py` from
`ui/events/keycodes/dom/dom_code_data.inc` and `dom_key_data.inc` of
<https://chromium.googlesource.com/chromium/src>, at the revision the file
names. The names in it are the KeyboardEvent `code` and `key` values of the
W3C UI Events specification; the numbers of the physical keys are USB HID
usages.

Copyright 2013 The Chromium Authors. BSD-3-Clause.

## webgpu-headers

The schema of `spec/gui.json` and the conventions of the generated header
follow <https://github.com/webgpu-native/webgpu-headers>.

Copyright 2019-2023 WebGPU-Native developers. BSD-3-Clause.

## SDL and winit

`integrations/sdl3` and `integrations/winit` hold patches against
<https://github.com/libsdl-org/SDL> (zlib license),
<https://github.com/rust-windowing/winit> (Apache-2.0) and
<https://github.com/rust-windowing/softbuffer> (MIT OR Apache-2.0).
