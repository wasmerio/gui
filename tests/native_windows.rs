//! The test programs that need nobody at the keyboard, in real windows.
//!
//! A window system's loop wants the main thread, which the test harness
//! keeps for itself, so this file has a `main` of its own. It opens windows
//! on the screen, so it only runs when `WGUI_TEST_WINDOWS` is set
//! (`make test-windows`); the windows open behind whatever is in front.
//!
//! What a program prints here must be what it prints on the headless window
//! system (`tests/expected`): that the two agree is the test.
//!
//! Built with the `webgpu` feature, a program also draws on a window with
//! the GPU (`make gpu-programs`). A window nobody can see has no frames to
//! give (it is covered, or the screen is locked): the program says so, and
//! that part is reported as skipped, not as passed.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use wasmer_gui::GuiCtx;
use wasmer_gui::cli::{RunOutput, run_wasix_main_capture};
use wasmer_gui::native::{CloseBehavior, GuiExit, GuiMainThread, NativeOptions};

/// The programs that drive themselves.
const PROGRAMS: &[&str] = &[
    "window_smoke",
    "threads",
    "newer",
    "misuse",
    // Draws without a GPU, which a build without the `pixels` feature cannot
    // show: there the program says so, and that is not what is expected.
    #[cfg(feature = "pixels")]
    "pixels",
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests")
}

/// What the program that draws prints when its frames reached the window.
#[cfg(feature = "webgpu")]
const DRAWN: &str = "drawing on the window\npresented 3 frames\ndone\n";

struct Ran {
    name: String,
    expected: String,
    output: anyhow::Result<RunOutput>,
}

fn run_all(ctx: GuiCtx, #[cfg(feature = "webgpu")] webgpu: wasmer_webgpu::WebGpuCtx) -> Vec<Ran> {
    let mut ran: Vec<Ran> = PROGRAMS
        .iter()
        .map(|name| {
            let wasm = root().join("programs/out").join(format!("{name}.wasm"));
            // A scope each, as an embedder that runs several programs does.
            let scope = ctx.scope();
            Ran {
                name: (*name).to_owned(),
                expected: std::fs::read_to_string(
                    root().join("expected").join(format!("{name}.txt")),
                )
                .expect("the expected output"),
                output: run_wasix_main_capture(scope.hooks(), &wasm, &[], false),
            }
        })
        .collect();
    #[cfg(feature = "webgpu")]
    {
        let scope = ctx.scope();
        let hooks = wasmer_gui::cli::Hooks::with_webgpu(scope.hooks(), webgpu.runtime_hooks());
        let wasm = root().join("programs/gpu/out/surface.wasm");
        ran.push(Ran {
            name: "gpu/surface".to_owned(),
            expected: DRAWN.to_owned(),
            output: run_wasix_main_capture(hooks, &wasm, &[], false),
        });
    }
    ran
}

fn main() -> ExitCode {
    if std::env::var_os("WGUI_TEST_WINDOWS").is_none() {
        println!("skipped: set WGUI_TEST_WINDOWS=1 to open real windows");
        return ExitCode::SUCCESS;
    }
    let (main_thread, windows) = GuiMainThread::with_options(NativeOptions {
        close: CloseBehavior::RequestThenEnd(Duration::from_secs(2)),
        activate: false,
    });
    #[cfg(feature = "webgpu")]
    let webgpu = wasmer_webgpu::WebGpuCtx::builder()
        .force_fallback_adapter(std::env::var_os("WEBGPU_TEST_FALLBACK_ADAPTER").is_some())
        .surface_provider(windows.surfaces(None))
        .build();
    let ctx = GuiCtx::builder().window_system(Arc::new(windows)).build();
    let results = match main_thread.run(move || {
        run_all(
            ctx,
            #[cfg(feature = "webgpu")]
            webgpu,
        )
    }) {
        Ok(GuiExit::Finished(results)) => results,
        Ok(GuiExit::Closed) => {
            eprintln!("a window was closed by hand; the run was abandoned");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("the window system failed: {error:#}");
            return ExitCode::FAILURE;
        }
    };

    let mut failed = false;
    for Ran {
        name,
        expected,
        output,
    } in results
    {
        match output {
            Ok(output) if output.exit_code == 0 && output.stdout == expected => {
                println!("ok      {name}");
            }
            // The program's own word for a window that has no frames to give.
            Ok(output) if output.exit_code == 0 && output.stdout.contains("\nunseen: ") => {
                println!(
                    "skipped {name}: its window cannot be seen (is the screen locked, or the \
                     window covered?), so nothing was presented"
                );
            }
            Ok(output) => {
                failed = true;
                println!(
                    "FAILED  {name}: exit code {}\n--- printed\n{}--- instead of\n{expected}--- stderr\n{}",
                    output.exit_code, output.stdout, output.stderr
                );
            }
            Err(error) => {
                failed = true;
                println!("FAILED  {name}: {error:#}");
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
