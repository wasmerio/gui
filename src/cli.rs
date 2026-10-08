//! Runs a WASIX module with the GUI imports installed.
//!
//! This is the standalone runner behind the `gui_wasmer` binary and the
//! tests. Embedders do the same thing with their own runtime: build a
//! [`GuiCtx`] and register its hooks.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use virtual_fs::AsyncReadExt;
use wasmer::sys::{Cranelift, EngineBuilder, Features};
use wasmer::{Module, Store};
use wasmer_types::ModuleHash;
use wasmer_wasix::{
    Pipe, PluggableRuntime, WasiError, WasiRuntimeError,
    runners::wasi::{RuntimeOrEngine, WasiRunner},
    runtime::task_manager::tokio::TokioTaskManager,
};

use crate::script::{Player, Script};
use crate::{GuiCtx, GuiRuntimeHooks};

/// What a captured run produced.
#[derive(Debug, Clone)]
pub struct RunOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

fn create_store() -> Store {
    // wasixcc builds use the exception-handling proposal for setjmp/longjmp
    // and C++ exceptions.
    let mut features = Features::default();
    features.exceptions(true);
    let engine = EngineBuilder::new(Cranelift::default())
        .set_features(Some(features))
        .engine();
    Store::new(engine)
}

fn drain(
    mut pipe: Pipe,
    mut echo: Option<Box<dyn Write + Send>>,
) -> std::thread::JoinHandle<Result<String>> {
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("failed to create the stdio drain runtime")?;
        let mut captured = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let n = runtime
                .block_on(pipe.read(&mut chunk))
                .context("failed reading a WASIX stdio pipe")?;
            if n == 0 {
                break;
            }
            if let Some(echo) = echo.as_mut() {
                echo.write_all(&chunk[..n]).ok();
                echo.flush().ok();
            }
            captured.extend_from_slice(&chunk[..n]);
        }
        Ok(String::from_utf8_lossy(&captured).into_owned())
    })
}

/// What a run's guests are given besides WASIX.
#[derive(Clone)]
pub struct Hooks {
    gui: GuiRuntimeHooks,
    #[cfg(feature = "webgpu")]
    webgpu: Option<wasmer_webgpu::WebGpuRuntimeHooks>,
}

impl From<GuiRuntimeHooks> for Hooks {
    fn from(gui: GuiRuntimeHooks) -> Self {
        Self {
            gui,
            #[cfg(feature = "webgpu")]
            webgpu: None,
        }
    }
}

impl Hooks {
    /// The GUI, and WebGPU to draw with on its windows.
    #[cfg(feature = "webgpu")]
    pub fn with_webgpu(gui: GuiRuntimeHooks, webgpu: wasmer_webgpu::WebGpuRuntimeHooks) -> Self {
        Self {
            gui,
            webgpu: Some(webgpu),
        }
    }
}

fn run(
    hooks: Hooks,
    wasm_path: &Path,
    args: &[String],
    configure: impl FnOnce(&mut WasiRunner),
) -> Result<i32> {
    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to create the tokio runtime for WASIX")?;
    let _guard = tokio_runtime.enter();

    let wasm = std::fs::read(wasm_path)
        .with_context(|| format!("failed to read {}", wasm_path.display()))?;
    let store = create_store();
    let module = Module::new(&store, &wasm).context("failed to compile the wasm module")?;
    let module_hash = ModuleHash::sha256(&wasm);

    let mut runner = WasiRunner::new();
    runner.with_args(args.iter().cloned());
    configure(&mut runner);

    let task_manager = Arc::new(TokioTaskManager::new(tokio::runtime::Handle::current()));
    let mut runtime = PluggableRuntime::new(task_manager);
    runtime.set_engine(store.engine().clone());
    runtime.with_instantiation_hook(hooks.gui);
    #[cfg(feature = "webgpu")]
    if let Some(webgpu) = hooks.webgpu {
        runtime.with_instantiation_hook(webgpu);
    }

    let program = wasm_path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("guest");
    match runner.run_wasm(
        RuntimeOrEngine::Runtime(Arc::new(runtime)),
        program,
        module,
        module_hash,
    ) {
        Ok(()) => Ok(0),
        Err(error) => match exit_code(&error) {
            Some(code) => Ok(code),
            None => Err(error).context("failed to run the WASIX module"),
        },
    }
}

/// The guest's exit code, if `error` is a guest exiting rather than the run
/// failing. The runner wraps it at different depths depending on how the
/// process ended.
fn exit_code(error: &anyhow::Error) -> Option<i32> {
    error.chain().find_map(|cause| {
        if let Some(WasiError::Exit(code)) = cause.downcast_ref::<WasiError>() {
            return Some(i32::from(*code));
        }
        cause
            .downcast_ref::<WasiRuntimeError>()
            .and_then(WasiRuntimeError::as_exit_code)
            .map(i32::from)
    })
}

/// Runs `wasm_path` with the process's own stdio. Returns the exit code.
pub fn run_wasix_main(hooks: impl Into<Hooks>, wasm_path: &Path, args: &[String]) -> Result<i32> {
    run(hooks.into(), wasm_path, args, |runner| {
        runner
            .with_stdin(Box::new(virtual_fs::host_fs::Stdin::default()))
            .with_stdout(Box::new(virtual_fs::host_fs::Stdout::default()))
            .with_stderr(Box::new(virtual_fs::host_fs::Stderr::default()));
    })
}

/// Runs `wasm_path` and captures what it prints. With `echo`, the output is
/// also forwarded to this process's stdout and stderr as it arrives.
pub fn run_wasix_main_capture(
    hooks: impl Into<Hooks>,
    wasm_path: &Path,
    args: &[String],
    echo: bool,
) -> Result<RunOutput> {
    let hooks = hooks.into();
    let (stdout_tx, stdout_rx) = Pipe::channel();
    let (stderr_tx, stderr_rx) = Pipe::channel();
    let stdout = drain(
        stdout_rx,
        echo.then(|| Box::new(std::io::stdout()) as Box<dyn Write + Send>),
    );
    let stderr = drain(
        stderr_rx,
        echo.then(|| Box::new(std::io::stderr()) as Box<dyn Write + Send>),
    );
    let exit_code = run(hooks, wasm_path, args, |runner| {
        runner
            .with_stdout(Box::new(stdout_tx))
            .with_stderr(Box::new(stderr_tx));
    })?;
    let join = |handle: std::thread::JoinHandle<Result<String>>| {
        handle
            .join()
            .map_err(|_| anyhow::anyhow!("a stdio drain thread panicked"))?
    };
    Ok(RunOutput {
        exit_code,
        stdout: join(stdout)?,
        stderr: join(stderr)?,
    })
}

/// How long a guest may go on after its script before the run is given up.
const AFTER_SCRIPT: Duration = Duration::from_secs(20);

/// Runs `wasm_path` on a headless window system while `script` plays the
/// user, and captures what the guest prints.
///
/// The guest is expected to end by itself, usually because the script's last
/// step closes its window. One that is still waiting for input when the
/// script is over is stopped, and that is an error.
pub fn run_scripted(
    wasm_path: &Path,
    args: &[String],
    script: Script,
    echo: bool,
) -> Result<RunOutput> {
    run_scripted_into(wasm_path, args, script, echo, None)
}

/// [`run_scripted`], with a directory for the script's `screenshot` steps to
/// write what the guest's windows show.
pub fn run_scripted_into(
    wasm_path: &Path,
    args: &[String],
    script: Script,
    echo: bool,
    screenshots: Option<&Path>,
) -> Result<RunOutput> {
    let system = script.system()?;
    let ctx = GuiCtx::builder()
        .window_system(Arc::new(system.clone()))
        .policy(script.policy()?)
        .limits(script.limits())
        .build();
    let scope = ctx.scope();
    let exited = Arc::new(AtomicBool::new(false));

    let user = {
        let (scope, exited) = (scope.clone(), Arc::clone(&exited));
        let screenshots = screenshots.map(Path::to_path_buf);
        let paced = system.options().redraws == crate::headless::HeadlessRedraws::Paced;
        std::thread::spawn(move || -> Result<()> {
            let mut player = Player::new(&system).screenshots(screenshots);
            let played = player.play(&script.steps).and_then(|()| {
                let over = Instant::now();
                loop {
                    if exited.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    if system.is_idle() {
                        if !paced {
                            return Err(anyhow!(
                                "the script is over and the guest is still waiting for input"
                            ));
                        }
                        // A guest that draws frame after frame waits for the
                        // display, not for its user: the display goes on.
                        system.advance(system.frame_interval().as_nanos() as u64);
                    }
                    if over.elapsed() >= AFTER_SCRIPT {
                        return Err(anyhow!("the guest did not end after the script"));
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            });
            if played.is_err() {
                // Whatever the guest is doing, it must not outlive the run.
                scope.close();
            }
            played
        })
    };

    let output = run_wasix_main_capture(scope.hooks(), wasm_path, args, echo);
    exited.store(true, Ordering::Release);
    user.join()
        .map_err(|_| anyhow!("the script player panicked"))??;
    output
}
