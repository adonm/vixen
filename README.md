# Vixen

A reading-focused browser written in Odin, aiming for small resource usage
and fast startup to the first usable page. **Vixen is in alpha, not yet a
general-purpose or hardened browser.** Wikipedia and documentation browsing
are the first usability targets. This is the active product implementation,
not a disposable spike.

The previous Flutter/Rust implementation is archived under [`flutter/`](flutter/)
and on branch `archive/flutter-final`. Active development stays in this
repository; the archived implementation is not part of the new runtime.

## Setup and build

The current development target is Linux, with Debian/Ubuntu provisioning
and sudo available:

```sh
mise trust
mise bootstrap --yes
mise exec -- just build
```

Mise owns tool versions and system packages through [`.mise.toml`](.mise.toml).
Just owns repository recipes: native-library setup, builds, and tests.
Bootstrap provisions the native libraries and checks system font resolution.
Dependency versions are recorded in `.mise.toml` and the `setup-*` recipes in
[`Justfile`](Justfile). Build products are not committed.

The executable uses system fonts discovered through fontconfig; no font
directory is required beside it. SDL3 is linked statically, but libcurl,
libpsl, glibc, their dependencies, and runtime fontconfig are system
requirements. This is **not a fully static binary** or a promise of
compatibility with every Linux distribution. Relocation is a release gate,
not yet a supported packaging guarantee.

## Try it

Use Ghostty for interactive browsing. The TUI uses the Kitty graphics
protocol, with no interactive text fallback:

```sh
./vixen browse 'https://en.wikipedia.org/wiki/Rust_(programming_language)'
./vixen browse --dump 'https://en.wikipedia.org/wiki/Rust_(programming_language)'
./vixen render --out .tmp/article.png --width 900 corpus/article.html
```

Build/test recipes run from the repository root. Fixture paths above are
examples, not runtime assets required by URL browsing. Create `.tmp` before
using it as an output directory if the test recipe has not done so.

### Commands

| Command | Current behavior |
|---|---|
| `browse <url>` | Interactive, keyboard-driven Kitty-graphics browser; requires a terminal |
| `browse --dump [--width N] <url>` | Fetch once and print laid-out text; no display required |
| `render --out file.png [--width N] page.html` | Render a local HTML file to a full-page PNG |
| `tui [--width N] page.html` | One-shot Kitty image output, **not** the interactive browser; emits graphics even to a pipe |
| `show page.html` | Experimental SDL window showing a static render briefly, not a desktop browser |
| `fetch [--profile DIR] <url>` | Fetch through cookies/cache and print response statistics, not the response body |
| `parse`, `js`, `rss` | Developer probes, not headless browser automation APIs |

Interactive layout uses terminal dimensions rather than `--width`. A
Kitty-graphics-capable terminal is assumed; there is no terminal-name
whitelist or text fallback. Ghostty is the primary target. Multiplexer
graphics passthrough is not implemented. `--kitty=off` is no longer supported.

### Interactive controls

- `j`/`k` or arrows: scroll; Space: page down; `g`/`G`: top/bottom.
- Type a visible link number, then Enter: follow the link.
- `u`: edit a URL; Enter opens it, Escape cancels.
- `b`/`f`: back/forward; `r`: reload through the cache.
- Tab/Shift-Tab: move field focus (scrolls into view); type to edit with
  visible caret/selection; Enter submits (textarea: Enter=new line, Tab to
  a submit button + Enter to send); Escape leaves the field.
- `/`: find in page (live highlights, Enter keeps for `n`/`N`, Esc clears);
  `n`/`N`: next/previous match. Fragment `#links` jump without refetch.
- `q`: quit outside an editor; Ctrl-C/Ctrl-D also quit from either editor.
  SIGTERM/SIGHUP restore the terminal and exit 128+sig.

The PTY suite covers fragmented Unicode, reverse tab, visible field paint
(typing changes PNG bytes), focus/value preservation across resize, reading
anchors across reflow/history/reload, fragment jumps, find highlights,
geometry, idle redraws, and signal restoration. Chrome displays non-ASCII
and control characters as `\u{...}` escapes for predictable row bounds;
document rendering and submitted input retain their original Unicode.
Fullscreen diagnostics go to `tui.log` inside the selected profile. Invalid
CLI invocations exit 2 with usage; failed loads/exports exit 1.
`vixen version` reports the build; `just bench` records repeatable timings.
Real-terminal presentation still needs manual checks.

### Profiles

All browsing and fetch commands use the same precedence:

1. `--profile DIR`
2. `$VIXEN_PROFILE`
3. `$SPIKE_PROFILE` (legacy explicit override)
4. `$XDG_CONFIG_HOME/vixen`, or `$HOME/.config/vixen` when unset

Missing profile directories are created. For example:

```sh
./vixen browse --profile "$HOME/.vixen-test" 'https://en.wikipedia.org/wiki/Odin'
```

Existing profiles are never moved or deleted automatically. To reuse an
older default profile, pass `--profile /tmp/opencode/vixen-profile` or
`--profile "$HOME/.config/spikebrowser"` explicitly. Do not use this alpha
for sensitive accounts.

## Feature status

| Area | Status |
|---|---|
| HTTP(S), cookies, cache | Integrated through system libcurl, libpsl, SQLite, and body files; partial policy coverage |
| Documents | Reader-style HTML flow with mid-word breaks (grapheme-safe), shaped text, links, fragment jumps, find-in-page, simple lists/tables; no author-CSS layout engine |
| Forms | Text/search/hidden/submit/button/textarea with visible editing (textarea: multi-row, Enter=newlines, vertical scroll), urlencoded GET/POST; skipped controls show notes; passwords not masked |
| Images | Eager fetching and stb decoding of supported raster formats; placeholders otherwise |
| JavaScript and DOM | Standalone QuickJS/DOM experiments with helper tests; not executed during live browsing |
| WebAssembly | Native WAMR round-trip experiment; no page/JS integration |
| Desktop | Static SDL demonstration only |

Image loading can delay first paint. Current size constants do not bound
all network/decode allocations. SVG/WebP, media, full CSS, JS-heavy sites,
and a sandbox are not supported. Unsupported content may be omitted rather
than rendered faithfully.

## Verification

```sh
mise exec -- just test
mise exec -- just test-tui
mise exec -- just test-sanitize
```

| Gate | Current evidence |
|---|---|
| `vixen shapetest` | Selected shaping and font-fallback cases |
| `vixen domtest` | Standalone DOM queries, mutations, and events |
| `vixen browsetest` | Helpers for layout, wrapping, fragments, find, limits, textarea, session, forms, images, visible field paint, scroll anchors, and TUI handlers; `tuitest` remains a compatibility alias |
| `vixen termtest` | Pure incremental input, metrics, chrome bounds, and invalidation tests |
| `just test-tui` | Decoder tests and independent PTY/Kitty output-model tests in `tests/tui_protocol.py` |
| `tests/profiles.py` | Profile precedence, old-data preservation, and headless commands run outside the checkout |
| `tests/cli.py` | Invalid invocations exit 2, failed loads/exports exit 1, `--help`/`version` exits 0 |
| `just bench` | Repeatable wall-time evidence (rss/parse/js/render/dump/PTY first frame) to `.tmp/bench.json`; no timing thresholds |
| `vixen nettest` | URL, cookie, cache, redirect, and storage cases against local fixtures |
| `vixen wasmtest corpus/wtest.wasm` | Native↔WASM calls |
| Parse/JS/render/Kitty smoke commands | Invocation/output smoke, not frontend usability certification |
| `just test-sanitize` | ASan/leak checks on session/network/terminal helpers and frontend harnesses; separately built C dependencies are not instrumented |

CI uses `jdx/mise-action`, `mise bootstrap`, and Just recipes. `just test`
includes the PTY and profile harnesses. The terminal model checks protocol
geometry, not actual Ghostty/Kitty presentation. A passing run does not
establish desktop usability, web compatibility, or absence of all
memory/resource bugs. Stronger gates are planned in
[`ROADMAP.md`](ROADMAP.md).

## Repository map

| Path | Role |
|---|---|
| `src/` | Active `vixen` Odin package: browser, document rendering, platform frontends, and bindings |
| `src/test_*.odin` | In-package helper tests, exercised by developer commands |
| `native/` | C adapters; compiled objects live under `.tmp/native/`, not in source directories |
| `tests/` | End-to-end Python harnesses for terminal protocol and CLI profiles |
| `corpus/` | Local fixtures and deterministic HTTP test server |
| `thirdparty/` | Provisioned native dependencies, not committed |
| `.tmp/` | Ignored build products and isolated test artifacts |
| `schema.sql` | Profile schema embedded at build time |
| `flutter/` | Archived Flutter/Rust implementation |

See [`ARCHITECTURE.md`](ARCHITECTURE.md) for implemented and target designs,
ownership rules, and limitations; [`ROADMAP.md`](ROADMAP.md) defines the path
to headless, TUI, and desktop betas.
