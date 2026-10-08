#!/usr/bin/env bash
#
# Dear ImGui's own SDL3 + SDL_Renderer example, unchanged, built for WASIX
# against the SDL3 of this directory (../build.sh): a program that asks SDL
# for a window, its software renderer, the keyboard, the mouse, text input,
# cursors and the clipboard, as programs written for desktops do.
#
#   ./build.sh [sdl-work-dir]      # default: ../work, where ../build.sh puts SDL
#
# The result is out/imgui_demo.wasm next to this script. Run it with
#
#   gui_wasmer --window out/imgui_demo.wasm
#   wasmer run --experimental-gui out/imgui_demo.wasm
#
# Environment, optional:
#   IMGUI_REPO   where to clone Dear ImGui from (another clone will do)
#
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="${1:-$HERE/../work}"
WORK="$(cd "$WORK" && pwd)"
PREFIX="$WORK/install"

IMGUI_REPO="${IMGUI_REPO:-https://github.com/ocornut/imgui.git}"
IMGUI_TAG="v1.92.9"
IMGUI_COMMIT="01380c579715e62fb9a8d6ec0502c4ea83bfde6e"

export PATH="$HOME/.wasixcc/bin:$PATH"

if [ ! -f "$PREFIX/lib/libSDL3.a" ]; then
  echo "SDL is not built in $WORK: run ../build.sh first" >&2
  exit 1
fi

SRC="$WORK/imgui"
if [ ! -d "$SRC/.git" ]; then
  echo "== clone Dear ImGui $IMGUI_TAG"
  git -c advice.detachedHead=false clone --quiet --depth 1 --branch "$IMGUI_TAG" "$IMGUI_REPO" "$SRC"
fi
GOT="$(git -C "$SRC" rev-parse HEAD)"
if [ "$GOT" != "$IMGUI_COMMIT" ]; then
  echo "unexpected Dear ImGui commit $GOT (wanted $IMGUI_COMMIT)" >&2
  exit 1
fi

mkdir -p "$HERE/out"
echo "== build imgui_demo.wasm"
wasix++ -O2 -fno-exceptions -fno-rtti \
  -I"$SRC" -I"$SRC/backends" -I"$PREFIX/include" \
  "$SRC/imgui.cpp" "$SRC/imgui_demo.cpp" "$SRC/imgui_draw.cpp" \
  "$SRC/imgui_tables.cpp" "$SRC/imgui_widgets.cpp" \
  "$SRC/backends/imgui_impl_sdl3.cpp" "$SRC/backends/imgui_impl_sdlrenderer3.cpp" \
  "$SRC/examples/example_sdl3_sdlrenderer3/main.cpp" \
  -L"$PREFIX/lib" -lSDL3 \
  -o "$HERE/out/imgui_demo.wasm"
ls -l "$HERE/out/imgui_demo.wasm"
