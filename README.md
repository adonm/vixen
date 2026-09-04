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

One command (Debian/Ubuntu, sudo available):

```sh
mise trust && mise bootstrap --yes && just build
```

That converges the system packages declared in `.mise.toml`, installs the
pinned Odin SDK + just, provisions the native libraries (QuickJS, lexbor,
SQLite, WAMR, stb, kb, SDL3-from-source), verifies font coverage, then builds.
SDL3 is built pinned from source because Ubuntu 24.04 has no SDL3 package;
fonts resolve at runtime through fontconfig, so nothing is vendored. Everything is idempotent — re-running is
safe. See `.mise.toml` (`[bootstrap.packages]`, `[tools]`) and the
`setup-*` recipes for the exact pins.

`just setup` is idempotent and recreates the exact `thirdparty/` layout the
build expects. Nothing under `thirdparty/` or `*.o`/`*.a` is
committed; versions are pinned in `.mise.toml` (Odin, just) and the
`setup-*` recipes. Run recipes from an activated shell
(`eval "$(mise activate bash)"`) or prefix with `mise exec --`.

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
`corpus/` paths are root-relative by convention; fonts come from the system
fontconfig, never the repo.

## Layout

| Path | Role |
|---|---|
| `spike/` | Odin engine: parse, JS, shape, raster, net, storage, DOM, TUI |
| `corpus/` | Test fixtures: pages, scripts, wasm module, test HTTP server |
| `thirdparty/` | Pinned SDKs/sources (provisioned, not committed) |
| `schema.sql` | Profile database schema (loaded at build time) |
| `flutter/` | Archived Flutter/Rust implementation (read-only) |

`ARCHITECTURE.md` records the pipeline, measured numbers, and every
porting gotcha found so far. The default profile lives at
`~/.config/spikebrowser` (`$SPIKE_PROFILE` overrides).
