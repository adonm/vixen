# Vixen justfile. Run from the repo root: asset paths are root-relative.
# `just setup` first (pinned toolchains + native libraries, idempotent).

ODIN := "thirdparty/odin-sdk/odin"
BIN  := "vixen"
SDL_LIB := justfile_directory() / "thirdparty/sdl3/lib"

default:
    @just --list

setup:
    bash setup.sh

build:
    test -x {{ ODIN }} || { echo "run 'just setup' first" >&2; exit 1; }
    test -f {{ SDL_LIB }}/libSDL3.so || { echo "run 'just setup' first" >&2; exit 1; }
    {{ ODIN }} build spike -out:{{ BIN }} -o:speed -extra-linker-flags:"-L{{ SDL_LIB }} -Wl,-rpath,\\\$ORIGIN/thirdparty/sdl3/lib"

test: build
    ./{{ BIN }} shapetest
    ./{{ BIN }} domtest
    ./{{ BIN }} nettest
    ./{{ BIN }} wasmtest corpus/wtest.wasm
    ./{{ BIN }} parse corpus/example.html corpus/github.html
    ./{{ BIN }} js corpus/bench.js
    ./{{ BIN }} render --out .tmp/example.png corpus/example.html
    SDL_VIDEODRIVER=dummy ./{{ BIN }} show corpus/example.html
    ./{{ BIN }} tui --kitty=off corpus/app-shell.html > /dev/null

clean:
    rm -rf {{ BIN }} spike/*.o spike/*.a .tmp
