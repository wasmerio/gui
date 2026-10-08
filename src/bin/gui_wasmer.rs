//! `gui_wasmer [--window | --script <file.json> [--screenshots <directory>]]
//! <module.wasm> [args...]`:
//! runs a WASIX module with the GUI imports.
//!
//! A small stand-in for `wasmer run` that this repository can build and test
//! on its own. Windows are headless unless asked for: they work, and nobody
//! sees them. With a script, its steps play the user (see
//! `wasmer_gui::script`). Built with the `winit` feature, `--window` opens
//! real ones.

use std::path::PathBuf;
use std::process::ExitCode;

use wasmer_gui::GuiCtx;
use wasmer_gui::script::Script;

const USAGE: &str = "usage: gui_wasmer [--window [--inactive] | --script <file.json> [--screenshots \
                     <directory>]] [--webgpu] <module.wasm> [args...]";

struct Options {
    wasm: PathBuf,
    args: Vec<String>,
    script: Option<PathBuf>,
    /// Where the script's `screenshot` steps write.
    screenshots: Option<PathBuf>,
    #[cfg_attr(not(feature = "winit"), allow(dead_code))]
    window: bool,
    #[cfg_attr(not(feature = "winit"), allow(dead_code))]
    inactive: bool,
    #[cfg_attr(not(feature = "webgpu"), allow(dead_code))]
    webgpu: bool,
}

fn parse() -> Result<Options, String> {
    let mut args = std::env::args().skip(1);
    let mut script = None;
    let mut screenshots = None;
    let mut window = false;
    let mut inactive = false;
    let mut webgpu = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--script" => script = Some(PathBuf::from(args.next().ok_or(USAGE)?)),
            "--screenshots" => screenshots = Some(PathBuf::from(args.next().ok_or(USAGE)?)),
            "--headless" => window = false,
            "--window" if cfg!(feature = "winit") => window = true,
            "--window" => {
                return Err(
                    "gui_wasmer was built without windows; rebuild with `--features winit`"
                        .to_owned(),
                );
            }
            // Windows open behind what the user is working in.
            "--inactive" => inactive = true,
            "--webgpu" if cfg!(feature = "webgpu") => webgpu = true,
            "--webgpu" => {
                return Err(
                    "gui_wasmer was built without WebGPU; rebuild with `--features webgpu`"
                        .to_owned(),
                );
            }
            "-h" | "--help" => return Err(USAGE.to_owned()),
            _ => {
                return Ok(Options {
                    wasm: PathBuf::from(arg),
                    args: args.collect(),
                    script,
                    screenshots,
                    window,
                    inactive,
                    webgpu,
                });
            }
        }
    }
    Err(USAGE.to_owned())
}

#[cfg(feature = "winit")]
fn run_in_windows(options: &Options) -> anyhow::Result<i32> {
    use std::sync::Arc;
    use std::time::Duration;
    use wasmer_gui::native::{CloseBehavior, GuiExit, GuiMainThread, NativeOptions};

    let (main_thread, windows) = GuiMainThread::with_options(NativeOptions {
        // A guest that does not answer the close button is taken to be stuck.
        close: CloseBehavior::RequestThenEnd(Duration::from_secs(2)),
        activate: !options.inactive,
    });
    #[cfg(feature = "webgpu")]
    let webgpu = options.webgpu.then(|| {
        wasmer_webgpu::WebGpuCtx::builder()
            .surface_provider(windows.surfaces(None))
            .build()
    });
    let ctx = GuiCtx::builder().window_system(Arc::new(windows)).build();
    let hooks = wasmer_gui::cli::Hooks::from(ctx.runtime_hooks());
    #[cfg(feature = "webgpu")]
    let hooks = match &webgpu {
        Some(webgpu) => {
            wasmer_gui::cli::Hooks::with_webgpu(ctx.runtime_hooks(), webgpu.runtime_hooks())
        }
        None => hooks,
    };
    let (wasm, args) = (options.wasm.clone(), options.args.clone());
    match main_thread.run(move || wasmer_gui::cli::run_wasix_main(hooks, &wasm, &args))? {
        GuiExit::Finished(code) => code,
        GuiExit::Closed => {
            ctx.runtime_control().terminate_all();
            #[cfg(feature = "webgpu")]
            if let Some(webgpu) = &webgpu {
                webgpu.runtime_control().terminate_all();
            }
            Ok(0)
        }
    }
}

fn run(options: &Options) -> anyhow::Result<i32> {
    #[cfg(feature = "winit")]
    if options.window {
        return run_in_windows(options);
    }
    match &options.script {
        Some(path) => {
            let script = Script::load(path)?;
            let output = wasmer_gui::cli::run_scripted_into(
                &options.wasm,
                &options.args,
                script,
                true,
                options.screenshots.as_deref(),
            )?;
            Ok(output.exit_code)
        }
        None => {
            #[cfg(feature = "webgpu")]
            if options.webgpu {
                // Drawn, and seen by nobody.
                let windows = wasmer_gui::headless::HeadlessSystem::default();
                let webgpu = wasmer_webgpu::WebGpuCtx::builder()
                    .surface_provider(windows.surfaces(None))
                    .build();
                let ctx = GuiCtx::builder()
                    .window_system(std::sync::Arc::new(windows))
                    .build();
                let hooks = wasmer_gui::cli::Hooks::with_webgpu(
                    ctx.runtime_hooks(),
                    webgpu.runtime_hooks(),
                );
                return wasmer_gui::cli::run_wasix_main(hooks, &options.wasm, &options.args);
            }
            let ctx = GuiCtx::default();
            wasmer_gui::cli::run_wasix_main(ctx.runtime_hooks(), &options.wasm, &options.args)
        }
    }
}

fn main() -> ExitCode {
    let options = match parse() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    match run(&options) {
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(error) => {
            eprintln!("gui_wasmer: {error:#}");
            ExitCode::FAILURE
        }
    }
}
