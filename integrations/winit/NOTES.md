<!-- Notes from the build spike of 2026-10-08. Line references are to the tag named below. -->

> These are the notes of the spike that came before the backends: what a
> winit backend is made of, what each of its calls needs from a host, and
> the same for softbuffer. The backends have since been written (`backend/`,
> `softbuffer/`, see `README.md`). The skeletons the spike compiled
> (`winit-wasix-skeleton.patch`, `softbuffer-wasix-skeleton.patch`) and its
> three small programs (`wasix-test/`) are no longer in this directory:
> `winit-wasix.patch`, `softbuffer-wasix.patch`, `tests/` and winit's own
> example took their place. What the spike found and printed is kept below
> as it was; where it says "skeleton", "stub" or "TODO", that was then.

Date: 2026-10-08. Sources read or run:

| What | Version | Commit |
|---|---|---|
| winit (Parts 1, 2) | tag `v0.30.13` | `e9809ef54b18499bb4f2cac945719ecc2a61061b` (2026-03-01) |
| winit 0.31 (read only) | `master` | `01634067402db15d26a31bfd199f5c69a989813d` (2026-10-05) |
| winit 0.31 (read only) | tag `v0.31.0-beta.3` (newest 0.31 tag) | `7d20408d33210c93bf036335b88ee083ca563a90` (2026-09-03) |
| softbuffer (Part 3) | tag `v0.4.8` (newest 0.4.x tag) | `d871852faa1137d4615b99b9ad31c3ba80f345b5` (2025-12-13) |
| softbuffer `master` (API direction only) | `Cargo.toml` still says 0.4.8 | `9a7c9caf4ac29a8ae361d68c9a5babb97e36dc70` (2026-07-18) |
| raw-window-handle | 0.6.2 (newest tag) | `~/.cargo/registry/src/index.crates.io-*/raw-window-handle-0.6.2` |
| W3C `code` values | w3c/uievents-code | `b201684d1de0af90bc403814bbdee6aa96647130` (2023-08-18) |

Toolchain: `cargo-wasix 0.1.34`, `rustc +wasix` = `rustc 1.96.1-dev` (toolchain `aarch64-apple-darwin_v2026-07-07.3+rust-1.96`), `wasmer 7.4.2`, macOS arm64.

Line numbers refer to the **pristine tags**, not the patched trees, unless the path is a new file from the patches (`src/platform_impl/wasix/*`, `src/backends/wasix.rs`).

## 1. Result

| Item | Status |
|---|---|
| winit 0.30.13 + wasix skeleton, `cargo wasix build --lib` | compiles (default features, `--no-default-features`, `--features rwh_04,rwh_05,rwh_06,serde,mint`) |
| softbuffer 0.4.8 + wasix skeleton, `cargo wasix build --lib` | compiles (default and `--no-default-features`) |
| `wasix-test/` (3 binaries using both patched crates) | builds; all three run to completion under `wasmer 7.4.2`, exit 0 |
| Patches apply to pristine tags | verified by running `build.sh` from an empty directory |
| Web / macOS / Linux builds after the patches | `cargo check --lib` passes on host and `wasm32-unknown-unknown` (both crates) and `x86_64-unknown-linux-gnu` (softbuffer); no new warnings |
| Default `cargo wasix build` (overlay registry) | fails at dependency resolution for both crates (2.2); all builds use `CARGO_WASIX_NO_REGISTRY_CONFIG=1` |

Program output (`./build.sh`):

```text
== wasmer run event_loop.wasm
target: arch=wasm32 os=wasi vendor=wasmer family=wasm
EventLoop::new() -> Ok
second EventLoop::new() -> Err(RecreationAttempt)
new_events(Init)
resumed: control_flow=Wait
resumed: create_window -> Err(os error at <spike>/winit/src/platform_impl/wasix/window.rs:36: creating windows is not supported on WASIX yet)
resumed: monitors=0 primary_monitor=None system_theme=None
about_to_wait #1: calling event_loop.exit()
about_to_wait: exiting()=true
exiting
run_app -> Ok(())
OK
== wasmer run proxy_wait.wasm
new_events(Init) -> Poll
resumed
new_events(Poll) #1
new_events(Poll) #2
new_events(Poll) #3
-> WaitUntil(now + 50ms)
new_events(ResumeTimeReached): waited>=50ms:true not_early:true
-> Wait, and a thread sends a user event in 30ms
new_events(WaitCancelled): waited>=30ms:true requested_resume=None
user_event(FromThread(42)) -> exit()
exiting
run_app -> Ok(())
send_event after the loop is gone -> Err(EventLoopClosed(FromThread(0)))
OK
== wasmer run softbuffer_surface.wasm
Context::new(web display) -> Ok
Surface::new(Drm window)   -> Err(The provided window returned an unsupported platform: DRM, Wasix. | Window handle: Drm(DrmWindowHandle { plane: 3 }))
Surface::new(Web id=7)     -> Ok
resize(4, 3)               -> Ok
buffer_mut: 4x3 len=12 age=0
pixel[5] = 0x005078ff
pixel[5] bytes in linear memory = [ff, 78, 50, 00] (B, G, R, X)
present()                  -> Err(This function is unimplemented on this platform.)
present_with_damage(ok)    -> Err(This function is unimplemented on this platform.)
present_with_damage(oob)   -> Err(Damage rect 2x1 at (3, 0) out of range for backend.)
fetch()                    -> Err(This function is unimplemented on this platform.)
OK
```

The wasm files import only `wasi_snapshot_preview1`, `wasix_32v1`, `wasi` (`thread-spawn`) and `env.memory` (checked with `wasmer inspect`); there are no `__wbindgen*` imports.

## 2. Toolchain facts

### 2.1 Target cfg (verified: `rustc +wasix --print cfg --target wasm32-wasmer-wasi`)

`target_os="wasi"`, `target_vendor="wasmer"`, `target_family="wasm"`, `target_arch="wasm32"`, `target_env=""`, `target_pointer_width="32"`, `panic="abort"`. `target_feature` is `atomics`, `bulk-memory`, `crt-static`, `multivalue`, `mutable-globals`, `nontrapping-fptoint`, `reference-types`, `sign-ext`. `unix` is not set.

- `all(target_os = "wasi", target_vendor = "wasmer")` selects WASIX and nothing else. Upstream `wasm32-wasip1` has vendor `unknown`, so there is no way to tell them apart without `target_vendor`.
- Every `cfg(target_family = "wasm")` that means "the Web" matches WASIX too, and so does `cfg(all(target_family = "wasm", target_feature = "atomics"))`.
- `target_vendor = "wasmer"` is unknown to upstream rustc's check-cfg. Writing it in a `#[cfg]` makes every non-WASIX build warn `unexpected cfg condition value: wasmer` (seen on softbuffer; winit avoids it because its cfgs go through `cfg_aliases` in `build.rs`).
- Threads, `Mutex`, `Condvar::wait_timeout`, `mpsc` and `std::time::Instant` work under wasmer (`proxy_wait`).
- Linear memory is imported and shared: `"env"."memory": shared (17 pages..65536 pages)`.
- `std::env::consts::OS` and `FAMILY` are both `""` on this toolchain.

### 2.2 cargo-wasix and the WASIX overlay registry

`cargo wasix build` (0.1.34) writes (creates, or appends to) the workspace's `.cargo/config.toml`. In both checkouts that is a tracked file.

```toml
[source.crates-io]
replace-with = "wasix"

[source.wasix]
registry = "sparse+https://cargo-registry.wasix.org/"
```

The registry serves only the forked versions of the crates it overrides and redirects (HTTP 307 to `index.crates.io`) for everything else. Queried 2026-10-08:

| Crate | Versions served |
|---|---|
| `rustix` | `0.33.7+wasix.1`, `0.34.8+wasix.1`, `1.0.8+wasix.1`, `1.1.5+wasix.1` |
| `libc` | 11 versions, newest `0.2.186+wasix.1` |
| `getrandom` | `0.2.7`, `0.2.15`, `0.3.3`, `0.4.3` (`+wasix.1`) |
| `tokio` | `1.24.1`, `1.24.2`, `1.35.1`, `1.47.0` (`+wasix.1`) |
| `mio`, `socket2`, `cc` | forked versions only |
| `winit`, `softbuffer`, `raw-window-handle`, `bitflags`, `smol_str`, `cursor-icon`, `tracing`, `dpi`, `cfg_aliases`, `web-time`, `memmap2` | not overridden |

Cargo resolves the lockfile for all targets, so dependencies never compiled for WASIX still have to resolve:

```text
$ cargo wasix build --lib            # winit v0.30.13, unmodified
error: failed to select a version for the requirement `rustix = "^0.38.4"`
candidate versions found which didn't match: 1.1.5+wasix.1, 1.0.8+wasix.1, 0.34.8+wasix.1, ...
location searched: `wasix` index (which is replacing registry `crates-io`)
required by package `winit v0.30.13 (...)`

$ cargo wasix build --lib            # softbuffer v0.4.8, unmodified
error: failed to select a version for the requirement `rustix = "^0.38.22"`
required by package `drm v0.14.1`
  ... which satisfies dependency `drm = "^0.14.1"` of package `softbuffer v0.4.8 (...)`
```

Who needs `rustix 0.38`:

- **winit 0.30.13:** `winit` itself (`Cargo.toml:261`), `calloop 0.13.0`, `calloop-wayland-source 0.3.0`, `smithay-client-toolkit 0.19.2`. All are Linux-only.
- **softbuffer 0.4.8:** `drm 0.14.1`, and the `winit = "0.30.0"` dev-dependency (`Cargo.toml:139`).

Probes, run in throw-away copies:

| Probe | Result |
|---|---|
| patched winit, overlay registry | fails (`rustix ^0.38.4`) |
| + winit's own `rustix` bumped to `1` | fails on `calloop v0.13.0` (`rustix ^0.38`) |
| + whole free-unix dependency table removed | resolves and builds through the overlay registry |
| a consumer crate with `winit = { path = ... }` | fails the same way |
| `CARGO_WASIX_NO_REGISTRY_CONFIG=1` (crates.io) | resolves and builds (used by `build.sh`) |
| winit `master` (0.31), overlay registry, `cargo wasix build -p winit-core` | resolves (`rustix 1.1.5+wasix.1`, `libc 0.2.186+wasix.1`) and builds unmodified |

## 3. Part 1: winit 0.30, what a platform backend is

### 3.1 Where the backend is selected

`build.rs:8-23` (`cfg_aliases!`):

| Alias | Definition |
|---|---|
| `android_platform` | `target_os = "android"` |
| `web_platform` | `all(target_family = "wasm", target_os = "unknown")` |
| `macos_platform` | `target_os = "macos"` |
| `ios_platform` | `target_os = "ios"` |
| `windows_platform` | `target_os = "windows"` |
| `apple` | `any(target_os = "ios", target_os = "macos")` |
| `free_unix` | `all(unix, not(apple), not(android_platform), not(target_os = "emscripten"))` |
| `redox` | `target_os = "redox"` |
| `x11_platform` | `all(feature = "x11", free_unix, not(redox))` |
| `wayland_platform` | `all(feature = "wayland", free_unix, not(redox))` |
| `orbital_platform` | `redox` |

`src/platform_impl/mod.rs` is laid out as follows:

- `#[cfg(X)] mod X;` at lines 4-17.
- `#[cfg(X)] use X as platform;` at lines 19-32.
- `pub use self::platform::*;` at line 34.
- The shared `Fullscreen` helper enum and its conversions at lines 36-66.
- For an unknown target, at lines 68-78:

```rust
#[cfg(all(not(ios_platform), not(windows_platform), not(macos_platform), not(android_platform),
        not(x11_platform), not(wayland_platform), not(web_platform), not(orbital_platform)))]
compile_error!("The platform you're compiling for is not supported by winit");
```

Verified with crates.io resolution: unmodified winit on `wasm32-wasmer-wasi` stops with exactly this error (`src/platform_impl/mod.rs:78:1`). Before that it compiles `js-sys`, `wasm-bindgen`, `wasm-bindgen-futures`, `web-sys`, `web-time`, `pin-project`, `atomic-waker` and `concurrent-queue` for WASIX. They are pulled in by `Cargo.toml:296` (`cfg(target_family = "wasm")`) and `Cargo.toml:354` (`cfg(all(target_family = "wasm", target_feature = "atomics"))`).

`web_platform` is false on WASIX, so `std::time::{Duration, Instant}` are used (`src/event_loop.rs:16-19`, `src/event.rs:39-46`).

### 3.2 Everything a platform module must export

Every `platform_impl::` reference in `src/*.rs`:

| File:line | Reference |
|---|---|
| `src/event_loop.rs:44` | `platform_impl::EventLoop<T>` |
| `src/event_loop.rs:53` | `platform_impl::ActiveEventLoop` |
| `src/event_loop.rs:65` | `platform_impl::PlatformSpecificEventLoopAttributes` |
| `src/event_loop.rs:125` | `platform_impl::EventLoop::new(&mut self.platform_specific)?` |
| `src/event_loop.rs:315`, `:385` | `platform_impl::Window::new(&..., window_attributes)?` |
| `src/event_loop.rs:517` | `platform_impl::OwnedDisplayHandle` |
| `src/event_loop.rs:550` | `platform_impl::EventLoopProxy<T>` |
| `src/window.rs:7`, `:129` | `PlatformSpecificWindowAttributes` |
| `src/window.rs:36` | `platform_impl::Window` |
| `src/window.rs:69`, `:80` | `platform_impl::WindowId`, `WindowId::dummy()` |
| `src/event.rs:447`, `:458` | `platform_impl::DeviceId`, `DeviceId::dummy()` |
| `src/event.rs:655` | `platform_impl::KeyEventExtra` |
| `src/monitor.rs:20`, `:104` | `platform_impl::VideoModeHandle`, `platform_impl::MonitorHandle` |
| `src/cursor.rs:8` | `PlatformCustomCursor`, `PlatformCustomCursorSource` |
| `src/icon.rs:1` | `PlatformIcon` |
| `src/error.rs:28`, `:60` | `platform_impl::OsError` |
| `src/platform_impl/mod.rs:40-41` | `VideoModeHandle`, `MonitorHandle` (inside `Fullscreen`) |

There is no `FingerId` in 0.30 (`Touch.id` is a plain `u64`, `src/event.rs:856`); it appears in 0.31.

#### 3.2.1 Types and required traits

| Export | Required (why) | Skeleton |
|---|---|---|
| `EventLoop<T: 'static>` | methods below | `mpsc` channel + `Waker` |
| `EventLoopProxy<T: 'static>` | `.clone()` for every `T` (`src/event_loop.rs:553-557`); by contract `Send + Sync` when `T: Send` (`tests/send_objects.rs:5-11`, `tests/sync_object.rs:5-11`) | `mpsc::Sender<T>` + `Arc<Waker>` |
| `ActiveEventLoop` | none; the backend itself builds the wrapper `event_loop::ActiveEventLoop { p, _marker: PhantomData<*mut ()> }` (`pub(crate)` fields, `src/event_loop.rs:52-55`) | `Cell<ControlFlow>`, `Cell<bool>`, `Arc<Waker>` |
| `OwnedDisplayHandle` | `Clone` (`src/event_loop.rs:514`) | ZST |
| `PlatformSpecificEventLoopAttributes` | `Default` (`src/event_loop.rs:63-67`, `:216`) | empty struct |
| `PlatformSpecificWindowAttributes` | `Debug + Clone + Default` (`src/window.rs:103-104`, `:158`); `Send + Sync` by contract | unit struct |
| `Window` | `Send + Sync` by contract (`tests/send_objects.rs:13-17`, `tests/sync_object.rs:13-17`) | `{ id: WindowId }` |
| `WindowId` | `Copy + Clone + PartialEq + Eq + PartialOrd + Ord + Hash` (`src/window.rs:68`), `Debug` (`:84-88`), `Into<u64>` (`:90-94`), `From<u64>` (`:96-100`), `const fn dummy() -> Self` (`:79-81`) | `WindowId(u32)` |
| `DeviceId` | `Debug + Copy + Clone + PartialEq + Eq + PartialOrd + Ord + Hash` (`src/event.rs:446`), `const fn dummy() -> Self` (`:457-459`) | `DeviceId(u32)` |
| `KeyEventExtra` | `Debug + Clone + Eq + PartialEq + Hash` (`src/event.rs:522`) | two fields (7.1) |
| `MonitorHandle` | `Debug + Clone + PartialEq + Eq + PartialOrd + Ord` (`src/monitor.rs:102`); `Hash` if `VideoModeHandle` contains it | `MonitorHandle(u32)`, never handed out |
| `VideoModeHandle` | `Clone + PartialEq + Eq + Hash` (`src/monitor.rs:18`), `Debug` (`:23-27`) | never constructed |
| `PlatformCustomCursor` | `Clone + Debug + Eq + Hash + PartialEq` (`src/cursor.rs:74`); cheap to clone (`:76`); `Send + Sync` (tests) | `crate::cursor::NoCustomCursor` |
| `PlatformCustomCursorSource` | `Debug` (`src/cursor.rs:110`); `fn from_rgba(rgba: Vec<u8>, width: u16, height: u16, hotspot_x: u16, hotspot_y: u16) -> Result<Self, BadImage>` (`:96-102`) | `crate::cursor::NoCustomCursor` |
| `PlatformIcon` | `Clone` (`src/icon.rs:96`), `Debug` (`:101-105`); `fn from_rgba(rgba: Vec<u8>, width: u32, height: u32) -> Result<Self, BadIcon>` (`:115`) | `crate::icon::NoIcon` |
| `OsError` | `Debug` (`src/error.rs:24`), `Display` (`:74`) | enum `NotSupported(&'static str)` / `Host(i32)` |

Ready-made helpers a backend can re-export:

- `crate::cursor::NoCustomCursor` (`src/cursor.rs:249-263`).
- `OnlyCursorImage` / `OnlyCursorImageSource` (`:168-200`).
- `crate::icon::NoIcon` (`src/icon.rs:61`, `:86-92`).
- `RgbaIcon` (`:53-84`).

#### 3.2.2 Event loop types (signatures as called by common code; the skeleton compiles with these)

```rust
impl<T: 'static> EventLoop<T> {
  // src/event_loop.rs:125 passes `&mut PlatformSpecificEventLoopAttributes`; `&` works too.
  pub(crate) fn new(attrs: &PlatformSpecificEventLoopAttributes) -> Result<Self, EventLoopError>;
  // src/event_loop.rs:231 (`run`) and :265 (`run_app`, via `dispatch_event_for_app`, :635-651)
  pub fn run<F>(self, event_handler: F) -> Result<(), EventLoopError>
  where F: FnMut(crate::event::Event<T>, &crate::event_loop::ActiveEventLoop);
  pub fn window_target(&self) -> &crate::event_loop::ActiveEventLoop;   // :278, :293, :298, :315, :321, :328, :336
  pub fn create_proxy(&self) -> EventLoopProxy<T>;                      // :271
}

impl<T: 'static> EventLoopProxy<T> {
  pub fn send_event(&self, event: T) -> Result<(), crate::event_loop::EventLoopClosed<T>>;  // :570
}
impl<T: 'static> Clone for EventLoopProxy<T> { /* :555 */ }

impl ActiveEventLoop {
  pub fn create_custom_cursor(&self, source: CustomCursorSource) -> crate::window::CustomCursor; // :321, :393
  pub fn available_monitors(&self) -> impl IntoIterator<Item = MonitorHandle>;   // :402 (`VecDeque` in small backends)
  pub fn primary_monitor(&self) -> Option<MonitorHandle>;                        // :416
  pub fn listen_device_events(&self, allowed: DeviceEvents);                     // :293, :437
  pub fn system_theme(&self) -> Option<Theme>;                                   // :448
  pub fn set_control_flow(&self, control_flow: ControlFlow);                     // :298, :453
  pub fn control_flow(&self) -> ControlFlow;                                     // :458
  pub(crate) fn exit(&self);                                                     // :467
  pub(crate) fn exiting(&self) -> bool;                                          // :474
  pub(crate) fn owned_display_handle(&self) -> OwnedDisplayHandle;               // :278, :481
  #[cfg(feature = "rwh_06")]
  pub fn raw_display_handle_rwh_06(&self) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError>; // :488
  #[cfg(feature = "rwh_05")]
  pub fn raw_display_handle_rwh_05(&self) -> rwh_05::RawDisplayHandle;           // :498
}

impl OwnedDisplayHandle {
  #[cfg(feature = "rwh_06")]
  pub fn raw_display_handle_rwh_06(&self) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError>; // :531
  #[cfg(feature = "rwh_05")]
  pub fn raw_display_handle_rwh_05(&self) -> rwh_05::RawDisplayHandle;           // :544
}
```

#### 3.2.3 `Window`

```rust
impl Window {
  // src/event_loop.rs:315, :385. Error type is `crate::error::OsError`, built with `os_error!`
  // (src/error.rs:66-70) from the platform `OsError`.
  pub(crate) fn new(event_loop: &ActiveEventLoop, attrs: WindowAttributes) -> Result<Self, crate::error::OsError>;
  // Every public `Window` method goes through one of these (src/window.rs:498-1620).
  pub(crate) fn maybe_queue_on_main(&self, f: impl FnOnce(&Self) + Send + 'static);
  pub(crate) fn maybe_wait_on_main<R: Send>(&self, f: impl FnOnce(&Self) -> R + Send) -> R;
}
```

On top of that come 55 inherent methods with exactly the signatures in `src/platform_impl/wasix/window.rs`. The full list, with the public wrapper's line and what may be refused, is in 7.4.

Easy to miss:

- `set_fullscreen` / `fullscreen` use the platform-side `crate::platform_impl::Fullscreen` (`src/platform_impl/mod.rs:38-66`), not `crate::window::Fullscreen` (`src/window.rs:1133`, `:1147`).
- `fullscreen()` and `set_fullscreen(None)` are called from `impl Drop for window::Window` (`src/window.rs:45-60`), so they must not panic.
- `available_monitors()` only needs `IntoIterator<Item = MonitorHandle>` (`src/window.rs:1602`).

raw-window-handle methods per feature:

- `raw_window_handle_rwh_06(&self) -> Result<rwh_06::RawWindowHandle, rwh_06::HandleError>` (`src/window.rs:1627`)
- `raw_display_handle_rwh_06(&self) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError>` (`:1638`)
- `raw_window_handle_rwh_05(&self) -> rwh_05::RawWindowHandle` (`:1660`)
- `raw_display_handle_rwh_05(&self) -> rwh_05::RawDisplayHandle` (`:1671`)
- `raw_window_handle_rwh_04(&self) -> rwh_04::RawWindowHandle` (`:1678`)

#### 3.2.4 `MonitorHandle`, `VideoModeHandle`

```rust
impl MonitorHandle {                                       // src/monitor.rs:107-167
  pub fn name(&self) -> Option<String>;
  pub fn size(&self) -> PhysicalSize<u32>;
  pub fn position(&self) -> PhysicalPosition<i32>;
  pub fn refresh_rate_millihertz(&self) -> Option<u32>;
  pub fn scale_factor(&self) -> f64;
  pub fn video_modes(&self) -> impl Iterator<Item = VideoModeHandle>;
}
impl VideoModeHandle {                                     // src/monitor.rs:50-82
  pub fn size(&self) -> PhysicalSize<u32>;
  pub fn bit_depth(&self) -> u16;
  pub fn refresh_rate_millihertz(&self) -> u32;
  pub fn monitor(&self) -> MonitorHandle;
}
```

#### 3.2.5 Opt-in extras (not needed to compile)

Each is enabled by adding the platform alias to a cfg list in `src/platform/mod.rs`:

| Module (cfg list) | Platforms in 0.30.13 | Backend must then provide |
|---|---|---|
| `modifier_supplement` (`:44-52`) | Windows, macOS, X11, Wayland, Orbital | `KeyEventExtra { text_with_all_modifiers: Option<SmolStr>, key_without_modifiers: Key }` (read at `src/platform/modifier_supplement.rs:28`, `:33`). Enabled in the skeleton. |
| `scancode` (`:54-55`) | Windows, macOS, X11, Wayland | `physicalkey_to_scancode(PhysicalKey) -> Option<u32>`, `scancode_to_physicalkey(u32) -> PhysicalKey` (`src/platform/scancode.rs:32`, `:36`) |
| `pump_events` (`:34-42`) | Windows, macOS, Android, X11, Wayland | `EventLoop::pump_events<F>(&mut self, timeout: Option<Duration>, event_handler: F) -> PumpStatus` (`src/platform/pump_events.rs:133`) |
| `run_on_demand` (`:24-32`) | same five | `EventLoop::run_on_demand<F>(&mut self, event_handler: F) -> Result<(), EventLoopError>` and `ActiveEventLoop::clear_exit(&self)` (`src/platform/run_on_demand.rs:88-89`, `:96`) |
| `platform::<name>` | per platform | extension traits; none are needed (`src/platform/orbital.rs` is 6 lines with no traits) |

#### 3.2.6 Root types the backend constructs

- `event_loop::ActiveEventLoop { p, _marker }`
- `window::WindowId(id)` and `event::DeviceId(id)`
- `window::CustomCursor { inner }`
- `event::KeyEvent { .., platform_specific }`
- `event::Modifiers { state, pressed_mods }`, or `Modifiers::from(ModifiersState)` (`src/event.rs:724-728`)
- `event::InnerSizeWriter::new(Weak<Mutex<PhysicalSize<u32>>>)` for `ScaleFactorChanged` (`src/event.rs:985-988`, `#[cfg(not(orbital_platform))]`)
- `monitor::MonitorHandle { inner }` and `monitor::VideoModeHandle { video_mode }`

### 3.3 Orbital as the template

| Backend | Lines | Files |
|---|---|---|
| **orbital** | **1,593** (`mod.rs` 255, `window.rs` 509, `event_loop.rs` 829) | 3 |
| android | 1,749 | 2 |
| ios | 3,308 | 8 |
| macos | 6,494 | 15 |
| web | 6,556 | 33 |
| windows | 9,625 | 17 |
| linux | 17,861 | 57 |

Orbital is the right template. It has:

- One blocking loop (`orbital/event_loop.rs:500-701`).
- A channel for user events and a wake socket written from other threads.
- Queues for window creation, redraw and destroy.
- `maybe_*_on_main` that just call the closure (`orbital/window.rs:129-135`).
- `NoCustomCursor` / `NoIcon`, and one dummy monitor.

About 100 lines of its `event_loop.rs` are the scancode table (`:31-132`).

## 4. The WASIX skeleton backend

`winit-wasix-skeleton.patch`: 8 files, +804 / -3.

| File | Change |
|---|---|
| `build.rs` | new alias `wasix_platform: { all(target_os = "wasi", target_vendor = "wasmer") }` |
| `Cargo.toml` | the three `cfg(target_family = "wasm")` tables (lines 296, 354, 358) now also require `target_os = "unknown"` |
| `src/platform_impl/mod.rs` | `mod wasix;`, `use wasix as platform;`, `not(wasix_platform)` in the `compile_error!` cfg |
| `src/platform/mod.rs` | `wasix_platform` added to the `modifier_supplement` cfg list |
| `src/event.rs` | `#[cfg_attr(wasix_platform, allow(dead_code))]` on `InnerSizeWriter::new` (unused until `ScaleFactorChanged` is emitted) |
| `src/platform_impl/wasix/mod.rs` (179 lines) | ids, attributes, `OsError`, `KeyEventExtra`, `MonitorHandle`, `VideoModeHandle`, cursor/icon re-exports |
| `src/platform_impl/wasix/event_loop.rs` (273 lines) | `EventLoop`, `EventLoopProxy`, `ActiveEventLoop`, `OwnedDisplayHandle`, `Waker` |
| `src/platform_impl/wasix/window.rs` (336 lines) | `Window`: `new` always fails; everything else stubbed |

Stub convention:

- `todo!()` means a real backend must implement it. There are four: `scale_factor`, `request_redraw`, `inner_size`, `has_focus`.
- `Err(NotSupported)`, `None`, an empty collection or a no-op means it may stay refused.
- No `Window` can exist yet, so no `todo!()` is reachable.

The event loop is already real. `EventLoop::run` follows the order documented at `src/event.rs:10-30`:

1. `NewEvents(cause)`, then `Resumed` once after `Init`.
2. User events.
3. `AboutToWait`, then the exit check.
4. `Poll` / `Wait` / `WaitUntil`, on a `Mutex<bool>` + `Condvar` `Waker` that `EventLoopProxy::send_event` signals.
5. `LoopExiting` at the end.

Three `TODO(wasmer_gui_v0)` markers show where host events, redraws and the host connection go.

`ActiveEventLoop::create_window` can only fail with `winit::error::OsError`; there is no `NotSupportedError` path in 0.30 (`src/event_loop.rs:378`). The skeleton returns `os_error!(OsError::NotSupported("creating windows"))`.

For raw-window-handle the skeleton answers `RawDisplayHandle::Web(WebDisplayHandle::new())`, and would answer `RawWindowHandle::Web(WebWindowHandle::new(id))` (reasons in 7.7).

## 5. Dependencies on WASIX

Resolved from crates.io; `cargo +wasix tree --target wasm32-wasmer-wasi -e normal,build`.

| Crate | Version | On WASIX | Attention |
|---|---|---|---|
| `bitflags` | 2.13.2 | compiles | none |
| `cursor-icon` | 1.2.0 (`alloc`, `std`) | compiles | none |
| `dpi` | 0.1.1 (in-repo path crate) | compiles | none |
| `smol_str` | 0.2.2 | compiles | none |
| `tracing` (`default-features = false`) + `tracing-core` 0.1.36 + `pin-project-lite` 0.2.17 | 0.1.44 | compiles | none |
| `raw-window-handle` as `rwh_06` | 0.6.2 (`alloc`, `std`) | compiles | keep its `wasm-bindgen-0-2` feature off |
| `raw-window-handle` as `rwh_05` | 0.5.2 | compiles | none; never executed |
| `raw-window-handle` as `rwh_04` | 0.4.3 (+ `cty` 0.2.2) | compiles | none; never executed |
| `serde` 1.0.229, `mint` 0.5.9 (optional) | | compile | none |
| `cfg_aliases` (build script) | 0.2.2 | n/a | none |
| `js-sys`, `wasm-bindgen`, `wasm-bindgen-futures`, `web-sys`, `web-time`, `pin-project` | | compile on WASIX and were pulled in by `cfg(target_family = "wasm")` (`Cargo.toml:296`) | gated to `target_os = "unknown"` in the patch |
| `atomic-waker`, `concurrent-queue` | | pulled in because WASIX has `+atomics` (`Cargo.toml:354`) | gated in the patch |
| `libc`, `rustix`, `calloop`, `x11-dl`, `x11rb`, `xkbcommon-dl`, `wayland-*`, `sctk`, `sctk-adwaita`, `memmap2`, `ahash`, `bytemuck`, `percent-encoding` | | not compiled (`unix` is unset; `Cargo.toml:254` also excludes `target_family = "wasm"`) | none for the build, but they take part in lockfile resolution and break the overlay registry (2.2) |
| dev-dependencies (`image`, `softbuffer 0.4`, `tracing-subscriber`, `winit` with `rwh_05`) | | not built for `--lib` | winit's examples would need a patched softbuffer (not built) |
| default features `x11`, `wayland`, `wayland-dlopen`, `wayland-csd-adwaita` | | harmless; they only enable optional deps of the free-unix table | none |

After the patch the WASIX build links:

- **winit:** `bitflags`, `cursor-icon`, `dpi`, `raw-window-handle`, `smol_str`, `tracing` (with `tracing-core` and `pin-project-lite`).
- **softbuffer:** `raw-window-handle`, `tracing`.

## 6. winit 0.31 (read only)

Read at `master` `0163406`, compared with tag `v0.31.0-beta.3` `7d20408`. `git diff --stat` between them shows 50 files, +2410 / -1854, mostly Wayland refactoring and error types.

Files read:

- `Cargo.toml`
- `winit/{Cargo.toml,build.rs,src/lib.rs,src/event_loop.rs,src/platform_impl/mod.rs,src/platform/mod.rs}`
- `winit-core/{Cargo.toml,src/lib.rs,src/event_loop/mod.rs,src/casting.rs,src/monitor.rs,src/cursor.rs,src/icon.rs}`
- Outlines of `winit-core/src/{window.rs,event.rs,keyboard.rs,error.rs,application/mod.rs}`
- `winit-orbital/{Cargo.toml,src/lib.rs}` and outlines of its `event_loop.rs` / `window.rs`

Two quick builds were done although not required; both are described below.

**Crate split.** The workspace (`edition = "2024"`, `rust-version = "1.86"`) has `winit`, `winit-core`, `winit-common` and one crate per backend: `winit-android`, `winit-appkit`, `winit-orbital`, `winit-uikit`, `winit-wayland`, `winit-web`, `winit-win32`, `winit-x11`.

- `winit-core` (7,674 lines) holds all shared types and traits.
- The top-level `winit` crate is about 1,265 lines of glue.
- `winit-orbital` is 1,556 lines and depends only on `winit-core`, `dpi`, `raw-window-handle`, `bitflags`, `smol_str`, `tracing` and its OS crates.

**Can a backend live out of tree?** Yes, for the backend itself. Everything it implements is an object-safe trait in `winit-core`, and every value type has a public constructor:

| `winit-core` item (master) | Shape |
|---|---|
| `event_loop::ActiveEventLoop` (`event_loop/mod.rs:114`) | `trait ActiveEventLoop: Any + fmt::Debug`. Methods: `create_window(&self, WindowAttributes) -> Result<Box<dyn Window>, CreateWindowError>`, `create_proxy`, `create_custom_cursor`, `available_monitors() -> Box<dyn Iterator<Item = MonitorHandle>>`, `primary_monitor`, `listen_device_events`, `system_theme`, `set_control_flow`, `control_flow`, `exit`, `exiting`, `owned_display_handle`, `rwh_06_handle(&self) -> &dyn HasDisplayHandle`. Data-transfer and drag-and-drop methods default to `NotSupported`. |
| `event_loop::EventLoopProvider` (`:26`) | `run_app<A: ApplicationHandler + 'static>(self, app: A) -> Result<(), EventLoopError>`, `create_proxy`, `owned_display_handle`, `listen_device_events`, `set_control_flow`, `create_custom_cursor` |
| `window::Window` (`window.rs:636`) | `trait Window: Any + Send + Sync + fmt::Debug`, about 75 methods. New ones include `surface_size` / `request_surface_size` (renamed from `inner_size`), `surface_position`, `safe_area`, `window_type`, `request_ime_update(ImeRequest) -> Result<(), ImeRequestError>`, `ime_capabilities`, `rwh_06_window_handle(&self) -> &dyn HasWindowHandle`, `rwh_06_display_handle`. |
| `event_loop::EventLoopProxy` (`:413`) | `EventLoopProxy::new(Arc<dyn EventLoopProxyProvider>)`; `EventLoopProxyProvider: Send + Sync + Debug { fn wake_up(&self); }` |
| `event_loop::OwnedDisplayHandle` (`:461`) | `OwnedDisplayHandle::new(Arc<dyn HasDisplayHandle + Send + Sync>)` |
| `monitor::MonitorHandle` (`monitor.rs:37`) | `MonitorHandle(pub Arc<dyn MonitorHandleProvider>)`. Provider: `id() -> u128`, `native_id() -> u64`, `name`, `position`, `scale_factor`, `current_video_mode`, `video_modes`. `VideoMode::new(..)` is a plain core struct. |
| `cursor::CustomCursor`, `icon::Icon` | `CustomCursor(pub Arc<dyn CustomCursorProvider>)`, `Icon(pub Arc<dyn IconProvider>)` |
| `window::WindowId` (`window.rs:28`), `event::DeviceId` (`event.rs:686`), `event::FingerId` (`:709`) | core newtypes (`usize`, `i64`, `usize`) with `from_raw` / `into_raw` |
| `window::PlatformWindowAttributes` (`window.rs:612`) | `WindowAttributes.platform: Option<Box<dyn PlatformWindowAttributes>>` replaces the per-platform struct |
| `event::KeyEvent` (`event.rs:787`) | all fields public, including `text_with_all_modifiers` (`:917`) and `key_without_modifiers` (`:934`). `KeyEventExtra` and `KeyEventExtModifierSupplement` are gone, so every backend fills these. |
| downcasting | `impl_dyn_casting!` (`casting.rs`): `cast_ref::<T>()` on `dyn Window` / `dyn ActiveEventLoop` |

First check: `winit-core` builds for `wasm32-wasmer-wasi` unmodified, through the default WASIX overlay registry. It resolved `bitflags 2.13.2`, `smol_str 0.3.2`, `cursor-icon 1.2.0`, `raw-window-handle 0.6.2`, `dpi 0.1.2`, `keyboard-types 0.8.3`. Its `Cargo.toml:34` says "`wasm32-unknown-unknown` and `wasm32-none`, but not `wasm32-wasi`" for the `web-time` dependency.

**How the top-level `winit` crate picks a backend.** Still statically:

- cfg aliases in `winit/build.rs`.
- Target-specific dependencies in `winit/Cargo.toml`.
- `winit/src/platform_impl/mod.rs:1-31` (`pub(crate) use winit_orbital as platform;` and so on), ending in the same `compile_error!`.

`winit::event_loop::EventLoop` (`winit/src/event_loop.rs:40`) wraps the concrete `platform_impl::EventLoop`. From the backend crate it needs only:

- `EventLoop` with `new(&mut PlatformSpecificEventLoopAttributes)` (`:97`), `run_app`, and `window_target() -> &dyn ActiveEventLoop`.
- `PlatformSpecificEventLoopAttributes: Default + Debug + PartialEq + Eq + Hash`.

There is no registration mechanism, so there are two routes:

- An application that calls `winit::event_loop::EventLoop::new()` still needs a patched top-level `winit`. That is roughly 15 lines: an alias in `build.rs`, a target dependency, and two `use` lines.
- Or it uses `winit_wasix::EventLoop` directly and only `winit-core` types; `EventLoopProvider` is the generic hook for that.

Second check: unmodified `winit` (master) on WASIX compiles `winit-web` and the `wasm-bindgen` stack (`winit/Cargo.toml:113-114` is `cfg(target_family = "wasm")`), then stops at `winit/src/platform_impl/mod.rs:31`.

**Pointer-event unification** (`winit-core/src/event.rs`, `WindowEvent` at `:49`):

| 0.30 | 0.31 |
|---|---|
| `CursorMoved { device_id, position }` and `Touch { phase: Moved, .. }` | `PointerMoved { device_id: Option<DeviceId>, position, primary: bool, source: PointerSource }` |
| `CursorEntered` / `CursorLeft` | `PointerEntered { device_id, position, primary, kind: PointerKind }` / `PointerLeft { .., position: Option<_>, .. }` |
| `MouseInput { state, button }` and `Touch { phase: Started / Ended, .. }` | `PointerButton { device_id, state, position, primary, button: ButtonSource, is_macos_activation_click }` |
| `Touch.id: u64`, `Touch.force` | `PointerKind::Touch(FingerId)`, `PointerSource::Touch { finger_id, force }`, `ButtonSource::Touch { finger_id, force }`; tablet tools via `TabletTool { kind, data }` |
| `MouseButton::Other(u16)` | `Button6` .. `Button32`; `ButtonSource::Unknown(u16)` |
| `DeviceEvent::{Added, Removed, MouseMotion, MouseWheel, Motion, Button, Key}` | `DeviceEvent::{PointerMotion, MouseWheel, Button, Key}` |
| `device_id: DeviceId` | `device_id: Option<DeviceId>` everywhere |
| `AxisMotion`, `HoveredFile`, `DroppedFile` | removed; drag and drop is `DragEntered` / `DragPosition` / `DragDropped` / `DragLeft` plus data transfers |
| `Resized`, `ScaleFactorChanged { inner_size_writer }` | `SurfaceResized`, `ScaleFactorChanged { surface_size_writer }` |
| `Ime::{Enabled, Preedit, Commit, Disabled}` | + `Ime::DeleteSurrounding { before_bytes, after_bytes }` |

Other changes that matter for the base layer:

- **Key names.** `KeyCode`, `KeyLocation`, `NamedKey` are re-exports from `keyboard-types` 0.8: `pub use keyboard_types::{Code as KeyCode, Location as KeyLocation, NamedKey};` (`winit-core/src/keyboard.rs:4`). That is the W3C names verbatim. `ModifiersState::meta_key()` replaces `super_key()`.
- **Proxy.** `EventLoopProxy` no longer carries a payload. `wake_up()` (`event_loop/mod.rs:434`) results in a coalesced `ApplicationHandler::proxy_wake_up()`. `EventLoop` is not generic.
- **Lifecycle.** `ApplicationHandler::can_create_surfaces` is the required method (`application/mod.rs:103`); `resumed` has a default body.
- **Orbital.** It gained `pump_app_events` and `run_app_on_demand` (`winit/src/event_loop.rs:283`, `:303`; `winit-orbital/src/event_loop.rs:523`, `:669`).
- **Still moving.** Between beta.3 and master, `ActiveEventLoop::create_window` changed from `Result<Box<dyn Window>, RequestError>` to `Result<Box<dyn Window>, CreateWindowError>`.

## 7. Part 2: what the backend needs from the base layer

All references are to winit `v0.30.13`.

### 7.1 Keyboard

**`KeyEvent`** (`src/event.rs:522-656`, `#[derive(Debug, Clone, Eq, PartialEq, Hash)]`):

| Field | Type | Meaning |
|---|---|---|
| `physical_key` | `keyboard::PhysicalKey` | layout-independent position (`:549`) |
| `logical_key` | `keyboard::Key` | "affected by all modifiers except Ctrl" (`:557-573`) |
| `text` | `Option<SmolStr>` | text produced by this key press, `None` if not text (`:575-594`); `Key::Named(NamedKey::Enter)` gives `Some("\r")` |
| `location` | `keyboard::KeyLocation` | `Standard` / `Left` / `Right` / `Numpad` (`:609`) |
| `state` | `ElementState` | `Pressed` / `Released` (`:614`) |
| `repeat` | `bool` | key-repeat flag (`:647`) |
| `platform_specific` | `pub(crate) platform_impl::KeyEventExtra` | private (`:649-655`) |

It is delivered as `WindowEvent::KeyboardInput { device_id, event, is_synthetic }` (`:205-219`). `is_synthetic` is only functional on X11 and Windows (`:209-217`).

**`KeyEventExtra` per backend:**

| Backend | Definition | Content |
|---|---|---|
| Web | `pub(crate) struct KeyEventExtra;` (`web/keyboard.rs:5-6`) | nothing |
| macOS | `{ text_with_all_modifiers: Option<SmolStr>, key_without_modifiers: Key }` (`macos/event.rs:17-21`) | `text_with_all_modifiers` is `NSEvent.characters()`, or `None` if empty (`:109-118`). `key_without_modifiers` is `UCKeyTranslate` with no modifiers (`get_modifierless_char`, `:23-77`), or the named key (`:120-152`). |
| Windows, X11/Wayland, Orbital | same two fields (`windows/mod.rs:94-98`, `linux/mod.rs:634-638`, `orbital/mod.rs:251-255`) | |
| iOS, Android | `pub struct KeyEventExtra {}` (`ios/mod.rs:43`, `android/mod.rs:132`) | nothing |

What Web puts in the public fields:

- `physical_key` from `KeyboardEvent.code` (`web/web_sys/event.rs:163-166`).
- `logical_key` from `KeyboardEvent.key` (`:168-170`).
- `text`: the character for `Key::Character`; `"\t"` / `"\r"` / `" "` for Tab / Enter / Space; otherwise `None` (`:172-183`).
- `location` from `KeyboardEvent.location` (`:185-196`), and `repeat` from `KeyboardEvent.repeat`.
- `device_id` is `DeviceId::dummy()` (`web/event_loop/window_target.rs:145`).

Web sets `text` on key release too (`web/web_sys/canvas.rs:272-293`). macOS and Android set it on press only (`macos/event.rs:154`, `android/mod.rs:449-453`).

**`PhysicalKey`** (`src/keyboard.rs:223-233`) is `Code(KeyCode)` or `Unidentified(NativeKeyCode)`. `NativeKeyCode` (`:88-100`) has `Unidentified`, `Android(u32)`, `MacOS(u16)`, `Windows(u16)`, `Xkb(u32)`. There is no variant for a new platform, so unmapped WASIX keys can only be `PhysicalKey::Unidentified(NativeKeyCode::Unidentified)`, which is what Web does (`web/keyboard.rs:519`).

**`KeyCode`** (`:293-742`, `#[non_exhaustive]`, 194 variants), compared mechanically against the W3C `code` list (172 values):

| winit `KeyCode` | W3C `code` | Note |
|---|---|---|
| `SuperLeft`, `SuperRight` (`:432`, `:434`) | `MetaLeft`, `MetaRight` | renamed (documented at `:286-291`) |
| `Meta` (`:617`, "Legacy modifier key. Also called 'Super' in certain places.") | legacy `Super` | Web never produces it; the string `"Super"` falls through to `Unidentified` |
| (none) | `Unidentified` | expressed as `PhysicalKey::Unidentified(..)` |
| `F13` .. `F35` (`:684-739`) | not in the tables | conforming by the spec's rule for keyboards with more than 12 function keys ("F" followed by the number) |
| the other 168 variants | identical names | `web/keyboard.rs:324-522` maps 193 strings; only `MetaLeft` / `MetaRight` differ from the variant name |

So `KeyCode` is the W3C `code` vocabulary. The only renames are `MetaLeft` to `SuperLeft`, `MetaRight` to `SuperRight`, and `Super` to `Meta`.

**`Key`** (`:1470-1492`) has `Named(NamedKey)`, `Character(SmolStr)`, `Unidentified(NativeKey)`, `Dead(Option<char>)`. `NativeKey` (`:141-156`) additionally has `Web(SmolStr)`.

**`NamedKey`** (`:752-1458`, `#[non_exhaustive]`, 306 variants) is the W3C `key` vocabulary with two documented exceptions (`:744-749`):

- W3C `"Meta"` is `NamedKey::Super` (`web/keyboard.rs:25`).
- W3C `" "` is `NamedKey::Space` (`web/keyboard.rs:28`).

`NamedKey::Meta` (`:795`) is never produced by Web. `NamedKey::to_text` (`:1576-1585`) maps `Enter` to `"\r"`, `Backspace` to `"\x08"`, `Tab` to `"\t"`, `Space` to `" "`, `Escape` to `"\x1b"`.

**`KeyLocation`** (`:1621-1686`): `Standard`, `Left`, `Right`, `Numpad`.

**`ModifiersState`** (`:1688-1725`, bitflags `u32`): `SHIFT = 0b100`, `CONTROL = 0b100 << 3`, `ALT = 0b100 << 6`, `SUPER = 0b100 << 9`, with `shift_key()`, `control_key()`, `alt_key()`, `super_key()`. There are no Caps Lock, Num Lock or AltGr bits.

**`Modifiers`** (`src/event.rs:659-728`) holds `state: ModifiersState` plus a private `pressed_mods: ModifiersKeys` (`src/keyboard.rs:1745-1757`: `LSHIFT`, `RSHIFT`, `LCONTROL`, `RCONTROL`, `LALT`, `RALT`, `LSUPER`, `RSUPER`).

- Getters `lshift_state()` .. `rsuper_state()` return `ModifiersKeyState::{Pressed, Unknown}`. There is no "released", only "unknown" (`src/event.rs:715-721`).
- Only macOS (`macos/event.rs:309-334`) and Orbital (`orbital/event_loop.rs:232-271`) fill `pressed_mods`. Every other backend uses `Modifiers::from(ModifiersState)`, so side-specific state is `Unknown` there.
- It is delivered as `WindowEvent::ModifiersChanged(Modifiers)` (`:222`). Android and iOS never send it.

**`KeyEventExtModifierSupplement`** (`src/platform/modifier_supplement.rs:6-23`) has two methods:

- `fn text_with_all_modifiers(&self) -> Option<&str>`: "Identical to `KeyEvent::text` but this is affected by Ctrl"; Ctrl+a gives `Some("\x01")`.
- `fn key_without_modifiers(&self) -> Key`: "ignores all modifiers including, but not limited to Shift, Caps Lock, and Ctrl"; never `Dead`.

It exists on Windows, macOS, X11, Wayland and Orbital (`src/platform/mod.rs:44-52`), and not on Web, iOS or Android.

A new platform can implement it: give `KeyEventExtra` those two fields and add the alias to that cfg list. The skeleton does this, and `wasix-test/src/bin/event_loop.rs` calls both methods in a compile-time check.

Base-layer consequence: the host reports, per key event, the key value without modifiers and the text with Ctrl applied. Alternatively the backend derives them, as Orbital does (`orbital/event_loop.rs:173-187`, `:352-365`).

### 7.2 IME

**`Ime`** (`src/event.rs:772-803`):

| Variant | Meaning |
|---|---|
| `Enabled` | IME became active; `Preedit` / `Commit` may follow (`:775-780`) |
| `Preedit(String, Option<(usize, usize)>)` | composing text and cursor range, byte indexed; `None` hides the cursor; an empty string means preedit cleared (`:782-789`) |
| `Commit(String)` | insert this text; "Right before this event winit will send empty `Self::Preedit` event" (`:791-794`) |
| `Disabled` | no more `Preedit` / `Commit` until the next `Enabled`; clear pending preedit (`:796-802`) |

It is delivered as `WindowEvent::Ime(Ime)` (`:224-231`): "You have to explicitly enable this event using `Window::set_ime_allowed`".

| Method (`src/window.rs`) | Doc | Implemented where |
|---|---|---|
| `set_ime_allowed(&self, allowed: bool)` (`:1283`) | "When IME is allowed, the window will receive `Ime` events, and during the preedit phase the window will NOT get `KeyboardInput` events." / "When IME is not allowed, the window won't receive `Ime` events, and will receive `KeyboardInput` events for every keypress instead." / "IME is **not** allowed by default." (`:1260-1270`) | iOS and Android show or hide the soft keyboard (`android/mod.rs:918-924`, `ios/window.rs:374-384`). Web and Orbital: unsupported (`:1277`). |
| `set_ime_cursor_area<P, S>(&self, position: P, size: S)` (`:1248`) | where the candidate box should go (`:1213-1222`) | unsupported on iOS, Android, Web, Orbital (`:1243`) |
| `set_ime_purpose(&self, purpose: ImePurpose)` (`:1294`) | hint | only Wayland (`:1292`; `linux/wayland/window/state.rs:1051`) |

`ImePurpose` (`:1827-1839`, `#[non_exhaustive]`): `Normal` (default), `Password`, `Terminal`.

**Is "no double insertion" between `KeyEvent.text` and `Ime::Commit` promised?** Not in so many words. The only documented rule is the `set_ime_allowed` text above (`src/window.rs:1262-1264`), plus the sequences described in the `Ime` docs (`src/event.rs:730-771`). The exclusivity is implemented per backend:

- **macOS** (`macos/view.rs`):
- `insertText:` commits only when there is marked text (`:410-415`), and sets `ImeState::Committed`.
- `doCommandBySelector:` then returns early with the comment "We shouldn't forward any character from just committed text, since we'll end up sending it twice with some IMEs like Korean one" (`:423-428`).
- `keyDown:` computes `had_ime_input` and sends `KeyboardInput` only `if !had_ime_input || forward_key_to_app` (`:479-497`).
- `keyUp:` sends only in `ImeState::Ground | Disabled` (`:507-517`).
- **X11:** one input goes down exactly one branch of `xinput_key_input`. It is `KeyboardInput` and `return` when `keycode != 0 && !self.is_composing` (`linux/x11/event_processor.rs:969-990`), otherwise `Ime::Preedit("")` plus `Ime::Commit(written)` (`:994-1011`).
- **Windows:** `Ime::Commit` comes only from `WM_IME_COMPOSITION` / `WM_IME_ENDCOMPOSITION` (`windows/event_loop.rs:1527-1611`).

An application may therefore insert both `KeyEvent.text` and `Ime::Commit` text and rely on the backend never reporting one input through both. A WASIX backend has to uphold that itself. The simplest host contract is that key events for composing keys are not delivered at all while a composition is active, and the committed string arrives once.

**macOS, dead keys, IME not allowed.** `keyDown:` calls `interpretKeyEvents` only `if self.ivars().ime_allowed.get()` (`macos/view.rs:466-468`). Without it nothing combines the sequence:

- The dead key is delivered as a plain `KeyboardInput` with `logical_key = Key::Dead(Some(c))`. `macos/event.rs:79-90` treats empty `charactersIgnoringModifiers` as "Probably a dead key".
- Its `text` is `None` (`:154`), and the following key arrives uncombined.
- The public doc agrees: "macOS: IME must be enabled to receive text-input where dead-key sequences are combined" (`src/window.rs:1274-1275`).

With IME allowed, the dead key becomes marked text (`Ime::Preedit`) and the result an `Ime::Commit`. `Window::reset_dead_keys` does nothing on macOS and Web (`src/window.rs:649`, `macos/window_delegate.rs:1707-1709`). This is source reading only; it was not run.

### 7.3 Pointer, touch, gestures, device events

| `WindowEvent` (`src/event.rs`) | Fields | Notes |
|---|---|---|
| `CursorMoved` (`:242-250`) | `device_id: DeviceId`, `position: PhysicalPosition<f64>` | physical pixels relative to the window's top-left |
| `CursorEntered` (`:261`), `CursorLeft` (`:272`) | `device_id` | |
| `MouseInput` (`:278`) | `device_id`, `state: ElementState`, `button: MouseButton` | No position; see notes below. `MouseButton` (`:939-948`): `Left`, `Right`, `Middle`, `Back`, `Forward`, `Other(u16)`. |
| `MouseWheel` (`:275`) | `device_id`, `delta: MouseScrollDelta`, `phase: TouchPhase` | see below |
| `Touch(Touch)` (`:365`) | `Touch` (`:840-857`): `device_id`, `phase: TouchPhase`, `location: PhysicalPosition<f64>`, `force: Option<Force>`, `id: u64` | `id` is the finger; reusable after `Ended` (`:824-826`) |
| `PinchGesture` (`:286-294`) | `device_id`, `delta: f64`, `phase` | macOS and iOS only |
| `PanGesture` (`:302-307`) | `device_id`, `delta: PhysicalPosition<f32>`, `phase` | iOS only |
| `DoubleTapGesture` (`:327`) | `device_id` | macOS and iOS only |
| `RotationGesture` (`:338-343`) | `device_id`, `delta: f32` (degrees, counterclockwise positive), `phase` | macOS and iOS only |
| `TouchpadPressure` (`:350`) | `device_id`, `pressure: f32`, `stage: i64` | macOS force touch only |
| `AxisMotion` (`:353`) | `device_id`, `axis: AxisId` (`u32`), `value: f64` | |

Notes on `MouseInput` and `Touch`:

- Because `MouseInput` has no position, Web sends a `CursorMoved` first: "A mouse down event may come in without any prior CursorMoved events" (`web/event_loop/window_target.rs:375-388`).
- DOM `button` values 0 / 1 / 2 / 3 / 4 map to `Left` / `Middle` / `Right` / `Back` / `Forward` (`web/web_sys/event.rs:55-68`).
- Web uses `pointerId` for both `Touch.id` and `device_id` (`web/event_loop/window_target.rs:304-310`).

`MouseScrollDelta` (`:951-975`):

- `LineDelta(f32, f32)`: lines or rows, horizontal then vertical. "Positive values indicate that the content that is being scrolled should move right and down (revealing more content left and up)."
- `PixelDelta(PhysicalPosition<f64>)`: physical pixels, same sign convention.

This is the opposite sign of the DOM `WheelEvent`. Web negates both axes and converts `DOM_DELTA_PIXEL` from CSS to physical pixels (`web/web_sys/event.rs:146-161`). macOS chooses `PixelDelta` when `hasPreciseScrollingDeltas` (`macos/view.rs:668-676`). X11, Windows and Orbital only send `LineDelta`.

`TouchPhase` (`:806-813`): `Started`, `Moved`, `Ended`, `Cancelled`. For `MouseWheel`, every backend except macOS always sends `Moved` (Web: `window_target.rs:509`). macOS derives `Started` / `Moved` / `Ended` from the gesture and momentum phases (`macos/view.rs:678-691`).

`Force` (`:860-910`) has two forms:

- `Calibrated { force: f64, max_possible_force: f64, altitude_angle: Option<f64> }`, used on iOS.
- `Normalized(f64)` in 0.0..=1.0, used on Web (`PointerEvent.pressure`, `web/web_sys/pointer.rs:89`), Android (`android/mod.rs:418`) and Windows (`pressure / 1024`).

`DeviceEvent` (`:470-505`) is delivered as `Event::DeviceEvent { device_id, event }`. Its variants:

- `Added`, `Removed`
- `MouseMotion { delta: (f64, f64) }` (raw, "unspecified units")
- `MouseWheel { delta: MouseScrollDelta }`
- `Motion { axis: AxisId, value: f64 }`
- `Button { button: ButtonId, state: ElementState }`
- `Key(RawKeyEvent)`, where `RawKeyEvent { physical_key, state }` (`:514-519`)

It is controlled by `ActiveEventLoop::listen_device_events(DeviceEvents::{Always, WhenFocused, Never})` (`src/event_loop.rs:596-605`). Android and iOS send none; Orbital sends only `MouseMotion`. A backend that supports `CursorGrabMode::Locked` needs `MouseMotion`.

Events actually emitted by the two smallest desktop-style backends, as a realistic first target:

| Backend | `WindowEvent` variants emitted |
|---|---|
| Orbital | `CloseRequested`, `CursorEntered`, `CursorLeft`, `CursorMoved`, `Destroyed`, `Focused`, `Ime`, `KeyboardInput`, `ModifiersChanged`, `MouseInput`, `MouseWheel`, `Moved`, `RedrawRequested`, `Resized` |
| Web | the same minus `CloseRequested`, `Ime`, `Moved`; plus `Occluded`, `ScaleFactorChanged`, `ThemeChanged`, `Touch` |

### 7.4 `Window`: every method

Lines are the public wrappers in `src/window.rs`. The platform method has the same name and takes the argument types after `Into` conversion (`Position`, `Size`, `Cursor`). "May refuse" cites the in-tree precedent from the method's own platform notes.

Synchronous "not supported" is only expressible for the seven methods marked with a `Result`, plus `Option` returns. Everything else returns `()` or a plain value, so "unsupported" means a silent no-op or a fixed answer.

Identity, scale, redraw:

| Method | Returns | Backend must / may |
|---|---|---|
| `id` (`:495`) | `WindowId` | must |
| `scale_factor` (`:563`) | `f64` | must answer; Orbital and Android return `1.0` (`:546-553`). Changes are announced with `ScaleFactorChanged { scale_factor, inner_size_writer }` (`src/event.rs:379-385`). |
| `request_redraw` (`:597`) | `()` | must: coalesce, deliver `WindowEvent::RedrawRequested`, wake the loop (any thread) |
| `pre_present_notify` (`:636`) | `()` | may no-op (everything except Wayland, `:631`) |
| `reset_dead_keys` (`:654`) | `()` | may no-op (Web, macOS, `:649`) |

Size and position:

| Method | Returns | Backend must / may |
|---|---|---|
| `inner_size` (`:758`) | `PhysicalSize<u32>` | must: physical pixels of the drawable area |
| `outer_size` (`:822`) | `PhysicalSize<u32>` | must answer; may equal `inner_size` (Web, `:819-820`) |
| `request_inner_size` (`:800`, `#[must_use]`) | `Option<PhysicalSize<u32>>` | `None` means it asked the host and `Resized` follows. `Some(size)` means applied now. To refuse, return the current size (`:766-774`). |
| `inner_position` (`:678`) | **`Result<PhysicalPosition<i32>, NotSupportedError>`** | may refuse (Android / Wayland, `:674`) |
| `outer_position` (`:701`) | **`Result<PhysicalPosition<i32>, NotSupportedError>`** | may refuse (Android / Wayland, `:699`) |
| `set_outer_position` (`:734`) | `()` | may no-op (Android / Wayland, `:730`) |
| `set_min_inner_size` (`:845`), `set_max_inner_size` (`:873`) | `()` | may no-op (iOS / Android / Orbital) |
| `resize_increments` (`:889`) | `Option<PhysicalSize<u32>>` | may return `None` (`:887`) |
| `set_resize_increments` (`:905`) | `()` | may no-op (`:903`) |

State:

| Method | Returns | Backend must / may |
|---|---|---|
| `set_title` (`:924`) | `()` | may no-op (iOS / Android) |
| `title` (`:1409`) | `String` | may return `""` (`:1407`) |
| `set_visible` (`:973`) | `()` | may no-op (Android / Wayland / Web) |
| `is_visible` (`:988`) | `Option<bool>` | may return `None` (`:986`) |
| `set_transparent` (`:945`), `set_blur` (`:959`) | `()` | may no-op |
| `set_resizable` (`:1009`) | `()` | may no-op (iOS / Android / Web) |
| `is_resizable` (`:1021`) | `bool` | fixed answer allowed (Web: `true`) |
| `set_enabled_buttons` (`:1032`) | `()` | may no-op |
| `enabled_buttons` (`:1047`) | `WindowButtons` | `WindowButtons::all()` when unsupported (`:1045-1046`) |
| `set_minimized` (`:1059`) | `()` | may no-op |
| `is_minimized` (`:1077`) | `Option<bool>` | may return `None` |
| `set_maximized` (`:1088`) | `()` | may no-op |
| `is_maximized` (`:1099`) | `bool` | `false` when unsupported |
| `set_decorations` (`:1160`) | `()` | may no-op |
| `is_decorated` (`:1174`) | `bool` | "returns `true` when no decorations are required" (`:1167-1168`) |
| `set_window_level` (`:1184`) | `()` | hint, may ignore (`:1181`) |
| `set_window_icon` (`:1208`) | `()` | may no-op (most platforms, `:1200`) |
| `focus_window` (`:1314`) | `()` | may no-op (`:1312`) |
| `has_focus` (`:1325`) | `bool` | should mirror `WindowEvent::Focused` (`:1321`); Orbital just returns `false` (`orbital/window.rs:490-492`) |
| `request_user_attention` (`:1344`) | `()` | may no-op |
| `set_theme` (`:1365`) | `()` | may no-op |
| `theme` (`:1383`) | `Option<Theme>` | may return `None` |
| `set_content_protected` (`:1397`) | `()` | may no-op |

Fullscreen:

| Method | Returns | Backend must / may |
|---|---|---|
| `set_fullscreen(Option<Fullscreen>)` (`:1127`) | `()` | may no-op (Android / Orbital, `:1122`); `Fullscreen::{Exclusive(VideoModeHandle), Borderless(Option<MonitorHandle>)}` (`:1745-1750`) |
| `fullscreen` (`:1145`) | `Option<Fullscreen>` | may return `None`; must not panic (used in `Drop`) |

Cursor:

| Method | Returns | Backend must / may |
|---|---|---|
| `set_cursor(impl Into<Cursor>)` (`:1425`) | `()` | may no-op (iOS / Android / Orbital); `Cursor::{Icon(CursorIcon), Custom(CustomCursor)}` |
| `set_cursor_icon` (`:1434`, deprecated) | `()` | forwards to `set_cursor` |
| `set_cursor_visible` (`:1505`) | `()` | may no-op (iOS / Android) |
| `set_cursor_position` (`:1457`) | **`Result<(), ExternalError>`** | may refuse (iOS / Android / Web / Orbital, `:1455`) |
| `set_cursor_grab(CursorGrabMode)` (`:1483`) | **`Result<(), ExternalError>`** | may refuse per mode: `None`, `Confined`, `Locked` (`:1687-1712`); apps try `Confined` then `Locked` (`:1476-1479`) |
| `set_cursor_hittest` (`:1575`) | **`Result<(), ExternalError>`** | may refuse (`:1573`) |
| `drag_window` (`:1522`) | **`Result<(), ExternalError>`** | may refuse (`:1520`) |
| `drag_resize_window(ResizeDirection)` (`:1537`) | **`Result<(), ExternalError>`** | may refuse (`:1534-1535`) |
| `show_window_menu` (`:1555`) | `()` | may no-op (everything except Windows, `:1552`) |

`ExternalError` (`src/error.rs:8-15`): `NotSupported(NotSupportedError)`, `Ignored`, `Os(OsError)`.

IME: `set_ime_allowed` (`:1283`), `set_ime_cursor_area` (`:1248`), `set_ime_purpose` (`:1294`). All return `()` and all may no-op (7.2).

Monitors and handles:

| Method | Returns | Backend must / may |
|---|---|---|
| `current_monitor` (`:1587`) | `Option<MonitorHandle>` | may return `None` (Web) |
| `available_monitors` (`:1599`) | iterator | may be empty (Web) |
| `primary_monitor` (`:1618`) | `Option<MonitorHandle>` | may return `None` (Wayland / Web, `:1614`) |
| `window_handle` (rwh 0.6, `:1625-1633`) | `Result<WindowHandle<'_>, HandleError>` | may refuse: `HandleError::NotSupported` / `Unavailable` |
| `display_handle` (rwh 0.6, `:1636-1644`) | `Result<DisplayHandle<'_>, HandleError>` | same |
| `raw_window_handle` / `raw_display_handle` (rwh 0.5, `:1657-1673`), rwh 0.4 (`:1675-1680`) | handle, infallible | cannot refuse |

**Creation.** `ActiveEventLoop::create_window(WindowAttributes) -> Result<Window, OsError>` (`src/event_loop.rs:378`). `WindowAttributes` (`src/window.rs:104-130`) carries `inner_size`, `min_inner_size`, `max_inner_size`, `position`, `resizable`, `enabled_buttons`, `title`, `maximized`, `visible`, `transparent`, `blur`, `decorations`, `window_icon`, `preferred_theme`, `resize_increments`, `content_protected`, `window_level`, `active`, `cursor`, `parent_window` (rwh 0.6), `fullscreen`, `platform_specific`. Dropping the `Window` closes it (`:19`).

**Threading.** `Window` is `Send + Sync` and "can be freely used from other threads" (`src/window.rs:21-29`). Setters go through `maybe_queue_on_main` (fire and forget). Getters and the `Result` methods go through `maybe_wait_on_main`, which needs the answer synchronously.

### 7.5 Event loop

**`ControlFlow`** (`src/event_loop.rs:155-190`, default `Wait`) is read after `AboutToWait`:

| Variant | Loop behaviour | Next `StartCause` (`src/event.rs:127-148`) |
|---|---|---|
| `Poll` | start the next iteration immediately | `Poll` |
| `Wait` | block until an event arrives | `WaitCancelled { start, requested_resume: None }` |
| `WaitUntil(Instant)` | block until an event or the deadline | `ResumeTimeReached { start, requested_resume }`, or `WaitCancelled { start, requested_resume: Some(_) }` if woken earlier |

`Init` is sent once. `ControlFlow::wait_duration(Duration)` (`:184-189`) builds `WaitUntil`.

One iteration (`src/event.rs:10-30`) runs in this order:

1. `new_events(cause)`.
2. Window, user and device events.
3. `RedrawRequested` for every window that asked.
4. `about_to_wait`, then the wait.

`exiting` is called once at the end. `resumed` is a required `ApplicationHandler` method (`src/application.rs:83`) and is where windows are created. Orbital sends it once after `Init` (`orbital/event_loop.rs:514-516`), and so does the skeleton.

**`EventLoopProxy<T>`** (0.30, `src/event_loop.rs:549-572`) is `Clone`. `send_event(&self, event: T) -> Result<(), EventLoopClosed<T>>` "emits a `UserEvent(event)` event in the event loop ... Returns an `Err` if the associated `EventLoop` no longer exists." (`:560-566`).

- Every event is delivered, without coalescing, through `ApplicationHandler::user_event` (`src/application.rs:88`).
- It must be `Send + Sync` for `T: Send`.
- It must wake a loop blocked in `Wait` from any thread.

All of this is verified by `proxy_wait`.

**Which entry points a new platform must provide:**

| API | Backing platform method | Required? |
|---|---|---|
| `EventLoop::run_app(self, app: &mut A)` (`src/event_loop.rs:264`) and deprecated `run` (`:225`) | `EventLoop::run` | yes; the only required one |
| `EventLoopExtPumpEvents::pump_app_events(&mut self, timeout: Option<Duration>, app)` (`src/platform/pump_events.rs:106`) | `pump_events` | opt-in (3.2.5); Windows, macOS, Android, X11, Wayland only |
| `EventLoopExtRunOnDemand::run_app_on_demand(&mut self, app)` (`src/platform/run_on_demand.rs:70`) | `run_on_demand` + `clear_exit` | opt-in; same five |

Only one `EventLoop` can exist per process. `EventLoopError::RecreationAttempt` comes from the common code (`src/event_loop.rs:69`, `:118-120`) and shows in the test output.

The two host primitives the loop needs are "wait for the next event, with an optional deadline" and "wake that wait from any thread".

### 7.6 Monitors

`MonitorHandle` (`src/monitor.rs:102-167`):

- `name() -> Option<String>`
- `size() -> PhysicalSize<u32>`
- `position() -> PhysicalPosition<i32>`
- `refresh_rate_millihertz() -> Option<u32>`
- `scale_factor() -> f64`
- `video_modes() -> impl Iterator<Item = VideoModeHandle>`

`VideoModeHandle` (`:18-95`):

- `size()`, `bit_depth() -> u16`, `refresh_rate_millihertz() -> u32`, `monitor() -> MonitorHandle`.
- Ordered by monitor, then size, refresh rate and bit depth descending (`:35-48`).
- `Display` is `"{w}x{h} @ {mHz} mHz ({bpp} bpp)"`.

Monitors are optional:

- Web returns none, and its handle methods are `unreachable!()` (`web/monitor.rs`).
- Orbital returns one dummy monitor with size `(0, 0)` and scale `1.0` (`orbital/mod.rs:187-223`).
- Video modes only matter for `Fullscreen::Exclusive`.

### 7.7 raw-window-handle 0.6.2

Both enums are `#[non_exhaustive]` (`src/lib.rs:111`, `:264`) and `#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]`. All variants exist on all targets (`src/lib.rs:100-104`, `:253-257`).

Every handle struct is `#[non_exhaustive]` as well. Outside the crate they are built with `::new(..)`, not a struct literal: `WebWindowHandle::new(id)`.

`RawWindowHandle` (16 variants, `src/lib.rs:113-210`):

| Variant | Struct and public fields |
|---|---|
| `UiKit` | `UiKitWindowHandle { ui_view: NonNull<c_void>, ui_view_controller: Option<NonNull<c_void>> }` |
| `AppKit` | `AppKitWindowHandle { ns_view: NonNull<c_void> }` |
| `Orbital` | `OrbitalWindowHandle { window: NonNull<c_void> }` |
| `OhosNdk` | `OhosNdkWindowHandle { native_window: NonNull<c_void> }` |
| `Xlib` | `XlibWindowHandle { window: c_ulong, visual_id: c_ulong }` |
| `Xcb` | `XcbWindowHandle { window: NonZeroU32, visual_id: Option<NonZeroU32> }` |
| `Wayland` | `WaylandWindowHandle { surface: NonNull<c_void> }` |
| `Drm` | `DrmWindowHandle { plane: u32 }` |
| `Gbm` | `GbmWindowHandle { gbm_surface: NonNull<c_void> }` |
| `Win32` | `Win32WindowHandle { hwnd: NonZeroIsize, hinstance: Option<NonZeroIsize> }` |
| `WinRt` | `WinRtWindowHandle { core_window: NonNull<c_void> }` |
| **`Web`** | **`WebWindowHandle { id: u32 }`** (`src/web.rs:47-58`) |
| `WebCanvas` | `WebCanvasWindowHandle { obj: NonNull<c_void> }` (pointer to a `JsValue`) |
| `WebOffscreenCanvas` | `WebOffscreenCanvasWindowHandle { obj: NonNull<c_void> }` |
| `AndroidNdk` | `AndroidNdkWindowHandle { a_native_window: NonNull<c_void> }` |
| `Haiku` | `HaikuWindowHandle { b_window: NonNull<c_void>, b_direct_window: Option<NonNull<c_void>> }` |

`RawDisplayHandle` (13 variants, `src/lib.rs:266-344`):

| Variant | Struct and public fields |
|---|---|
| `UiKit`, `AppKit`, `Orbital`, `Ohos`, `Windows`, **`Web`**, `Android`, `Haiku` | empty structs: `UiKitDisplayHandle {}`, `AppKitDisplayHandle {}`, `OrbitalDisplayHandle {}`, `OhosDisplayHandle {}`, `WindowsDisplayHandle {}`, `WebDisplayHandle {}`, `AndroidDisplayHandle {}`, `HaikuDisplayHandle {}` |
| `Xlib` | `XlibDisplayHandle { display: Option<NonNull<c_void>>, screen: c_int }` |
| `Xcb` | `XcbDisplayHandle { connection: Option<NonNull<c_void>>, screen: c_int }` |
| `Wayland` | `WaylandDisplayHandle { display: NonNull<c_void> }` |
| `Drm` | `DrmDisplayHandle { fd: i32 }` |
| `Gbm` | `GbmDisplayHandle { gbm_device: NonNull<c_void> }` |

**Which variant can carry a 32-bit WASIX window id today?**

| Candidate | Fits? |
|---|---|
| **`Web(WebWindowHandle { id: u32 })` + `RawDisplayHandle::Web(WebDisplayHandle {})`** | Best fit. Plain `u32`, `Send + Sync` (`src/lib.rs:468`, `:484`), and `DisplayHandle::web()` is a safe constructor (`src/web.rs:26-44`). |
| `Drm(DrmWindowHandle { plane: u32 })` + `Drm { fd: i32 }` | also plain integers and `Send + Sync`, but means a KMS plane and a DRM fd |
| `Xcb { window: NonZeroU32, .. }`, `Xlib { window: c_ulong }`, `Win32 { hwnd: NonZeroIsize }` | 32-bit on wasm32, but mean X11 / Win32 |
| pointer variants (`Orbital`, `AndroidNdk`, ..) | 32-bit on wasm32, but `!Send`, non-null, and an integer smuggled in a pointer |

Notes on borrowing `Web`:

- The documented meaning of the id is "ID value inserted into the data attributes of the canvas element as `raw-handle`" (`src/web.rs:50-57`).
- raw-window-handle 0.5.2 adds "0 should be reserved for invalid / null IDs", so do not hand out id 0.
- winit 0.30's own Web backend no longer uses `Web` under `rwh_06`. It returns `RawWindowHandle::WebCanvas` (`web/window.rs:89-103`) and sets the `data-raw-handle` attribute only for `rwh_04` / `rwh_05` (`web/event_loop/window_target.rs:78-79`).
- On a WASIX target the only consumers that interpret `Web` are crates that mistake WASIX for the Web; softbuffer does until patched (8).
- Because the enum is `#[non_exhaustive]`, a dedicated variant can be added upstream in a minor release later.
- `RawWindowHandle` itself is `!Send + !Sync` (`src/lib.rs:452`). That does not matter, since winit builds it on demand.

### 7.8 Checklist for `wasmer_gui_v0`, derived from the above

| Need | Why |
|---|---|
| Blocking "next event" with optional deadline, plus a wake call usable from any thread | `ControlFlow::{Wait, WaitUntil}`, `EventLoopProxy`, `request_redraw` from other threads (7.5) |
| All window calls thread-safe, or a way to marshal them to the loop thread and get an answer | `Window: Send + Sync`; `maybe_wait_on_main` needs a synchronous result (7.4) |
| Window create / destroy with size, title, visibility; 32-bit non-zero id | `Window::new`, `WindowId: From<u64> + Into<u64>`, `WebWindowHandle.id` (7.7) |
| Synchronous getters: surface size in physical pixels, scale factor, focus | `inner_size`, `scale_factor`, `has_focus` return plain values |
| Events: resized, scale factor changed, close requested, destroyed, focus, redraw | Orbital / Web sets (7.3) |
| Keyboard: W3C `code`, W3C `key` (or character), location, repeat, pressed / released, text, modifiers (ideally left / right) | `KeyEvent`, `Modifiers` (7.1) |
| Optionally key-without-modifiers and text-with-all-modifiers | `KeyEventExtModifierSupplement` (7.1) |
| Text composition: enabled / preedit (byte cursor range) / commit / disabled, an "allowed" switch, a cursor rectangle; composing keys withheld | `Ime`, `set_ime_allowed`, `set_ime_cursor_area` (7.2) |
| Pointer: move, enter, leave, button (5 named + numbered), wheel in lines or physical pixels with winit's sign, pointer id | 7.3 |
| Touch: id, phase including cancel, position, optional normalized force | `Touch` (7.3) |
| Optional: raw relative motion, cursor icon / visibility / grab, fullscreen, monitors, theme | each can be refused (7.4) |
| Pixel presentation: window id, pointer, width, height, format, stride, alpha mode, damage rectangles | softbuffer (8) |

## 8. Part 3: softbuffer 0.4.8

### 8.1 Where backends are selected

There is no `build.rs` and there are no cfg aliases in 0.4.8 ("Remove `cfg_aliases` dependency", `CHANGELOG.md`, 0.4.7). The cfgs are written out twice:

| Backend | cfg (`src/backends/mod.rs:4-46`, repeated in `src/backend_dispatch.rs:232-277`) |
|---|---|
| `android` | `target_os = "android"` |
| `cg` | `target_vendor = "apple"` |
| `kms`, `wayland`, `x11` | `all(feature = "<name>", not(any(target_os = "android", target_vendor = "apple", target_os = "redox", target_family = "wasm", target_os = "windows")))` |
| `orbital` | `target_os = "redox"` |
| **`web`** | **`target_family = "wasm"`** |
| `win32` | `target_os = "windows"` |

`make_dispatch!` (`src/backend_dispatch.rs:10-228`) generates the enums `ContextDispatch<D>`, `SurfaceDispatch<D, W>` and `BufferDispatch<'a, D, W>`, with one variant per enabled backend, and forwards every trait method. `ContextDispatch::new` tries the variants in order and moves on when one returns `InitError::Unsupported(display)` (`:38-55`).

On WASIX, unmodified softbuffer therefore compiles and silently selects the Web backend, with `js-sys`, `wasm-bindgen` and `web-sys`. Checked at run time: `softbuffer::Context::new(DisplayHandle::web())` under wasmer panics at `js-sys-0.3.106/src/lib.rs:13652` with "cannot access imported statics on non-wasm targets".

### 8.2 The traits a backend implements (`src/backend_interface.rs`, complete)

```rust
pub(crate) trait ContextInterface<D: HasDisplayHandle + ?Sized> {
  fn new(display: D) -> Result<Self, InitError<D>> where D: Sized, Self: Sized;
}

pub(crate) trait SurfaceInterface<D: HasDisplayHandle + ?Sized, W: HasWindowHandle + ?Sized> {
  type Context: ContextInterface<D>;
  type Buffer<'a>: BufferInterface where Self: 'a;

  fn new(window: W, context: &Self::Context) -> Result<Self, InitError<W>> where W: Sized, Self: Sized;
  fn window(&self) -> &W;
  fn resize(&mut self, width: NonZeroU32, height: NonZeroU32) -> Result<(), SoftBufferError>;
  fn buffer_mut(&mut self) -> Result<Self::Buffer<'_>, SoftBufferError>;
  fn fetch(&mut self) -> Result<Vec<u32>, SoftBufferError> { Err(SoftBufferError::Unimplemented) }
}

pub(crate) trait BufferInterface {
  fn width(&self) -> NonZeroU32;
  fn height(&self) -> NonZeroU32;
  fn pixels(&self) -> &[u32];
  fn pixels_mut(&mut self) -> &mut [u32];
  fn age(&self) -> u8;
  fn present_with_damage(self, damage: &[Rect]) -> Result<(), SoftBufferError>;
  fn present(self) -> Result<(), SoftBufferError>;
}
```

Supporting types:

- `InitError<D>` (`src/error.rs:163-169`) is `Failure(SoftBufferError)` or `Unsupported(D)`, which gives the handle back.
- A blanket `impl<D: HasDisplayHandle> ContextInterface<D> for D` (`src/backends/mod.rs:48-52`) serves as the context for backends without display state: Android, Win32, CoreGraphics, Orbital and the WASIX skeleton.
- `Rect` (`src/lib.rs:62-73`) is `{ x: u32, y: u32, width: NonZeroU32, height: NonZeroU32 }`.

The traits are `pub(crate)`, so a backend cannot live outside the crate.

### 8.3 Pixel format and expected behaviour

- **Pixel format** (`src/lib.rs:170-188`): one `u32` per pixel, row-major from the top-left, `00000000RRRRRRRRGGGGGGGGBBBBBBBB`: "the highest order 8 bits are to be set to 0".
- In little-endian linear memory that is bytes B, G, R, X, i.e. XRGB8888. Native backends use it directly (`wl_shm::Format::Xrgb8888`, `backends/wayland/buffer.rs:102`; `DrmFourcc::Xrgb8888`, `backends/kms.rs:403`).
- Confirmed by the test: `0x005078ff` is stored as `[ff, 78, 50, 00]`.
- There is no alpha and no stride.
- **`Surface::resize(width, height)`** (`src/lib.rs:108-116`) sets the size of the buffer returned by `buffer_mut`. "If the size of the buffer does not match the size of the window, the buffer is drawn in the upper-left corner of the window."
- It must be called first. Backends panic with "Must set size of surface before calling ..." otherwise (`backends/web.rs:129`, `:273`, `:419`; `backends/win32.rs:274`).
- Unrepresentable sizes return `SoftBufferError::SizeOutOfRange` (`backends/x11.rs:324-325`).
- **`Surface::buffer_mut()`** (`:130-144`): "The initial contents of the buffer may be zeroed, or may contain a previous frame. Call `Buffer::age` to determine this."
- **`Buffer::age()`** (`:233-241`): `0` is a new buffer with unspecified contents, `1` is the last presented frame, `2` the one before. On Web it is `1` once presented and `0` after a resize (`backends/web.rs:406-412`).
- **`Buffer::present()`** (`:243-257`) shows the buffer. On Web, AppKit and UIKit it "must block copying image data" (`:197-200`).
- **`Buffer::present_with_damage(&[Rect])`** (`:259-272`): only the listed rectangles need to reach the screen. "Supported on: Wayland, X (XShm), Win32, Web. Otherwise this is equivalent to `present`". Out-of-range rectangles give `SoftBufferError::DamageOutOfRange` (`backends/x11.rs:477`, `backends/win32.rs:188`, `backends/wayland/mod.rs:132`).
- **`Surface::fetch()`** (`:118-128`) reads the window back; it is `Unimplemented` on most backends.
- **Thread safety.** `Context` is `Send + Sync` iff `D` is. `Surface` and `Buffer` are `Send` but not `Sync` (`src/lib.rs:353-377`, checked only `#[cfg(not(target_family = "wasm"))]` upstream).

### 8.4 How the Web backend maps to a canvas (`src/backends/web.rs`)

| Step | Code |
|---|---|
| Context | `WebDisplayImpl::new` requires `RawDisplayHandle::Web(..)`, then caches `web_sys::window().document()` (`:27-44`) |
| Surface | `RawWindowHandle::Web(handle)` resolves to `document.querySelector("canvas[data-raw-handle=\"{id}\"]")`. `WebCanvas` / `WebOffscreenCanvas` use the `JsValue` behind the pointer. Then `canvas.getContext("2d")` (`:217-244`, `:82-124`). |
| `resize` | reallocates a `Vec<u32>` and sets `canvas.width` / `canvas.height` (`:254-264`) |
| `present_with_damage` | takes the union of the damage rectangles, converts each `u32` to bytes `[R, G, B, 255]` (`:137-150`), builds an `ImageData`, and calls `putImageData(image, dx, dy, dirtyX, dirtyY, w, h)` once per rectangle (`:189-202`) |
| atomics | "When using atomics, the underlying memory becomes `SharedArrayBuffer`, which can't be shared with `ImageData`", so the bytes are first copied into a non-shared `Uint8Array` (`:157-180`) |
| `fetch` | `getImageData` and the inverse conversion (`:270-288`) |
| extras | `SurfaceExtWeb::from_canvas` / `from_offscreen_canvas` (`:292-328`) |

Two things carry over to a WASIX host running in a browser:

- The format conversion: softbuffer's bytes are B, G, R, X, and canvas wants R, G, B, A with A = 255.
- The shared-memory copy: WASIX memory is always shared (2.1). This is an inference; it was not run in a browser.

### 8.5 The WASIX skeleton backend

`softbuffer-wasix-skeleton.patch`: 5 files, +206 / -7.

| File | Change |
|---|---|
| `Cargo.toml` | Web dependency tables (lines 113, 117, 155) exclude WASIX: `cfg(all(target_family = "wasm", not(all(target_os = "wasi", target_vendor = "wasmer"))))`. Adds `[lints.rust] unexpected_cfgs = { level = "warn", check-cfg = ['cfg(target_vendor, values("wasmer"))'] }` so upstream toolchains do not warn. |
| `src/backends/mod.rs` | `mod wasix` for `all(target_os = "wasi", target_vendor = "wasmer")`; `mod web` excludes it |
| `src/backend_dispatch.rs` | new variant `Wasix(D, backends::wasix::WasixImpl<D, W>, backends::wasix::BufferImpl<'a, D, W>)`; `Web` excludes WASIX |
| `src/lib.rs` | `SurfaceExtWeb` re-export excludes WASIX; `__assert_send` is now also checked on WASIX |
| `src/backends/wasix.rs` (179 lines) | the backend |

Only WASIX is carved out of the Web cfg, rather than narrowing Web to `target_os = "unknown"`, so that other wasm targets keep compiling as before. A dispatch enum with zero backends does not compile: stock softbuffer on Linux with `--no-default-features` fails with 28 errors (checked).

What the skeleton does:

- The context is the display handle itself (blanket impl).
- `SurfaceInterface::new` accepts `RawWindowHandle::Web(handle)` and keeps `handle.id` as the host window id. Anything else is `InitError::Unsupported`; the test shows "unsupported platform: DRM, Wasix".
- `resize` reallocates a guest-side `Vec<u32>`, returning `SizeOutOfRange` on overflow.
- `buffer_mut`, `pixels`, `pixels_mut`, `width`, `height` and `age` are real.
- `present` / `present_with_damage` validate the damage rectangles (`DamageOutOfRange`) and then return `SoftBufferError::Unimplemented` at the one `TODO(wasmer_gui_v0)`.

Because the frame lives in guest linear memory, the host import can read the pixels in place.

Compile-time proof that the patched winit plugs in is in `wasix-test/src/bin/softbuffer_surface.rs`. `with_winit` builds `Context<winit::event_loop::OwnedDisplayHandle>` and `Surface<_, Rc<winit::window::Window>>`. It is never run, since no window can be created yet.

### 8.6 softbuffer `master` is changing this interface (read only, commit `9a7c9ca`)

From `CHANGELOG.md` "Unreleased" and `src/backend_interface.rs`:

- **Pixels.** "**Breaking:** Add `Pixel` struct, and use that for pixels instead of `u32`" and "The pixel format is now target-dependent".
- `Pixel` is `#[repr(C)]` with fields `r, g, b, a` on `any(target_family = "wasm", target_os = "android")`, and `b, g, r, a` elsewhere (`src/pixel.rs`).
- `PixelFormat::default()` is `Rgba8` on wasm / Android and `Bgra8` elsewhere (`src/format.rs`).
- WASIX is `target_family = "wasm"`, so it would get RGBA byte order.
- **Alpha.** `AlphaMode` (`Opaque` default, `Ignored`, `Premultiplied`, ..) comes with `SurfaceInterface::supports_alpha_mode`. `configure(width, height, alpha_mode)` replaces `resize`, and `next_buffer` replaces `buffer_mut`.
- **Buffer trait.** `BufferInterface` becomes `byte_stride`, `width`, `height`, `pixels_mut() -> &mut [Pixel]`, `age`, `present_with_damage`. There is no separate `present` and no `pixels`.
- **Removals.** `DamageOutOfRange` is removed (damage is clamped), and so is `SurfaceExtWeb`.
- **Selection unchanged.** The Web backend is still selected by `#[cfg(target_family = "wasm")]` (`src/backends/mod.rs:32`, `src/backend_dispatch.rs:274`).

A present import that takes an explicit pixel format, byte stride and alpha mode covers both 0.4.8 and the next release.

## 9. Surprises (complete list)

1. Default `cargo wasix build` cannot resolve winit 0.30 or softbuffer 0.4.8 (2.2).
2. `cargo wasix` edits the tracked `.cargo/config.toml` of the workspace it runs in.
3. WASIX is silently treated as the Web by stock softbuffer (run-time panic), stock winit 0.30 (builds the `wasm-bindgen` stack before `compile_error!`) and stock winit 0.31 (builds `winit-web`).
4. `target_vendor = "wasmer"` in a `#[cfg]` warns on every upstream toolchain.
5. softbuffer 0.4.8 has no `build.rs` or cfg aliases (removed in 0.4.7).
6. softbuffer's backend traits are `pub(crate)`.
7. winit 0.30's `create_window` has no "not supported" error, only `OsError`.
8. winit 0.30's Web backend no longer produces `RawWindowHandle::Web` under `rwh_06`.
9. `KeyEvent.text` on key release differs between backends.
10. Side-specific modifier state exists only on macOS and Orbital.
11. winit's wheel sign is the opposite of the DOM's.
12. winit 0.31's `winit-core` builds for WASIX unmodified, through the default overlay registry.
13. softbuffer `master` switches wasm targets to RGBA byte order and adds stride and alpha modes.
14. The patched winit compiled for WASIX on the first attempt; common code needed only one dead-code `allow`.
15. `std::env::consts::OS` is `""` on the wasix toolchain.

## 10. Biggest risks

1. **Registry conflict on the 0.30 line.** Combining the winit 0.30 fork with crates that need WASIX forks (`tokio`, `mio`, `socket2`, `libc`, ..) rules out `CARGO_WASIX_NO_REGISTRY_CONFIG=1`. The options are:
 - the overlay registry gains a `rustix 0.38.x`;
 - the fork drops its free-unix dependency table;
 - the work targets winit 0.31, which resolves today.
2. **Two moving targets.** winit 0.31 has a different backend shape and still changed between beta.3 and master. softbuffer's next release changes pixel format, stride and alpha. A 0.30 / 0.4.8 backend is a fork to carry. The base layer is better designed against the newer shapes: W3C key names, unified pointer events with id / kind / primary, explicit pixel format.
3. **Threading contract.** `Window` is `Send + Sync`, and every method may be called from any thread while the loop thread is blocked in the host.
4. **IME and text.** "No double insertion" is each backend's job. Dead keys and composition need the host to withhold composing keys.
5. **raw-window-handle has no WASIX variant.** Borrowing `Web` works for the two skeletons. Other crates that equate `target_family = "wasm"` with the browser will misread WASIX the way softbuffer did (wgpu, glutin: not checked).
6. **Frame transport in browsers.** WASIX memory is shared, so a 2D-canvas host would have to copy each presented frame out of it, and swizzle B, G, R, X to R, G, B, A for softbuffer 0.4. This is inferred, not run.

## 11. Not checked

- Anything involving a real window: there is no `wasmer_gui_v0` host. Window creation, input translation, redraw scheduling, `ScaleFactorChanged` and presentation are stubs or TODOs.
- Running in a browser (wasmer-js); only the native `wasmer 7.4.2` CLI was used.
- `wasm64-wasmer-wasi` (`cargo wasix build64`) and `wasm32-wasmer-wasi-dl`.
- Release builds, `cargo wasix test`, clippy, rustdoc, and winit's own tests and examples on WASIX.
- `rwh_04` / `rwh_05` paths: compiled, never executed.
- winit 0.31: only `winit-core` was built for WASIX; no 0.31 backend was written.
- softbuffer `master`: read only, not built.
- macOS dead-key and IME behaviour: source reading only.
- Whether other crates (wgpu, glutin, egui / eframe, ..) build or behave on WASIX.
- Whether the overlay registry will carry `rustix 0.38.x`.
- The W3C comparison used the spec repository at the commit listed at the top, not a dated TR.

## 12. Exact commands (from `build.sh`)

```bash
export CARGO_WASIX_NO_REGISTRY_CONFIG=1
git clone --depth 1 --branch v0.30.13 https://github.com/rust-windowing/winit.git winit
git -C winit apply --index ../winit-wasix-skeleton.patch
git clone --depth 1 --branch v0.4.8 https://github.com/rust-windowing/softbuffer.git softbuffer
git -C softbuffer apply --index ../softbuffer-wasix-skeleton.patch
(cd winit && cargo wasix build --lib)
(cd winit && cargo wasix build --lib --no-default-features)
(cd winit && cargo wasix build --lib --features rwh_04,rwh_05,rwh_06,serde,mint)
(cd softbuffer && cargo wasix build --lib)
(cd softbuffer && cargo wasix build --lib --no-default-features)
(cd wasix-test && cargo wasix build)
for bin in event_loop proxy_wait softbuffer_surface; do
~/.wasmer/bin/wasmer run wasix-test/target/wasm32-wasmer-wasi/debug/$bin.wasm
done
# regenerate the patches from the working trees:
git -C winit add -A && git -C winit diff --cached > winit-wasix-skeleton.patch
git -C softbuffer add -A && git -C softbuffer diff --cached > softbuffer-wasix-skeleton.patch
```
