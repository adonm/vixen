# Vixen architecture

Status: reading-focused browser in active development (alpha). The active
Odin package is `vixen` under `src/`; this is the product, not a throwaway spike.
The Flutter/Rust implementation under `flutter/` is archived, not a runtime
dependency. Milestone status and acceptance criteria live in
[`ROADMAP.md`](ROADMAP.md).

## Implemented pipelines

Interactive browsing and headless URL dumps currently use:

```text
libcurl -> cookie jar/cache -> HTML bytes -> lexbor DOM
  -> eager image fetch/decode -> reader-style flow + shaped text
  -> retained Page (lines, links, fields, images, placements)
     -> viewport raster + hints/caret -> PNG -> Kitty graphics
     -> laid-out text -> headless dump
```

The parsed DOM is currently destroyed after layout. Source bytes are retained
for relayout. Forms are separate page records, not a live scripted DOM.

Local `render`, one-shot `tui`, and `show` use a separate file-loading path
and full-page rasterization. `show` uploads a static SDL texture and waits
briefly; it has no browsing event loop. The paths share layout/raster code,
but image loading and presentation differ. Output parity must be tested,
not assumed from a shared framebuffer.

QuickJS execution, JS↔DOM bindings/events, storage helpers, and WAMR native
calls are separate experiments/tests. The browsing path does not execute
page scripts, pump JS jobs, or instantiate page WASM.

## Components and boundaries

| Layer | Implementation | Current boundary |
|---|---|---|
| HTML | lexbor 2.5.0, static | Parsing/selectors, not a complete CSS renderer |
| Text | Odin SDK `kb_text_shape` + stb_truetype | Shaped glyph rasterization; document line breaking is custom and incomplete |
| Fonts | System fontconfig, runtime loaded | No checked-in runtime font assets; system fonts affect metrics and screenshots |
| Images/PNG | stb_image, stb_image_resize, stb_image_write | Supported raster formats only; viewport PNG transport for browsing |
| Network | System libcurl through `mincurl` | Synchronous easy handle, manual redirects; no responsive resource scheduler yet |
| Cookies | Custom jar + system libpsl | curl cookie engine disabled; partial browser policy coverage |
| Cache | Memory LRU + SQLite metadata + body files | HTTP caching subset, not full RFC/browser conformance |
| Profile | SQLite 3530400 + body files | Cookie/cache/localStorage helpers; history schema exists, browsing history is currently in RAM |
| Session storage | RAM | Helper implementation, not a complete live-page API |
| JavaScript | QuickJS 2024-01-13, static | Standalone evaluation and DOM/event tests only |
| WebAssembly | WAMR 2.4.4 interpreter, static | Native round trip; no page integration or execution budget |
| TUI | Linux termios + Kitty protocol | Ghostty primary; incremental decoder, bounded viewport/chrome, explicit image replacement; real-terminal validation pending |
| Desktop | SDL3 3.2.10, static | Static demonstration, not an interactive browser |

Mise owns tool/system provisioning through `mise bootstrap`; Just owns
repository recipes. CI uses `jdx/mise-action` and those same recipes.
Static native dependencies do not make the executable fully static: libcurl,
libpsl, glibc and their dependencies remain, and fontconfig is loaded at
runtime. Supported distribution/ABI and relocation checks are release work.

## Target design (not all implemented)

Three frontends should invoke the same browser actions and consume the same
document/layout state:

```text
headless commands | Kitty input | desktop input
                   -> browser/session actions
resource scheduler -> document -> layout snapshot + text/hit-test mapping
                                      -> viewport paint
                                      -> PNG | Kitty | SDL texture
profile services <-> browser/session and resource scheduler
```

### Ownership and lifecycle

- **Session:** owns profile services, navigation state, loading/error state,
  and eventually tabs. Async completions carry a navigation generation so
  obsolete requests cannot replace a newer document.
- **Document:** owns source/DOM, final URL/title, stable element identifiers,
  current form values, and resource handles. Input state is not recreated
  from the original HTML every time the viewport changes.
- **Layout snapshot:** owns derived geometry, shaped runs, links/control
  rectangles, and selection mappings. Construct a replacement before
  publishing it. Relayout must not navigate, refetch, or mutate history.
- **Viewport:** owns dimensions, scale, scroll anchor, focus, and selection.
  Reflow preserves logical position/focus rather than blindly retaining
  stale pixel coordinates or array indices.
- **Painter:** clips to the viewport and reuses buffers. Full-page exports
  need explicit size limits or tiled/streamed output.

M1 initially repairs ownership in the existing `Page` representation rather
than requiring a wholesale package rewrite. Stable document/layout
separation can then be introduced behind passing lifecycle tests.

### Source organization

- `src/browser.odin`: session, navigation, retained page, and relayout ownership.
- `src/render.odin`, `forms.odin`, `images.odin`: document geometry and painting.
- `src/tui.odin`, `tui_chrome.odin`: interaction state, overlays, and bounded chrome.
- `src/tui_runtime.odin`: event pump, terminal geometry, and invalidation.
- `src/terminal.odin`, `terminal_input.odin`, `kitty.odin`: terminal I/O,
  pure incremental decoding, and image transport.
- `src/test_*.odin`: in-package helper tests. `tests/` holds independent
  end-to-end harnesses; `corpus/` holds fixtures.
- `native/`: C adapters. Compiled objects/archives go to `.tmp/native/`.

Keep a single Odin package while these ownership interfaces settle. Splitting
every component into packages now would expose unstable internals just to
share them. Standalone DOM/JS/WASM experiments are not part of live navigation.

### Scheduling and presentation

Use bounded, cancellable libcurl resource work rather than blocking the UI
for each image. Initial text/placeholder paint should not wait for optional
images. Visible resources get priority, and image completion preserves the
reading anchor. Resource services and profile mutation need explicit owner
threads if workers are introduced.

The current event loop coalesces buffered input, marks page/chrome changes
separately, and checks geometry on a 100 ms idle poll. Unchanged state does
not cause a raster/PNG upload. TIOCGWINSZ supplies actual rows/columns and,
where available, pixels. Bounded CSI replies can supply cell pixels; a short
initial deadline falls back to 8×16 metrics if no reply arrives. Kitty cell
placement bounds still constrain the image. Viewports larger than 4096 pixels
on either axis are rejected rather than allocated; tiny windows show bounded
chrome without an image. These are not total pipeline memory bounds.

The Kitty backend reuses an owned image/placement ID, suppresses implicit
cursor movement with `C=1`, and deletes the image on exit. Geometry changes
reflow without resetting field builders/focus. Incremental document edits,
reading anchors, responsive network work, and real-terminal presentation
remain incomplete. Temp-allocator scratch is released between event ticks.

Interactive TUI assumes Kitty graphics capability, with Ghostty primary.
No environment-name whitelist is used. Headless output is an explicit
separate mode, never an automatic TUI fallback. During fullscreen operation,
Odin diagnostics are redirected to the profile's private `tui.log`; native
fd 2 is left intact for runtime/sanitizer failures. Current chrome uses
bounded ASCII escapes for Unicode/control characters, never raw page escape
bytes or guessed grapheme widths. Document pixels and input remain Unicode.
Catchable OS-signal restoration still needs a dedicated lifecycle mechanism.

### Reuse rather than mandatory DIY

SDL3 remains the platform layer. Before desktop implementation, evaluate a
small toolkit (for example Dear ImGui) for address bar, tabs, menus, text
input, clipboard, IME, and accessibility. Measure startup, memory, integration
work, and usability before choosing. A UI toolkit does not implement HTML/CSS
document layout; neither that fact nor current code size justifies building
every widget ourselves. Prefer tested Unicode segmentation/line-break data
over handwritten range tables. Further document-layout dependencies should
be evaluated against the declared HTML/CSS subset and shared shaping needs.

## Resource and trust model

This is currently a single-process, unsandboxed alpha. Not executing page JS
does not make native HTML/image parsers safe against arbitrary input. Before
beta, bound response/header bytes, decompressed data, document size/depth,
decoded pixel counts, concurrent requests, viewport allocations, and cache
usage. Apply limits before expensive allocations, with overflow checks.

Keep TLS verification enabled, constrain supported URL schemes, validate
redirect/origin/cookie behavior, and make failures recoverable. Page scripting
stays disabled until execution budgets, lifecycle, origin policy, and network
integration have dedicated tests. Process isolation and a broader security
model are requirements to evaluate before claiming hardened general-web use.

Current cache defaults are 32 MB RAM / 256 MB disk; localStorage has a 5 MB
per-origin helper quota. Image constants are 12 images, 8 MB decoded display
pixels per image, and 24 MB per page. They are **not proof of peak memory
bounds**: network buffering, natural-image decoding, and cumulative checks
need work. Images are also currently downscaled before layout, complicating
quality and sizing on later resizes.

## Measurement and verification

The primary performance metric is process launch to first usable page,
including fonts, layout, rasterization, encoding, and backend submission.
Measure cache-cold/cache-warm and process-cold starts separately; report
network time and image settlement separately. Kitty upload completion is an
observable proxy, not proof the terminal has presented the image. Desktop
tests should record first present.

Record commit/build flags, host, viewport, font versions, fixture hash,
cache state, and repeated p50/p95 results. Check peak RSS, allocation/resource
caps, steady-state memory, input latency, and bytes/frames transmitted.

Early September 2026 spike measurements compared different feature sets and
sometimes different memory metrics (JS heap versus process RSS). They are
historical observations, not a current performance baseline or evidence that
this browser is smaller/faster at equivalent work. Establish a reproducible
baseline in M0 before using benchmark claims in releases.

`browsetest` (legacy alias `tuitest`) exercises helpers/session/layout headlessly.
`termtest` exercises pure decoding and geometry. `tests/tui_protocol.py`
independently models output cursor bounds, Kitty placement/chunk dimensions,
image lifetime, raw-mode restoration, and idle behavior while driving a real
PTY. It covers fragmented input, paste, form submission through resize, and
hangup. It is not an actual graphics terminal and does not certify Ghostty
presentation. Desktop implementation remains gated behind headless/TUI beta
criteria; desktop beta additionally requires real window/input tests.

## Gotchas found (for the record)

- Upstream QuickJS uses **16-byte `JSValue` on 64-bit** (NaN boxing is
  32-bit-only). `JS_FreeValue`/`JS_IsException`/`JS_ToCString` are
  `static inline` — reachable only through `native/shim.c`.
- kb resolves font fallback **last-pushed-first**: push the widest-coverage
  fallback first, preferred fonts last (`FONT_PATHS` order matters).
- TTC table offsets are absolute, not member-relative (`font_upm_at`).
- lexbor `lxb_dom_node_tag_id` / `lxb_tag_name_by_id` are header-inline;
  link the `_noi` twins.
- Explicit-stack DOM walk needs a separate `entered` flag: `child == nil`
  means both "unvisited" and "exhausted" (caused the title/text infinite loop).
- kb context allocator must outlive the context: heap-hold it in `Render_Ctx`.
- Never `delete` slices aliasing static tables (`FONT_PATHS[i].path`).
- Never `delete` strings borrowed from lexbor tables (`tag_name_of` clones).
- `fmt.tprintf` is temp-allocator backed: stored strings need `aprintf` —
  and must NEVER be `delete()`d. Freeing temp memory corrupts the heap
  (segfaulted the TUI via the link-hint status line). Prefer stack buffers
  (`fmt.bprintf`) or direct `fmt.printf` for transient formatting.
- Odin map headers don't propagate mutations via old copies: re-store after
  mutating nested maps; `delete_key` doesn't free key strings.
- `delete()` on dynamic arrays frees backing but leaves headers: nil anything
  reused later (browse pages), or appends write into freed memory.
- Never store pointers to own-struct fields across returns (`&sess.store`
  dangles after `browse_open` returns): heap-hold shared state.
- Double `defer` of one closer double-frees; audit teardown for duplicates.
- `JS_NewClass` references the class def (no copy): static defs with literal
  names, or typeof lies and builtins segfault.
- QuickJS setters take ownership of set values; getters/setters/methods
  installed via SetProperty must NOT be freed after install (over-release
  frees live objects). Call/property-get results MUST be freed.
- Property setters use the generic argc/argv form; a (ctx,this,val) shape
  reads the argv pointer as a value.
- `lxb_html_serialize_cb` is single-node; subtree HTML needs `tree_cb`.

## Current compatibility limitations

- Reader-style greedy word wrapping, not author-CSS layout; long words can
  overflow. Tables are flattened, and whitespace/preformatted text is limited.
- Word-by-word shaping does not establish paragraph-level BiDi correctness.
  Rune-width helpers use SDK Unicode data, not full terminal-specific
  grapheme/emoji handling. Chrome avoids that ambiguity with ASCII escapes.
- `head`/`script`/`style` and several other subtrees are skipped. Landmark
  filtering can omit useful content; this is a readability heuristic, not
  a guarantee that those elements contain no article content.
- Forms are a partial text/search/hidden/submit/button/textarea subset.
  Select, checkbox/radio, file, image-button, and `type=button` are skipped.
  Controls in skipped landmarks are specially handled.
- Images: PNG/JPEG/GIF-first-frame/BMP through stb; SVG/WebP, data URLs,
  srcset, and animation are not supported by the current browsing path.
- No live page-script execution, JS job pump, timers, fetch/XHR bridge,
  JS↔WASM integration, media, or extension platform.
- Runtime system fonts; webfonts/variable fonts are not established support.
- Profile selection is shared across commands; see README for precedence
  and explicit access to legacy profiles. Browsing history is not yet
  persisted despite the database schema.
- Terminal input, geometry, focus, repaint, and memory defects are bugs to
  fix, not accepted compatibility omissions. Milestones track the remaining
  work without treating a helper-suite pass as frontend certification.
