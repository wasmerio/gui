#!/usr/bin/env bash
#
# winit 0.30 and softbuffer 0.4 for WASIX, with backends for <wasmer/gui.h>:
# winit's windows and events from the Wasmer runtime, and softbuffer's frames
# to it.
#
#   ./build.sh              # work in ./work next to this script
#   ./build.sh /some/dir    # work in /some/dir
#
# Clones winit and softbuffer at the releases this was made against, applies
# the two patches of this directory and copies the backends in:
#
#   backend/          -> winit/src/platform_impl/wasix/
#   platform/wasix.rs -> winit/src/platform/wasix.rs
#   softbuffer/       -> softbuffer/src/backends/
#
# and builds, for WASIX:
#
#   example/out/window.wasm       winit's own `window` example, unchanged
#   tests/out/winit_events.wasm   the program of tests/, for `make test-winit`
#
# A second run builds what changed: edit a backend here and run it again.
#
# An application uses the two crates the same way these programs do: it names
# winit and softbuffer as it would anywhere, and says where they come from.
#
#   [patch.crates-io]
#   winit = { path = "<work>/winit" }
#   softbuffer = { path = "<work>/softbuffer" }
#   wasmer-gui-sys = { path = "<wasmer-gui>/sys" }
#
# Environment, all optional:
#   WINIT_REPO, SOFTBUFFER_REPO   where to clone from (another clone will do)
#
# Needs: git, and rustup with cargo-wasix and its `wasix` toolchain
# (cargo install cargo-wasix; cargo wasix build fetches the toolchain).
#
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="${1:-$HERE/work}"
SYS="$(cd "$HERE/../../sys" && pwd)"
TARGET=wasm32-wasmer-wasi

WINIT_REPO="${WINIT_REPO:-https://github.com/rust-windowing/winit.git}"
WINIT_TAG="v0.30.13"
WINIT_COMMIT="e9809ef54b18499bb4f2cac945719ecc2a61061b"
SOFTBUFFER_REPO="${SOFTBUFFER_REPO:-https://github.com/rust-windowing/softbuffer.git}"
SOFTBUFFER_TAG="v0.4.8"
SOFTBUFFER_COMMIT="d871852faa1137d4615b99b9ad31c3ba80f345b5"

# cargo-wasix points Cargo at a registry of crates forked for WASIX, which
# has none of what winit's other platforms depend on: Cargo resolves a
# lockfile for every target, and fails before anything is compiled. Nothing
# built here needs a fork.
export CARGO_WASIX_NO_REGISTRY_CONFIG=1

mkdir -p "$WORK"
WORK="$(cd "$WORK" && pwd)"

# A clone at the commit the patch is for, with the patch applied. The clone
# is this script's own: it notes which patch it applied, and when that is
# not the one here now it goes back to the release first.
fetch() { # <directory> <repository> <tag> <commit> <patch>
  local dir="$WORK/$1" repo="$2" tag="$3" commit="$4" patch="$HERE/$5"
  if [ ! -d "$dir/.git" ]; then
    echo "== clone $1 $tag"
    git -c advice.detachedHead=false clone --quiet --depth 1 --branch "$tag" "$repo" "$dir"
  fi
  local got
  got="$(git -C "$dir" rev-parse HEAD)"
  if [ "$got" != "$commit" ]; then
    echo "unexpected $1 commit $got (wanted $commit)" >&2
    exit 1
  fi
  local stamp="$dir/.git/wasmer-patch" want
  want="$(git hash-object "$patch")"
  if [ "$(cat "$stamp" 2>/dev/null || true)" != "$want" ]; then
    git -C "$dir" checkout --quiet -- .
    echo "== apply $5"
    git -C "$dir" apply "$patch"
    echo "$want" > "$stamp"
  fi
}

# Only what differs is copied, so that nothing is built again for nothing.
place() { # <file> <directory>
  mkdir -p "$2"
  cmp -s "$1" "$2/$(basename "$1")" || cp "$1" "$2/"
}

fetch winit "$WINIT_REPO" "$WINIT_TAG" "$WINIT_COMMIT" winit-wasix.patch
fetch softbuffer "$SOFTBUFFER_REPO" "$SOFTBUFFER_TAG" "$SOFTBUFFER_COMMIT" softbuffer-wasix.patch
for file in "$HERE"/backend/*.rs; do
  place "$file" "$WORK/winit/src/platform_impl/wasix"
done
place "$HERE/platform/wasix.rs" "$WORK/winit/src/platform"
place "$HERE/softbuffer/wasix.rs" "$WORK/softbuffer/src/backends"

# Where the three crates come from, as an application's [patch.crates-io]
# would say it.
PATCHES=(
  --config "patch.crates-io.wasmer-gui-sys.path=\"$SYS\""
  --config "patch.crates-io.softbuffer.path=\"$WORK/softbuffer\""
)

echo "== winit's window example"
(cd "$WORK/winit" && cargo wasix build --release --example window "${PATCHES[@]}")
mkdir -p "$HERE/example/out"
cp "$WORK/winit/target/$TARGET/release/examples/window.wasm" "$HERE/example/out/window.wasm"

echo "== the test program"
(cd "$HERE/tests/winit_events" && CARGO_TARGET_DIR="$WORK/target" cargo wasix build --release \
  "${PATCHES[@]}" --config "patch.crates-io.winit.path=\"$WORK/winit\"")
mkdir -p "$HERE/tests/out"
cp "$WORK/target/$TARGET/release/winit_events.wasm" "$HERE/tests/out/winit_events.wasm"

ls -l "$HERE/example/out/window.wasm" "$HERE/tests/out/winit_events.wasm"
