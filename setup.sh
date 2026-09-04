#!/usr/bin/env bash
# Vixen one-shot setup: pinned Odin SDK, native libraries, corpus fonts.
# Idempotent: existing artifacts are reused. No sudo: install the apt list
# from README.md first. Run from the repo root.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
tp="$root/thirdparty"
mkdir -p "$tp" "$root/fonts" "$root/.tmp"

ODIN_VERSION=dev-2026-03
ODIN_DIR="$tp/odin-sdk"
if [[ ! -x "$ODIN_DIR/odin" ]]; then
  echo "fetch: odin $ODIN_VERSION"
  curl -fsSL -o "$tp/odin.tar.gz" \
    "https://github.com/odin-lang/Odin/releases/download/$ODIN_VERSION/odin-linux-amd64-$ODIN_VERSION.tar.gz"
  rm -rf "$ODIN_DIR"
  mkdir -p "$ODIN_DIR"
  tar -xzf "$tp/odin.tar.gz" -C "$ODIN_DIR" --strip-components=1
  rm -f "$tp/odin.tar.gz"
fi
"$ODIN_DIR/odin" version

# QuickJS 2024-01-13 (static).
if [[ ! -f "$root/quickjs-2024-01-13/libquickjs.a" ]]; then
  echo "build: quickjs"
  curl -fsSL -o "$tp/quickjs.tar.xz" https://bellard.org/quickjs/quickjs-2024-01-13.tar.xz
  tar -xf "$tp/quickjs.tar.xz" -C "$root"
  (cd "$root/quickjs-2024-01-13" && make libquickjs.a -j"$(nproc)")
  rm -f "$tp/quickjs.tar.xz"
fi

# lexbor v2.5.0 (static).
if [[ ! -f "$root/lexbor-2.5.0/build/liblexbor_static.a" ]]; then
  echo "build: lexbor"
  curl -fsSL -o "$tp/lexbor.tar.gz" https://github.com/lexbor/lexbor/archive/refs/tags/v2.5.0.tar.gz
  tar -xzf "$tp/lexbor.tar.gz" -C "$root"
  (cd "$root/lexbor-2.5.0" && cmake -B build -DCMAKE_BUILD_TYPE=Release \
    -DLEXBOR_BUILD_SHARED=OFF -DLEXBOR_BUILD_STATIC=ON > /dev/null \
    && cmake --build build -j"$(nproc)")
  rm -f "$tp/lexbor.tar.gz"
fi

# SQLite amalgamation 3530400 (object for the spike link).
if [[ ! -f "$root/spike/sqlite3.o" ]]; then
  echo "build: sqlite"
  curl -fsSL -o "$tp/sqlite.zip" https://www.sqlite.org/2026/sqlite-amalgamation-3530400.zip
  rm -rf "$tp/sqlite-amalgamation-3530400"
  unzip -o -q "$tp/sqlite.zip" -d "$tp"
  gcc -O2 -c "$tp/sqlite-amalgamation-3530400/sqlite3.c" -o "$root/spike/sqlite3.o" \
    -DSQLITE_THREADSAFE=1 -DSQLITE_DEFAULT_SYNCHRONOUS=1
  rm -f "$tp/sqlite.zip"
fi

# WAMR 2.4.4, interpreter only (in-page wasm runtime).
if [[ ! -f "$tp/wasm-micro-runtime-WAMR-2.4.4/build/libiwasm.a" ]]; then
  echo "build: wamr"
  curl -fsSL -o "$tp/wamr.tar.gz" \
    https://github.com/bytecodealliance/wasm-micro-runtime/archive/refs/tags/WAMR-2.4.4.tar.gz
  rm -rf "$tp"/wasm-micro-runtime-WAMR-*
  tar -xzf "$tp/wamr.tar.gz" -C "$tp"
  (cd "$tp/wasm-micro-runtime-WAMR-2.4.4" && cmake -B build -S product-mini/platforms/linux \
    -DWAMR_BUILD_INTERP=1 -DWAMR_BUILD_AOT=0 -DWAMR_BUILD_JIT=0 -DWAMR_BUILD_FAST_JIT=0 \
    -DWAMR_BUILD_LIBC_BUILTIN=1 -DWAMR_BUILD_LIBC_WASI=1 -DCMAKE_BUILD_TYPE=MinSizeRel > /dev/null \
    && cmake --build build -j"$(nproc)")
  rm -f "$tp/wamr.tar.gz"
fi

# stb objects + local shim (compiled with the SDK's stb sources).
STB="$ODIN_DIR/vendor/stb/src"
gcc -O2 -c "$root/spike/shim.c" -o "$root/spike/shim.o" -I"$root" -I"$STB"
gcc -O2 -c "$STB/stb_truetype.c" -o "$root/spike/stb_truetype.o" -I"$STB"
gcc -O2 -c "$STB/stb_image_write.c" -o "$root/spike/stb_image_write.o" -I"$STB"
ar rcs "$root/spike/stb_native.a" "$root/spike/stb_truetype.o" "$root/spike/stb_image_write.o"

# SDL 3.2.10 from source (Ubuntu 24.04 has no libsdl3-dev; a pinned local
# build keeps dev and CI identical). Installed under thirdparty/sdl3; the
# Justfile links with -L/-rpath to it, so no sudo is ever needed.
SDL_VERSION=3.2.10
if [[ ! -f "$tp/sdl3/lib/libSDL3.so" ]]; then
  echo "build: SDL $SDL_VERSION"
  curl -fsSL -o "$tp/SDL.tar.gz" \
    "https://github.com/libsdl-org/SDL/releases/download/release-$SDL_VERSION/SDL3-$SDL_VERSION.tar.gz"
  rm -rf "$tp/SDL3-$SDL_VERSION"
  tar -xzf "$tp/SDL.tar.gz" -C "$tp"
  (cd "$tp/SDL3-$SDL_VERSION" && cmake -B build -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX="$tp/sdl3" > /dev/null \
    && cmake --build build -j"$(nproc)" > /dev/null \
    && cmake --install build > /dev/null)
  rm -rf "$tp/SDL3-$SDL_VERSION" "$tp/SDL.tar.gz"
fi

# kb_text_shape static lib (ships unbuilt inside the SDK).
if [[ ! -f "$ODIN_DIR/vendor/kb_text_shape/lib/kb_text_shape.a" ]]; then
  echo "build: kb_text_shape"
  (cd "$ODIN_DIR/vendor/kb_text_shape/src" && bash build_unix.sh)
fi

# Corpus fonts: system packages first, one download for Naskh Arabic.
need_fonts=0
for fam in "DejaVu Sans" "Noto Sans Arabic" "Noto Sans Hebrew" "Noto Sans Devanagari" "WenQuanYi Micro Hei"; do
  fc-match "$fam" file 2>/dev/null | grep -q . || { echo "missing font family: $fam"; need_fonts=1; }
done
if [[ ! -f "$root/fonts/NotoNaskhArabic[wght].ttf" ]]; then
  echo "fetch: Noto Naskh Arabic"
  curl -fsSL -o "$root/fonts/NotoNaskhArabic[wght].ttf" \
    "https://raw.githubusercontent.com/google/fonts/main/ofl/notonaskharabic/NotoNaskhArabic%5Bwght%5D.ttf"
fi
copy_font() { # $1 = fc pattern, $2 = dest name
  src=$(fc-match -f "%{file}\n" "$1" 2>/dev/null) || return 1
  cp "$src" "$root/fonts/$2"
}
copy_font "DejaVu Sans" DejaVuSans.ttf
copy_font "Noto Sans Arabic" NotoSansArabic-Regular.ttf
copy_font "Noto Sans Hebrew" NotoSansHebrew-Regular.ttf
copy_font "Noto Sans Devanagari" NotoSansDevanagari-Regular.ttf
copy_font "Noto Serif Thai:style=Regular" NotoSerifThai-Regular.ttf || \
  cp /usr/share/fonts/truetype/tlwg/Waree.ttf "$root/fonts/Waree.ttf"
copy_font "WenQuanYi Micro Hei" wqy-microhei.ttc
if [[ "$need_fonts" != 0 ]]; then
  echo "setup: some font families are missing; install the README apt list and rerun" >&2
  exit 1
fi

echo "setup: ok ($("$ODIN_DIR/odin" version))"
