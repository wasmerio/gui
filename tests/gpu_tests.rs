//! Windows drawn on with WebGPU, on the headless window system: what a guest
//! presents arrives at the window it named.
//!
//! Needs the programs of `make gpu-programs examples` and a GPU adapter. On a
//! machine without one, `WEBGPU_TEST_FALLBACK_ADAPTER=1` asks for a software
//! adapter (lavapipe, WARP) instead.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use wasmer_gui::GuiCtx;
use wasmer_gui::cli::{Hooks, RunOutput, run_wasix_main_capture};
use wasmer_gui::headless::{HeadlessOptions, HeadlessSystem, HeadlessWindow};
use wasmer_webgpu::WebGpuCtx;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// Runs `wasm` with a GUI and WebGPU on `system`, and returns what it
/// printed with every window it opened.
fn run(system: &HeadlessSystem, wasm: &Path, args: &[&str]) -> (RunOutput, Vec<HeadlessWindow>) {
    assert!(
        wasm.exists(),
        "{} is missing: build it with `make gpu-programs examples`",
        wasm.display()
    );
    let opened = Arc::new(Mutex::new(Vec::new()));
    {
        let opened = Arc::clone(&opened);
        system.on_window(move |window| opened.lock().expect("not poisoned").push(window.clone()));
    }
    let webgpu = WebGpuCtx::builder()
        .force_fallback_adapter(std::env::var_os("WEBGPU_TEST_FALLBACK_ADAPTER").is_some())
        .surface_provider(system.surfaces(None))
        .build();
    let gui = GuiCtx::builder()
        .window_system(Arc::new(system.clone()))
        .build();
    let args: Vec<String> = args.iter().map(|&arg| arg.to_owned()).collect();
    let output = run_wasix_main_capture(
        Hooks::with_webgpu(gui.runtime_hooks(), webgpu.runtime_hooks()),
        wasm,
        &args,
        false,
    )
    .expect("the module runs");
    // The run is over when the program has ended; what it held goes when its
    // threads have left the hosts, which is a moment later.
    let ended = std::time::Instant::now();
    while gui.usage().windows != 0 || webgpu.usage().processes != 0 {
        assert!(
            ended.elapsed() < std::time::Duration::from_secs(5),
            "the guest left {:?} and {:?} behind",
            gui.usage(),
            webgpu.usage()
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let opened = opened.lock().expect("not poisoned").clone();
    (output, opened)
}

#[test]
fn frames_arrive_at_the_window_they_were_drawn_on() {
    let system = HeadlessSystem::new(HeadlessOptions {
        size: (64, 48),
        capture_frames: true,
        ..HeadlessOptions::default()
    });
    let (output, windows) = run(
        &system,
        &root().join("tests/programs/gpu/out/surface.wasm"),
        &[],
    );
    assert_eq!(output.exit_code, 0, "{}{}", output.stdout, output.stderr);
    assert_eq!(
        output.stdout,
        "drawing on the window\npresented 3 frames\ndone\n"
    );

    let [window] = windows.as_slice() else {
        panic!("the guest opened {} windows", windows.len());
    };
    assert!(!window.is_alive());
    assert_eq!(window.frames_presented(), 3);
    // The program asks for 320 by 200 in logical units, on a display with a
    // scale factor of one. Its last frame is blue.
    let frame = window.last_frame().expect("frames are kept");
    assert_eq!((frame.width, frame.height), (320, 200));
    assert_eq!(frame.pixels.len(), 320 * 200 * 4);
    let blue: [u8; 4] = if frame.bgra {
        [255, 0, 0, 255]
    } else {
        [0, 0, 255, 255]
    };
    assert!(
        frame
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| *pixel == blue),
        "the last frame is not all blue: it starts with {:?}",
        &frame.pixels[..8]
    );
}

#[test]
fn a_scaled_display_gives_a_bigger_surface() {
    let system = HeadlessSystem::new(HeadlessOptions {
        scale_factor: 2.0,
        capture_frames: true,
        ..HeadlessOptions::default()
    });
    let (output, windows) = run(
        &system,
        &root().join("tests/programs/gpu/out/surface.wasm"),
        &[],
    );
    assert_eq!(output.exit_code, 0, "{}{}", output.stdout, output.stderr);
    let frame = windows[0].last_frame().expect("frames are kept");
    assert_eq!((frame.width, frame.height), (640, 400));
}

#[test]
fn the_example_draws_its_frames() {
    let system = HeadlessSystem::default();
    let (output, windows) = run(&system, &root().join("examples/out/triangle.wasm"), &["5"]);
    assert_eq!(output.exit_code, 0, "{}{}", output.stdout, output.stderr);
    assert_eq!(output.stdout, "presented 5 frames at 640x480\n");
    assert_eq!(windows[0].frames_presented(), 5);
    // Frames nobody asked to keep are counted and let go.
    assert!(
        windows[0]
            .last_frame()
            .is_some_and(|frame| frame.pixels.is_empty())
    );
}
