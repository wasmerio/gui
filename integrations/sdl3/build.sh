#!/usr/bin/env bash
#
# SDL3 for WASIX, with the "wasmer" video driver: SDL's windows, input, text
# entry, clipboard and software framebuffer on <wasmer/gui.h>.
#
#   ./build.sh              # work in ./work next to this script
#   ./build.sh /some/dir    # work in /some/dir
#
# Clones SDL at the release this was made against, applies the two patches
# of this directory, copies the driver in (driver/ -> src/video/wasmer/) and
# builds a static library:
#
#   <work>/install/lib/libSDL3.a      <work>/install/include/SDL3/
#
# A second run builds what changed: edit the driver here and run it again.
#
# Environment, all optional:
#   SDL_REPO        where to clone SDL from (another clone will do)
#   SDL_TESTS=1     also build SDL's own test programs: <work>/build/test/*.wasm
#   SDL_HIDE_API=1  compile SDL with -DSDL_DECLSPEC= so that its ~1000 public
#                   functions are not exported from the programs linked with
#                   it (wasixcc links with --export-dynamic). Smaller modules.
#
# Needs on PATH, or where their installers put them:
#   wasixcc >= 0.4.7 (~/.wasixcc/bin)   cmake >= 3.16   ninja   git
#
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="${1:-$HERE/work}"
GUI_INCLUDE="$(cd "$HERE/../../include" && pwd)"

SDL_REPO="${SDL_REPO:-https://github.com/libsdl-org/SDL.git}"
SDL_TAG="release-3.4.18"
SDL_COMMIT="829a65d769d935c4852f8159e964312c0957260a"

export PATH="$HOME/.wasixcc/bin:$PATH"

mkdir -p "$WORK"
WORK="$(cd "$WORK" && pwd)"
SRC="$WORK/SDL"
BUILD="$WORK/build"
PREFIX="$WORK/install"

# 1. SDL, at the commit the patches are for.
if [ ! -d "$SRC/.git" ]; then
  echo "== clone SDL $SDL_TAG"
  git -c advice.detachedHead=false clone --quiet --depth 1 --branch "$SDL_TAG" "$SDL_REPO" "$SRC"
fi
GOT="$(git -C "$SRC" rev-parse HEAD)"
if [ "$GOT" != "$SDL_COMMIT" ]; then
  echo "unexpected SDL commit $GOT (wanted $SDL_COMMIT)" >&2
  exit 1
fi

# 2. The patches, in order: SDL on WASIX, then the place for the driver. The
#    clone is this script's own: it notes which patches it applied, and when
#    they are not the ones here now it goes back to the release first.
PATCHES="sdl3-wasix.patch sdl3-wasmer.patch"
STAMP="$SRC/.git/wasmer-patches"
WANT="$(cd "$HERE" && cat $PATCHES | git hash-object --stdin)"
if [ "$(cat "$STAMP" 2>/dev/null || true)" != "$WANT" ]; then
  git -C "$SRC" checkout --quiet -- .
  for patch in $PATCHES; do
    echo "== apply $patch"
    git -C "$SRC" apply "$HERE/$patch"
  done
  echo "$WANT" > "$STAMP"
fi

# 3. The driver. Only what differs is copied, so that nothing is built again
#    for nothing.
mkdir -p "$SRC/src/video/wasmer"
for file in "$HERE"/driver/*; do
  cmp -s "$file" "$SRC/src/video/wasmer/$(basename "$file")" || cp "$file" "$SRC/src/video/wasmer/"
done

# 4. Configure: a static library with the wasmer and dummy video drivers and
#    the software renderer. Threads, timers, time and the file system are
#    WASIX's own (chosen by the WASI branch the first patch adds); what a host
#    would have to provide and does not (audio devices, joysticks, sensors)
#    gets SDL's dummy backends, so that programs asking for those subsystems
#    still start. SDL's own iconv is used: the C library's gives other
#    results than SDL's tests expect.
EXTRA_CFLAGS=""
if [ "${SDL_HIDE_API:-0}" = "1" ]; then
  EXTRA_CFLAGS="-DSDL_DECLSPEC="
fi
TESTS=OFF
if [ "${SDL_TESTS:-0}" = "1" ]; then
  TESTS=ON
fi

echo "== configure"
cmake -S "$SRC" -B "$BUILD" -G Ninja \
  -DCMAKE_TOOLCHAIN_FILE="$HERE/wasix-toolchain.cmake" \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_INSTALL_PREFIX="$PREFIX" \
  -DCMAKE_C_FLAGS="$EXTRA_CFLAGS" \
  -DSDL_WASMER_GUI_INCLUDE_DIR="$GUI_INCLUDE" \
  -DSDL_SHARED=OFF -DSDL_STATIC=ON \
  -DSDL_TEST_LIBRARY="$TESTS" -DSDL_TESTS="$TESTS" -DSDL_EXAMPLES=OFF \
  -DSDL_ASSEMBLY=OFF -DSDL_SYSTEM_ICONV=OFF \
  -DSDL_VIDEO=ON -DSDL_WASMER_VIDEO=ON -DSDL_DUMMYVIDEO=ON -DSDL_OFFSCREEN=OFF \
  -DSDL_RENDER=ON -DSDL_GPU=OFF \
  -DSDL_OPENGL=OFF -DSDL_OPENGLES=OFF -DSDL_VULKAN=OFF \
  -DSDL_AUDIO=ON -DSDL_DUMMYAUDIO=ON -DSDL_DISKAUDIO=OFF \
  -DSDL_JOYSTICK=ON -DSDL_HAPTIC=ON -DSDL_SENSOR=ON -DSDL_POWER=ON \
  -DSDL_CAMERA=ON -DSDL_DIALOG=ON -DSDL_TRAY=ON \
  -DSDL_HIDAPI=OFF -DSDL_LIBUDEV=OFF \
  > "$WORK/configure.log" 2>&1 || { tail -40 "$WORK/configure.log"; exit 1; }
sed -n '/^-- Enabled backends:/,/^-- $/p' "$WORK/configure.log"
if ! grep -q "Video drivers:.*wasmer" "$WORK/configure.log"; then
  echo "the wasmer video driver was not enabled: see $WORK/configure.log" >&2
  exit 1
fi

# 5. Build and install: headers, libSDL3.a, the CMake package, sdl3.pc.
echo "== build"
cmake --build "$BUILD" > "$WORK/build.log" 2>&1 || { grep -n "error" "$WORK/build.log" | head -40; exit 1; }
echo "warnings: $(grep -c 'warning:' "$WORK/build.log" || true)"
cmake --install "$BUILD" > "$WORK/install.log" 2>&1
# For whoever builds by its date (make): the library is as of now.
touch "$PREFIX/lib/libSDL3.a"
ls -l "$PREFIX/lib/libSDL3.a"
