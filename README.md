# Vixen

Smallest/fastest usable browser, written in Odin. Single process, explicit
memory, zero giant runtimes: lexbor for HTML, QuickJS for JS, kb_text_shape +
stb_truetype for text, SDL3 for display, SQLite for profile storage, WAMR for
in-page WebAssembly.

The previous Flutter/Rust implementation is archived untouched under
[`flutter/`](flutter/) (branch `archive/flutter-final` matches it exactly).
This rewrite does not share code with it; it reuses only the idea that every
claim ships with a gate.

## Setup

System packages (Debian/Ubuntu):

```sh
sudo apt-get install -y --no-install-recommends \
  build-essential cmake curl python3 pkg-config \
  libcurl4-openssl-dev libpsl-dev libx11-dev libxext-dev \
  libxrandr-dev libxcursor-dev libxfixes-dev libxi-dev libxss-dev \
  libxkbcommon-dev libwayland-dev libegl-dev libasound2-dev \
  fonts-noto-core fonts-lohit-deva fonts-thai-tlwg fonts-wqy-microhei \
  fonts-sil-ezra fonts-dejavu-core
```

SDL3 is built pinned from source by `just setup` (Ubuntu 24.04 has no
SDL3 package); no sudo is needed for that step.

Then fetch the pinned toolchains and build the native libraries
(Odin SDK, QuickJS, lexbor, SQLite, WAMR, stb objects, corpus fonts):

```sh
just setup
```

`setup.sh` is idempotent and recreates the exact `thirdparty/` layout the
build expects. Nothing under `thirdparty/`, `fonts/`, or `*.o`/`*.a` is
committed; versions are pinned in `setup.sh`.

## Build and gates

```sh
just build   # odin build spike -> ./vixen
just test    # shaping, DOM, network, wasm, parse/js/render smoke
```

| Gate | What it proves |
|---|---|
| `vixen shapetest` | 8 shaping cases: lam-alef, conjuncts, BiDi, CJK uniformity |
| `vixen domtest` | 20 DOM cases: query, attrs, tree edit, event phases |
| `vixen nettest` | 58 network cases: URL, cookies, cache, Vary, redirects, storage |
| `vixen wasmtest corpus/wtest.wasm` | native->wasm round trip both directions |
| `vixen parse/js/render` | corpus smoke incl. PNG output |

Modes: `render` (page to PNG), `show` (SDL3 window), `tui` (Kitty graphics
or plain text), `fetch` (URL through jar+cache). Run from the repo root:
all asset paths (`corpus/`, `fonts/`) are root-relative by convention.

## Layout

| Path | Role |
|---|---|
| `spike/` | Odin engine: parse, JS, shape, raster, net, storage, DOM, TUI |
| `corpus/` | Test fixtures: pages, scripts, wasm module, test HTTP server |
| `fonts/` | Pinned corpus fonts (provisioned, not committed) |
| `thirdparty/` | Pinned SDKs/sources (provisioned, not committed) |
| `schema.sql` | Profile database schema (loaded at build time) |
| `flutter/` | Archived Flutter/Rust implementation (read-only) |

`ARCHITECTURE.md` records the pipeline, measured numbers, and every
porting gotcha found so far. The default profile lives at
`~/.config/spikebrowser` (`$SPIKE_PROFILE` overrides).
