# Vixen recipes. Run from the repo root; fixture paths are root-relative.
# Toolchain: mise (odin, just). Provision pinned natives/fonts once:
#     mise trust && mise bootstrap --yes

set shell := ["bash", "-euo", "pipefail", "-c"]

BIN     := "vixen"
TP      := justfile_directory() / "thirdparty"
SDL_LIB := TP / "sdl3/lib"
NATIVE  := justfile_directory() / ".tmp/native"

ODIN_VERSION    := "dev-2026-03"
QUICKJS_VERSION := "2024-01-13"
LEXBOR_VERSION  := "2.5.0"
SQLITE_VERSION  := "3530400"
WAMR_VERSION    := "2.4.4"
SDL_VERSION     := "3.2.10"

default:
    @just --list

# Full one-shot provision: native libraries, then fonts.
setup: setup-dirs setup-native setup-fonts

setup-dirs:
    mkdir -p thirdparty "{{ NATIVE }}"

setup-native: setup-quickjs setup-lexbor setup-sqlite setup-wamr setup-stb setup-kb setup-sdl

setup-quickjs:
    #!/usr/bin/env bash
    set -euo pipefail
    test -f "quickjs-{{ QUICKJS_VERSION }}/libquickjs.a" && exit 0
    curl -fsSL -o "{{ TP }}/quickjs.tar.xz" "https://bellard.org/quickjs/quickjs-{{ QUICKJS_VERSION }}.tar.xz"
    tar -xf "{{ TP }}/quickjs.tar.xz" -C "{{ justfile_directory() }}"
    (cd "quickjs-{{ QUICKJS_VERSION }}" && make "libquickjs.a" -j"$(nproc)")
    rm -f "{{ TP }}/quickjs.tar.xz"

setup-lexbor:
    #!/usr/bin/env bash
    set -euo pipefail
    test -f "lexbor-{{ LEXBOR_VERSION }}/build/liblexbor_static.a" && exit 0
    curl -fsSL -o "{{ TP }}/lexbor.tar.gz" "https://github.com/lexbor/lexbor/archive/refs/tags/v{{ LEXBOR_VERSION }}.tar.gz"
    tar -xzf "{{ TP }}/lexbor.tar.gz" -C "{{ justfile_directory() }}"
    (cd "lexbor-{{ LEXBOR_VERSION }}" && cmake -B build -DCMAKE_BUILD_TYPE=Release -DLEXBOR_BUILD_SHARED=OFF -DLEXBOR_BUILD_STATIC=ON > /dev/null && cmake --build build -j"$(nproc)")
    rm -f "{{ TP }}/lexbor.tar.gz"

setup-sqlite: setup-dirs
    #!/usr/bin/env bash
    set -euo pipefail
    test -f "{{ NATIVE }}/sqlite3.o" && exit 0
    curl -fsSL -o "{{ TP }}/sqlite.zip" "https://www.sqlite.org/2026/sqlite-amalgamation-{{ SQLITE_VERSION }}.zip"
    rm -rf "{{ TP }}/sqlite-amalgamation-{{ SQLITE_VERSION }}"
    unzip -o -q "{{ TP }}/sqlite.zip" -d "{{ TP }}"
    gcc -O2 -c "{{ TP }}/sqlite-amalgamation-{{ SQLITE_VERSION }}/sqlite3.c" -o "{{ NATIVE }}/sqlite3.o" -DSQLITE_THREADSAFE=1 -DSQLITE_DEFAULT_SYNCHRONOUS=1
    rm -f "{{ TP }}/sqlite.zip"

setup-wamr:
    #!/usr/bin/env bash
    set -euo pipefail
    test -f "{{ TP }}/wasm-micro-runtime-WAMR-{{ WAMR_VERSION }}/build/libiwasm.a" && exit 0
    curl -fsSL -o "{{ TP }}/wamr.tar.gz" "https://github.com/bytecodealliance/wasm-micro-runtime/archive/refs/tags/WAMR-{{ WAMR_VERSION }}.tar.gz"
    rm -rf "{{ TP }}"/wasm-micro-runtime-WAMR-*
    tar -xzf "{{ TP }}/wamr.tar.gz" -C "{{ TP }}"
    (cd "{{ TP }}/wasm-micro-runtime-WAMR-{{ WAMR_VERSION }}" && cmake -B build -S product-mini/platforms/linux -DWAMR_BUILD_INTERP=1 -DWAMR_BUILD_AOT=0 -DWAMR_BUILD_JIT=0 -DWAMR_BUILD_FAST_JIT=0 -DWAMR_BUILD_LIBC_BUILTIN=1 -DWAMR_BUILD_LIBC_WASI=1 -DCMAKE_BUILD_TYPE=MinSizeRel > /dev/null && cmake --build build -j"$(nproc)")
    rm -f "{{ TP }}/wamr.tar.gz"

setup-stb: setup-dirs
    #!/usr/bin/env bash
    set -euo pipefail
    # Resolve the real SDK (mise shims must not be used as a base path).
    ODIN_BIN="$(mise which odin 2>/dev/null || command -v odin)"
    STB="$(dirname "$ODIN_BIN")/vendor/stb/src"
    gcc -O2 -c native/shim.c -o "{{ NATIVE }}/shim.o" -I. -I"$STB"
    gcc -O2 -c "$STB/stb_truetype.c" -o "{{ NATIVE }}/stb_truetype.o" -I"$STB"
    gcc -O2 -c "$STB/stb_image_write.c" -o "{{ NATIVE }}/stb_image_write.o" -I"$STB"
    gcc -O2 -c "$STB/stb_image.c" -o "{{ NATIVE }}/stb_image.o" -I"$STB"
    gcc -O2 -c "$STB/stb_image_resize.c" -o "{{ NATIVE }}/stb_image_resize.o" -I"$STB"
    ar rcs "{{ NATIVE }}/stb_native.a" "{{ NATIVE }}/stb_truetype.o" "{{ NATIVE }}/stb_image_write.o" "{{ NATIVE }}/stb_image.o" "{{ NATIVE }}/stb_image_resize.o"

setup-kb:
    #!/usr/bin/env bash
    set -euo pipefail
    # kb_text_shape ships unbuilt inside the Odin SDK; build it in place
    # (user-writable, version-scoped; rebuilt automatically per Odin pin).
    ODIN_BIN="$(mise which odin 2>/dev/null || command -v odin)"
    KB="$(dirname "$ODIN_BIN")/vendor/kb_text_shape"
    test -f "$KB/lib/kb_text_shape.a" && exit 0
    (cd "$KB/src" && bash build_unix.sh)

# SDL from source (Ubuntu 24.04 has no package; pinned build everywhere).
setup-sdl:
    #!/usr/bin/env bash
    set -euo pipefail
    # Static-only SDL (sidecar-free binary). Rebuilds when the recorded
    # prefix no longer matches this checkout.
    FRESH=0
    test -f "{{ SDL_LIB }}/libSDL3.a" || FRESH=1
    grep -q "prefix={{ TP }}/sdl3" "{{ SDL_LIB }}/pkgconfig/sdl3.pc" 2>/dev/null || FRESH=1
    test "$FRESH" = 0 && exit 0
    curl -fsSL -o "{{ TP }}/SDL.tar.gz" "https://github.com/libsdl-org/SDL/releases/download/release-{{ SDL_VERSION }}/SDL3-{{ SDL_VERSION }}.tar.gz"
    rm -rf "{{ TP }}/SDL3-{{ SDL_VERSION }}"
    tar -xzf "{{ TP }}/SDL.tar.gz" -C "{{ TP }}"
    (cd "{{ TP }}/SDL3-{{ SDL_VERSION }}" && cmake -B build -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="{{ TP }}/sdl3" -DSDL_SHARED=OFF -DSDL_STATIC=ON > /dev/null && cmake --build build -j"$(nproc)" > /dev/null && cmake --install build > /dev/null)
    rm -f "{{ TP }}/sdl3/lib/libSDL3.so"* "{{ TP }}/sdl3/lib/libSDL3_test"* 2>/dev/null || true
    rm -rf "{{ TP }}/SDL3-{{ SDL_VERSION }}" "{{ TP }}/SDL.tar.gz"

setup-fonts:
    #!/usr/bin/env bash
    # Fonts resolve at runtime through fontconfig (see fontfind.odin);
    # nothing is vendored. Verify the coverage the gates need.
    set -euo pipefail
    fail=0
    for fam in "DejaVu Sans" "Noto Sans Arabic" "Noto Sans Hebrew" "Noto Sans Devanagari" "Noto Serif Thai" "WenQuanYi Micro Hei"; do
      if fc-match "$fam" file > /dev/null 2>&1; then
        echo "font ok: $fam -> $(fc-match "$fam" file | cut -d: -f1)"
      else
        echo "font MISSING: $fam"
        fail=1
      fi
    done
    # Thai accepts Waree as fallback (see FONT_SPECS).
    fc-match "Waree" file > /dev/null 2>&1 || echo "note: Waree (Thai fallback) not installed"
    test "$fail" = 0 || { echo "install the README font list and rerun" >&2; exit 1; }

build:
    command -v odin > /dev/null || { echo "activate mise (eval \"$(mise activate bash)\") or run: mise exec -- just build" >&2; exit 1; }
    test -f {{ SDL_LIB }}/libSDL3.a || { echo "run 'just setup' first" >&2; exit 1; }
    test -f "{{ NATIVE }}/shim.o" && test -f "{{ NATIVE }}/sqlite3.o" && test -f "{{ NATIVE }}/stb_native.a" || { echo "run 'mise bootstrap --yes' to provision native objects" >&2; exit 1; }
    odin build src -out:{{ BIN }} -o:speed -extra-linker-flags:"`./scripts/sdl-flags.sh`"

test: build
    mkdir -p .tmp
    ./{{ BIN }} shapetest
    ./{{ BIN }} domtest
    ./{{ BIN }} termtest
    ./{{ BIN }} browsetest
    ./{{ BIN }} nettest
    ./{{ BIN }} wasmtest corpus/wtest.wasm
    ./{{ BIN }} parse corpus/example.html corpus/github.html
    ./{{ BIN }} js corpus/bench.js
    ./{{ BIN }} render --out .tmp/example.png corpus/example.html
    # One-shot Kitty encoder smoke only, not a terminal geometry/input test.
    ./{{ BIN }} tui corpus/app-shell.html > /dev/null
    python3 tests/tui_protocol.py ./{{ BIN }}
    python3 tests/profiles.py ./{{ BIN }}
    python3 tests/cli.py ./{{ BIN }}
    # NOTE: `show` (SDL window) stays manual-only until a GUI suite exists.
    # GUI work is gated behind green TUI+headless suites (ARCHITECTURE.md).

# Focused terminal tests: pure decoder plus independent PTY/protocol model.
test-tui: build
    ./{{ BIN }} termtest
    python3 tests/tui_protocol.py ./{{ BIN }}

# Focused session/network/terminal lifetime checks. The Odin executable is
# instrumented; separately compiled native dependencies are not rebuilt here.
test-sanitize:
    mkdir -p .tmp
    odin build src -out:.tmp/vixen-asan -o:none -debug -sanitize:address -extra-linker-flags:"`./scripts/sdl-flags.sh`"
    ASAN_OPTIONS=detect_leaks=1:halt_on_error=1 ./.tmp/vixen-asan termtest
    ASAN_OPTIONS=detect_leaks=1:halt_on_error=1 ./.tmp/vixen-asan browsetest
    ASAN_OPTIONS=detect_leaks=1:halt_on_error=1 ./.tmp/vixen-asan nettest
    ASAN_OPTIONS=detect_leaks=1:halt_on_error=1 python3 tests/tui_protocol.py ./.tmp/vixen-asan
    ASAN_OPTIONS=detect_leaks=1:halt_on_error=1 python3 tests/profiles.py ./.tmp/vixen-asan
    ASAN_OPTIONS=detect_leaks=1:halt_on_error=1 python3 tests/cli.py ./.tmp/vixen-asan

clean:
    rm -rf {{ BIN }} .tmp
