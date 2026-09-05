# Vixen

An experimental browser written in Odin, aiming for small resource usage
and fast startup to the first usable page. **The rewrite is an alpha, not a
general-purpose or hardened browser.** Wikipedia and documentation browsing
are the first usability targets.

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

Interactive layout uses terminal dimensions rather than `--width`. Current
terminal detection checks `TERM` for `ghostty`/`kitty` or `KITTY_WINDOW_ID`;
this heuristic can reject capable terminals and will be replaced by the
capability assumption described in the roadmap. `--kitty=off` is no longer
supported.

### Interactive controls

- `j`/`k` or arrows: scroll; Space: page down; `g`/`G`: top/bottom.
- Type a visible link number, then Enter: follow the link.
- `u`: edit a URL; Enter opens it, Escape cancels.
- `b`/`f`: back/forward; `r`: reload through the cache.
- Tab: focus a field; type to edit; Enter submits; Escape leaves the field.
- `q`: quit outside an editor; Ctrl-C/Ctrl-D also quit outside the URL editor.

These are prototype controls. Unicode input, reverse tab, visible field
editing, resize/focus preservation, geometry, and redraw behavior still need
end-to-end terminal coverage. See [the roadmap](ROADMAP.md), not the helper
test count, for usability status.

### Profiles

Prefer an explicit existing-parent directory while defaults are being
unified:

```sh
./vixen browse --profile "$HOME/.vixen-test" 'https://en.wikipedia.org/wiki/Odin'
```

Currently `browse` (including `--dump`) defaults to
`/tmp/opencode/vixen-profile`, while `fetch` uses `$SPIKE_PROFILE` or
`~/.config/spikebrowser`. These inconsistent development defaults are not
the intended release interface. Do not use this alpha for sensitive accounts.

## Feature status

| Area | Status |
|---|---|
| HTTP(S), cookies, cache | Integrated through system libcurl, libpsl, SQLite, and body files; partial policy coverage |
| Documents | Reader-style HTML flow, shaped text, links, simple lists/tables; no author-CSS layout engine |
| Forms | Partial text/search/hidden/submit/button/textarea support and urlencoded GET/POST submission |
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
```

| Gate | Current evidence |
|---|---|
| `vixen shapetest` | Selected shaping and font-fallback cases |
| `vixen domtest` | Standalone DOM queries, mutations, and events |
| `vixen tuitest` | **Headless helper tests** for layout, session, forms, images, and selected TUI handlers—not a terminal emulator test |
| `vixen nettest` | URL, cookie, cache, redirect, and storage cases against local fixtures |
| `vixen wasmtest corpus/wtest.wasm` | Native↔WASM calls |
| Parse/JS/render/Kitty smoke commands | Invocation/output smoke, not frontend usability certification |

CI uses `jdx/mise-action`, `mise bootstrap`, and Just recipes. A passing run
does not establish terminal geometry, desktop usability, web compatibility,
or absence of memory/resource bugs. Stronger gates are planned in
[`ROADMAP.md`](ROADMAP.md).

## Repository map

| Path | Role |
|---|---|
| `spike/` | Active Odin implementation; historical package name |
| `corpus/` | Local fixtures and deterministic HTTP test server |
| `thirdparty/` | Provisioned native dependencies, not committed |
| `schema.sql` | Profile schema embedded at build time |
| `flutter/` | Archived Flutter/Rust implementation |

See [`ARCHITECTURE.md`](ARCHITECTURE.md) for implemented and target designs,
ownership rules, and limitations; [`ROADMAP.md`](ROADMAP.md) defines the path
to headless, TUI, and desktop betas.
