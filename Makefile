# Everyday tasks. Nothing here is needed to use the crate from Rust: cargo
# builds it. The header checks and the test programs need wasixcc
# (https://github.com/wasix-org/wasixcc).

CARGO ?= cargo
PYTHON ?= python3
WASIXCC ?= wasixcc
WASIXCXX ?= wasix++
ifeq ($(shell command -v $(WASIXCC) 2>/dev/null),)
WASIXCC := $(HOME)/.wasixcc/bin/wasixcc
WASIXCXX := $(HOME)/.wasixcc/bin/wasix++
endif

CFLAGS := --target=wasm32-wasix -Wall -Wextra -Werror -Iinclude
OUT := tests/programs/out
PROGRAMS := $(patsubst tests/programs/%.c,$(OUT)/%.wasm,$(wildcard tests/programs/*.c)) $(OUT)/link_all.wasm
EXAMPLES := $(patsubst examples/%.c,examples/out/%.wasm,$(wildcard examples/*.c))

.PHONY: all gen check check-header check-winit check-web check-sys fmt lint programs gpu-programs examples test test-loom test-gpu test-windows test-page test-web-wait sdl sdl-programs test-sdl imgui-demo test-imgui winit-programs test-winit clean

all: programs examples

## Regenerate everything derived from spec/ (the results are checked in).
gen:
	$(PYTHON) codegen/gen.py

## What CI holds a change to, short of running programs.
check: check-header check-winit check-web check-sys lint
	$(PYTHON) codegen/gen.py --check
	$(CARGO) fmt --check

## The header against the C compiler: it has to be valid C99, C11 and C++
## from 11 on, and to lay structs out the way the host decodes them.
check-header:
	@mkdir -p $(OUT)/check
	$(WASIXCC) $(CFLAGS) -std=c11 -c guest/tests/abi_check.gen.c -o $(OUT)/check/abi_check.o
	$(WASIXCC) $(CFLAGS) -std=c99 -pedantic -c guest/tests/header_use.gen.c -o $(OUT)/check/header_use_c99.o
	$(WASIXCC) $(CFLAGS) -std=c11 -pedantic -c guest/tests/header_use.gen.c -o $(OUT)/check/header_use_c11.o
	$(WASIXCXX) $(CFLAGS) -x c++ -std=c++11 -pedantic -c guest/tests/header_use.gen.c -o $(OUT)/check/header_use_cxx11.o
	$(WASIXCXX) $(CFLAGS) -x c++ -std=c++17 -pedantic -c guest/tests/header_use.gen.c -o $(OUT)/check/header_use_cxx17.o

## winit is one window system among several: nothing outside src/native may
## come to depend on it.
check-winit:
	@if grep -rn "winit::" src --include='*.rs' | grep -v "^src/native/"; then \
		echo "winit is used outside src/native"; exit 1; \
	fi

## The browser half of the crate, as the Wasmer SDK compiles it, and the page
## half as a JavaScript engine reads it.
check-web:
	$(CARGO) clippy --target wasm32-unknown-unknown --no-default-features --features js,wasix -- -D warnings
	node --check js/gui_page.js

## The header for Rust (sys/): lints, and its layout, which the compiler
## confirms where pointers are 32 bits wide.
check-sys:
	$(CARGO) clippy --manifest-path sys/Cargo.toml -- -D warnings
	$(CARGO) check --manifest-path sys/Cargo.toml --target wasm32-unknown-unknown

fmt:
	$(CARGO) fmt

lint:
	$(CARGO) clippy --features cli --all-targets -- -D warnings
	$(CARGO) clippy --features cli,winit,clipboard,webgpu,pixels --all-targets -- -D warnings

## The test programs: tests/programs/out/*.wasm
programs: $(PROGRAMS)

$(OUT)/link_all.wasm: guest/tests/link_all.gen.c include/wasmer/gui.h
	@mkdir -p $(OUT)
	$(WASIXCC) $(CFLAGS) -O2 $< -o $@

$(OUT)/%.wasm: tests/programs/%.c include/wasmer/gui.h $(wildcard tests/programs/*.h)
	@mkdir -p $(OUT)
	$(WASIXCC) $(CFLAGS) -O2 -Itests/programs $< -o $@

## The examples, and the test programs under tests/programs/gpu, draw with
## WebGPU: they need wasmer-webgpu's header and its guest library (`make
## guest` there).
WEBGPU ?= ../webgpu
WEBGPU_FLAGS := -I$(WEBGPU)/include -L$(WEBGPU)/lib/wasm32-wasix -lwebgpu
# Not under $(OUT): the rule for the programs there would claim these too.
GPU_OUT := tests/programs/gpu/out
GPU_PROGRAMS := $(patsubst tests/programs/gpu/%.c,$(GPU_OUT)/%.wasm,$(wildcard tests/programs/gpu/*.c))

examples: $(EXAMPLES)

examples/out/%.wasm: examples/%.c include/wasmer/gui.h $(WEBGPU)/lib/wasm32-wasix/libwebgpu.a
	@mkdir -p examples/out
	$(WASIXCC) $(CFLAGS) -O2 $< $(WEBGPU_FLAGS) -o $@

gpu-programs: $(GPU_PROGRAMS)

$(GPU_OUT)/%.wasm: tests/programs/gpu/%.c include/wasmer/gui.h $(wildcard tests/programs/*.h) $(WEBGPU)/lib/wasm32-wasix/libwebgpu.a
	@mkdir -p $(GPU_OUT)
	$(WASIXCC) $(CFLAGS) -O2 -Itests/programs $< $(WEBGPU_FLAGS) -o $@

$(WEBGPU)/lib/wasm32-wasix/libwebgpu.a:
	$(MAKE) -C $(WEBGPU) guest

## Unit tests, the ABI sweep and the test programs on the headless host.
test: programs
	$(CARGO) test --features cli

## The handshake between waiting and waking (src/host/wait.rs) under loom,
## in every interleaving and with every store each load may read. The first
## run builds the dependencies again, with `--cfg loom`.
test-loom:
	RUSTFLAGS="--cfg loom" $(CARGO) test --release --lib -- wait::model

## Windows drawn on with WebGPU, on the headless host. Needs a GPU adapter;
## WEBGPU_TEST_FALLBACK_ADAPTER=1 asks for a software one.
test-gpu: gpu-programs examples
	$(CARGO) test --features cli,webgpu --test gpu_tests

## The same programs in real windows. They open on the screen for a moment,
## behind what you are working in; under CI, on a virtual display.
test-windows: programs gpu-programs
	WGUI_TEST_WINDOWS=1 $(CARGO) test --features cli,winit,webgpu,pixels --test native_windows

## The page half (js/gui_page.js) in the installed Chrome, with a core that
## writes down what it is told.
test-page:
	cd tests/browser && npm install --no-audit --no-fund && npm test

## The wait that suspends (`GuiCtxBuilder::suspending_waits`), across
## Wasmer's import boundary (tests/web_wait.rs): built for wasm32 with shared
## memory, which takes nightly and rust-src, and run by wasm-bindgen's test
## runner in Node, which needs JSPI. The runner's version has to be the one
## of wasm-bindgen in Cargo.lock.
CARGO_NIGHTLY ?= $(CARGO) +nightly
WASM_BINDGEN_TEST_RUNNER ?= wasm-bindgen-test-runner
WEB_WAIT_RUSTFLAGS := -C target-feature=+atomics,+bulk-memory,+mutable-globals \
	-C link-arg=--shared-memory -C link-arg=--import-memory \
	-C link-arg=--max-memory=1073741824 -C link-arg=--export=__wasm_init_tls \
	-C link-arg=--export=__tls_size -C link-arg=--export=__tls_align \
	-C link-arg=--export=__tls_base

test-web-wait:
	RUSTFLAGS="$(WEB_WAIT_RUSTFLAGS)" \
	CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER="$(WASM_BINDGEN_TEST_RUNNER)" \
	$(CARGO_NIGHTLY) test -Z build-std=std,panic_abort --target wasm32-unknown-unknown \
		--target-dir target/web-wait --no-default-features --features js --test web_wait

## SDL3 for WASIX with its "wasmer" video driver (integrations/sdl3): cloned
## at a pinned release, patched and built in $(SDL_WORK). A few minutes the
## first time; after that, what changed.
SDL_WORK ?= integrations/sdl3/work
SDL_LIB := $(SDL_WORK)/install/lib/libSDL3.a
SDL_OUT := integrations/sdl3/tests/out
SDL_PROGRAMS := $(patsubst integrations/sdl3/tests/%.c,$(SDL_OUT)/%.wasm,$(wildcard integrations/sdl3/tests/*.c))
IMGUI_DEMO := integrations/sdl3/imgui/out/imgui_demo.wasm

sdl: $(SDL_LIB)

$(SDL_LIB): integrations/sdl3/build.sh $(wildcard integrations/sdl3/*.patch integrations/sdl3/driver/*) include/wasmer/gui.h
	integrations/sdl3/build.sh $(SDL_WORK)

## Programs that use SDL, for the tests: integrations/sdl3/tests/out/*.wasm
sdl-programs: $(SDL_PROGRAMS)

$(SDL_OUT)/%.wasm: integrations/sdl3/tests/%.c $(SDL_LIB)
	@mkdir -p $(SDL_OUT)
	$(WASIXCC) --target=wasm32-wasix -Wall -Wextra -Werror -O2 $< -I$(SDL_WORK)/install/include -L$(SDL_WORK)/install/lib -lSDL3 -o $@

## What SDL makes of the host: the SDL programs on the headless host, each
## with the script that plays its user.
test-sdl: sdl-programs
	$(CARGO) test --features cli --test manifest_tests -- --ignored sdl_programs

## Dear ImGui's own SDL3 example, unchanged: integrations/sdl3/imgui/out
imgui-demo: $(IMGUI_DEMO)

$(IMGUI_DEMO): integrations/sdl3/imgui/build.sh $(SDL_LIB)
	integrations/sdl3/imgui/build.sh $(SDL_WORK)

## The demo with a script for a user, on the headless host. What it showed
## along the way is left as pictures in $(SDL_WORK)/imgui-shots.
test-imgui: $(IMGUI_DEMO)
	$(CARGO) run --release --features cli --bin gui_wasmer -- --script integrations/sdl3/imgui/demo.json --screenshots $(SDL_WORK)/imgui-shots $(IMGUI_DEMO)
	@ls $(SDL_WORK)/imgui-shots

## winit and softbuffer for WASIX with their backends for this API
## (integrations/winit): cloned at pinned releases, patched and built in
## $(WINIT_WORK), with winit's own `window` example and the test program.
## Needs cargo-wasix.
WINIT_WORK ?= integrations/winit/work
WINIT_PROGRAM := integrations/winit/tests/out/winit_events.wasm
WINIT_EXAMPLE := integrations/winit/example/out/window.wasm

winit-programs: $(WINIT_PROGRAM)

$(WINIT_PROGRAM): integrations/winit/build.sh $(wildcard integrations/winit/*.patch integrations/winit/backend/* integrations/winit/platform/* integrations/winit/softbuffer/* integrations/winit/tests/winit_events/src/* integrations/winit/tests/winit_events/Cargo.toml) sys/src/bindings.gen.rs
	integrations/winit/build.sh $(WINIT_WORK)

# The same run of the script leaves both.
$(WINIT_EXAMPLE): $(WINIT_PROGRAM)

## What winit makes of the host: its test program on the headless host with
## scripts for a user, and winit's own example walked through its key
## bindings.
test-winit: $(WINIT_PROGRAM) $(WINIT_EXAMPLE)
	$(CARGO) test --features cli --test manifest_tests -- --ignored winit_programs
	$(CARGO) run --features cli --bin gui_wasmer -- --script integrations/winit/example/window.json $(WINIT_EXAMPLE) > /dev/null

clean:
	$(CARGO) clean
	rm -rf $(OUT) $(GPU_OUT) examples/out dist $(SDL_OUT) integrations/sdl3/imgui/out integrations/winit/tests/out integrations/winit/example/out
