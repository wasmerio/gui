//! What an embedder relies on: that a guest can always be stopped, and that
//! what it leaves behind goes away with it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wasmer_gui::GuiCtx;
use wasmer_gui::cli::run_wasix_main_capture;
use wasmer_gui::headless::{HeadlessOptions, HeadlessSystem};

fn program(name: &str) -> PathBuf {
    let wasm = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/programs/out")
        .join(format!("{name}.wasm"));
    assert!(
        wasm.exists(),
        "{} is missing: build the test programs with `make programs`",
        wasm.display()
    );
    wasm
}

#[test]
fn a_guest_waiting_for_input_stops_when_its_scope_closes() {
    let system = HeadlessSystem::default();
    let ctx = GuiCtx::builder()
        .window_system(Arc::new(system.clone()))
        .build();
    let scope = ctx.scope();
    let hooks = scope.hooks();
    let guest =
        std::thread::spawn(move || run_wasix_main_capture(hooks, &program("events"), &[], false));

    assert!(
        system.wait_idle(Duration::from_secs(20)),
        "the guest opens its window and waits"
    );
    assert_eq!(ctx.usage().windows, 1);
    let closed = Instant::now();
    scope.close();
    let outcome = guest.join().expect("the runner does not panic");
    let took = closed.elapsed();
    let error = format!(
        "{:#}",
        outcome.expect_err("a stopped guest does not exit by itself")
    );
    assert!(error.contains("stopped"), "{error}");
    assert!(
        took < Duration::from_millis(200),
        "it took {took:?} to stop"
    );
    // Nothing of it is left.
    assert!(system.windows().is_empty());
    assert_eq!(ctx.usage().windows, 0);
}

#[test]
fn a_guest_killed_while_it_waits_lets_go() {
    // The guest kills itself from a second thread while its first one waits
    // for input without ever making a system call.
    let ctx = GuiCtx::default();
    let outcome = run_wasix_main_capture(ctx.runtime_hooks(), &program("killed"), &[], false);
    match outcome {
        Ok(output) => {
            assert!(output.stdout.contains("killing"), "{}", output.stdout);
            assert_ne!(output.exit_code, 0, "a killed program did not succeed");
        }
        Err(error) => {
            let error = format!("{error:#}");
            assert!(error.contains("stopped"), "{error}");
        }
    }
    // The run is over either way: a killed process is reported as ended
    // whatever its threads are doing. What tells is whether the thread that
    // was waiting came out of the host, and with it what the guest held.
    let ended = Instant::now();
    while ctx.usage().instances != 0 {
        assert!(
            ended.elapsed() < Duration::from_millis(500),
            "the killed guest still holds {:?}",
            ctx.usage()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn what_a_guest_leaves_open_goes_when_it_exits() {
    // `misuse` and `newer` end with windows they never destroyed.
    let system = HeadlessSystem::default();
    let ctx = GuiCtx::builder()
        .window_system(Arc::new(system.clone()))
        .build();
    let scope = ctx.scope();
    let output =
        run_wasix_main_capture(scope.hooks(), &program("newer"), &[], false).expect("runs");
    assert_eq!(output.exit_code, 0, "{}", output.stderr);
    assert!(system.windows().is_empty());
    assert_eq!(ctx.usage().instances, 0);
    assert!(!scope.is_closed());
}

#[test]
fn what_a_guest_draws_itself_is_what_its_window_holds() {
    let system = HeadlessSystem::new(HeadlessOptions {
        scale_factor: 2.0,
        capture_frames: true,
        ..HeadlessOptions::default()
    });
    let opened = Arc::new(Mutex::new(Vec::new()));
    {
        let opened = Arc::clone(&opened);
        system.on_window(move |window| opened.lock().expect("not poisoned").push(window.clone()));
    }
    let ctx = GuiCtx::builder()
        .window_system(Arc::new(system.clone()))
        .build();
    let output = run_wasix_main_capture(ctx.runtime_hooks(), &program("pixels"), &[], false)
        .expect("the program runs");
    assert_eq!(output.exit_code, 0, "{}{}", output.stdout, output.stderr);

    let windows = opened.lock().expect("not poisoned").clone();
    let [window] = windows.as_slice() else {
        panic!("the program opened {} windows", windows.len());
    };
    // The first frame, the same in another layout, a corner of it, and one
    // when asked. What changed nothing was not shown.
    assert_eq!(window.frames_presented(), 4);
    // 400 by 250 logical units, two pixels each.
    let frame = window.last_frame().expect("frames are kept");
    assert_eq!((frame.width, frame.height), (800, 500));
    let at = |x: u32, y: u32| {
        let start = ((y * frame.width + x) * 4) as usize;
        <[u8; 4]>::try_from(&frame.pixels[start..start + 4]).expect("four bytes")
    };
    // The pattern: red grows to the right, green downward, blue is 0x40,
    // and the corner that was presented last is white. Nothing is
    // transparent, though the program's alpha bytes said so, and none of
    // the padding between its rows shows.
    assert_eq!(at(0, 0), [255, 255, 255, 255]);
    assert_eq!(at(15, 15), [255, 255, 255, 255]);
    assert_eq!(at(16, 0), [5, 0, 0x40, 255]);
    assert_eq!(at(799, 0), [255, 0, 0x40, 255]);
    assert_eq!(at(0, 499), [0, 255, 0x40, 255]);
    assert_eq!(at(799, 499), [255, 255, 0x40, 255]);
    assert_eq!(at(400, 250), [127, 127, 0x40, 255]);
}
