//! Runs test programs on the headless window system, each with the script
//! that plays its user, and compares what it prints with what it should.
//! The same programs and scripts are what other hosts are held to.
//!
//! The programs of `tests/programs` use the API directly, and are built by
//! `make programs`. The ones of `integrations/sdl3/tests` use SDL3 on its
//! "wasmer" video driver, and need SDL built first (`make sdl-programs`);
//! the one of `integrations/winit/tests` uses winit on its WASIX backend
//! (`make winit-programs`).
//!
//! `WGUI_BLESS=1` rewrites the expected output from what the programs print
//! now, for review.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use wasmer_gui::GuiCtx;
use wasmer_gui::cli::{RunOutput, run_scripted, run_wasix_main_capture};
use wasmer_gui::script::Script;

#[derive(Deserialize)]
struct Manifest {
    runs: Vec<Run>,
}

#[derive(Deserialize)]
struct Run {
    name: String,
    program: String,
    script: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    /// The status the program ends with: 0 unless it says otherwise.
    #[serde(default)]
    exit: i32,
}

/// A directory of programs to run: its `manifest.json`, `scripts` and
/// `expected`, where the built programs are, and how they get there.
struct Suite {
    root: PathBuf,
    programs: PathBuf,
    built_by: &'static str,
}

impl Suite {
    fn play(&self, run: &Run) -> RunOutput {
        let wasm = self.programs.join(format!("{}.wasm", run.program));
        assert!(
            wasm.exists(),
            "{} is missing: build the test programs with `{}`",
            wasm.display(),
            self.built_by
        );
        let output = match &run.script {
            Some(script) => {
                let script =
                    Script::load(&self.root.join("scripts").join(format!("{script}.json")))
                        .unwrap_or_else(|error| panic!("{}: {error:#}", run.name));
                run_scripted(&wasm, &run.args, script, false)
            }
            None => {
                run_wasix_main_capture(GuiCtx::default().runtime_hooks(), &wasm, &run.args, false)
            }
        };
        output.unwrap_or_else(|error| panic!("{}: {error:#}", run.name))
    }

    fn check(&self) {
        let manifest: Manifest = serde_json::from_str(
            &std::fs::read_to_string(self.root.join("manifest.json")).expect("the manifest"),
        )
        .expect("a valid manifest");
        let bless = std::env::var_os("WGUI_BLESS").is_some();
        let mut failures = Vec::new();
        for run in &manifest.runs {
            let output = self.play(run);
            let expected_path = self.root.join("expected").join(format!("{}.txt", run.name));
            if output.exit_code != run.exit {
                failures.push(format!(
                    "{}: exit code {} instead of {}\n--- stdout\n{}--- stderr\n{}",
                    run.name, output.exit_code, run.exit, output.stdout, output.stderr
                ));
                continue;
            }
            if bless {
                std::fs::write(&expected_path, &output.stdout).expect("writes");
                continue;
            }
            let expected = std::fs::read_to_string(&expected_path).unwrap_or_else(|_| {
                panic!(
                    "{} is missing: run with WGUI_BLESS=1 and review it",
                    expected_path.display()
                )
            });
            if output.stdout != expected {
                failures.push(format!(
                    "{}: printed\n{}--- instead of\n{}--- stderr\n{}",
                    run.name, output.stdout, expected, output.stderr
                ));
            }
        }
        assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    }
}

#[test]
fn programs_print_what_they_should() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    Suite {
        programs: root.join("programs/out"),
        root,
        built_by: "make programs",
    }
    .check();
}

/// SDL3 is a port: these say whether its "wasmer" video driver turns what
/// the host tells of into the SDL events and calls a program expects.
#[test]
#[ignore = "needs SDL3 built for WASIX: `make test-sdl` builds it and runs this"]
fn sdl_programs_print_what_they_should() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("integrations/sdl3/tests");
    Suite {
        programs: root.join("out"),
        root,
        built_by: "make sdl-programs",
    }
    .check();
}

/// The same for winit: whether its WASIX backend turns the host's records
/// into the events and answers a winit program expects.
#[test]
#[ignore = "needs winit built for WASIX: `make test-winit` builds it and runs this"]
fn winit_programs_print_what_they_should() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("integrations/winit/tests");
    Suite {
        programs: root.join("out"),
        root,
        built_by: "make winit-programs",
    }
    .check();
}
