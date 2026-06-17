# Vixen build plan

Phased execution runbook. Each phase ends in a green test suite, a
working binary, and a measured size. Do not start the next phase until
the previous one's gate passes.

Tick-tock discipline applies throughout: each phase is a *tick*
(capability lands); the post-phase cleanup is the *tock* (dead-code
removal, module ≤ 1 kLOC, references cited). See `docs/ACCEPTANCE.md`
for the per-phase gates.

---

## Phase 0 — Scaffolding (≈ 3 days)

Create the workspace from `docs/ARCHITECTURE.md`. Empty crates with
stub `lib.rs` so the workspace compiles.

**Steps:**

1. Workspace `Cargo.toml` with all 7 crates as members. Root `src/main.rs`
   calls `vixen_shell::run()` (which is a stub for now).
2. `vixen-api` populated: `Engine` trait, `EngineDelegate` (`Send`),
   `EngineInspector`, `EngineProfile`, DTOs, `EngineDiagnostic` shape —
   per `docs/ARCHITECTURE.md`.
3. `vixen-shell` skeleton: `App` component with empty
   `FactoryVecDeque<TabModel>` and a placeholder window. Establish the
   Relm4 worker/factory patterns early per ADR-010 — the shell's
   idioms should be set in Phase 0, not retrofitted later.
4. `vixen-net`, `vixen-store`, `vixen-wpt`, `vixen-headless`, `vixen-core`
   all empty with `pub mod placeholder;` stubs.
5. `justfile` adapted: `check-all-host` builds the workspace; `test-host`
   runs `vixen-api` tests (the only crate with logic yet — the other
   crates are stubs at this point).
6. `.gitignore`, `LICENSE` (Apache 2.0), `data/`, `build-aux/` skeleton,
   `fixtures/` (empty), `benches/` (empty).
7. `.mise.toml` pins the dev toolchain (`rust = latest`, `just`,
   `cargo-binstall`) so `mise bootstrap --yes` converges a fresh machine.
   The library MSRV (1.88) is in each crate's `rust-version`; the dev
   toolchain floats to latest stable. The **GNOME 50 SDK is not installed
   on the host** — it is managed inside a flatpak-builder container
   (`just flatpak-update-sdk` / `just flatpak-build`); see
   [`docs/guidance/gnome-sdk-flatpak-builder.md`](guidance/gnome-sdk-flatpak-builder.md)
   and [`mise bootstrap`](https://mise.jdx.dev/bootstrap.html).

**Gate:** `cargo check --workspace` passes. `cargo test -p vixen-api`
passes (the trait shape compiles, basic DTO tests pass). The shell's
empty `App` launches and renders an empty window.

---

## Phase 1 — Networking and storage crown jewels (≈ 1 week)

Build the well-tested, fail-closed subsystems first. These are pure
Rust with no upstream-crate dependencies.

**Steps:**

1. `vixen-net/src/network.rs`: reqwest + rustls HTTP client, HTTP/2,
   gzip, brotli, redirect handling, max body size, cookie header
   generation. Test surface: every error variant of `NetworkError`.
2. `vixen-net/src/cookie.rs`: RFC 6265 jar, every rule in
   `docs/SPEC.md` "Cookie contract". Test surface: every rejection rule,
   every outgoing-header rule, the 512-entry cap, FIFO eviction.
3. `vixen-net/src/url_policy.rs`: blocklist per `docs/SPEC.md` "URL
   policy", including the precise CGNAT check
   (`100.64.0.0/10`, not all of `100/8`).
4. `vixen-net/src/csp.rs`: directive parser + enforcer per
   `docs/SPEC.md` "CSP contract". Test surface: every directive, every
   source-list grammar element.
5. `vixen-net/src/permissions.rs`, `origin.rs`, `fetch_types.rs`,
   `http_helpers.rs`: small supporting modules.
6. `vixen-store/src/lib.rs`: redb-backed persistence, per-origin
   partitioning, schema per `docs/ARCHITECTURE.md` "App ID and profile
   paths".

**Gate:** `cargo test -p vixen-net -p vixen-store` green. `cargo audit`
clean. Fuzz `url_policy::validate_http_url` and `csp::parse` for 1 M
iterations each without panic.

---

## Phase 2 — SpiderMonkey runtime (≈ 1 week)

Stand up the JS engine.

**Steps:**

1. Add `mozjs` to `vixen-core/Cargo.toml`. Per ADR-005, default to
   static link for development; system-link in the production Flatpak
   manifest.
2. `vixen-core/src/script.rs`: `JsRuntime` owning a
   `mozjs::rust::Runtime`, with `compartment_for_origin(&Origin)` and
   `evaluate(&mut self, origin, src) -> Result<JsValue>`.
   3. Host hook registration, minimum viable: `console.log`, `fetch`
   (delegating to `vixen-net::Network`), `document.title` getter.
   Defer the full DOM/Event/Storage surface to Phase 6.
4. Rooting discipline: every `JS::Value` / `JSObject*` goes through
   `mozjs::rust::RootedGuard`. No naked handles.

**Gate:** `vixen-headless --url file:///.../hello.html --eval '1+2'`
returns `3`. `cargo test -p vixen-core` green (basic eval tests).
Binary size recorded.

---

## Phase 3 — HTML parse + Stylo cascade (≈ 2 weeks)

Wire up HTML parsing and CSS cascade.

**Steps:**

1. `html5ever` parse into RcDom. Already a dependency.
   2. `vixen-core/src/doc.rs`: `Node`/`Element` implementing
   `style::dom::{TNode, TElement, TDocument, TRestyleSummary}`. Backed
   by the RcDom. This is the bulk of the integration work — budget 3–4
   days for trait conformance. Consult
   `reference-browsers/firefox/servo/components/script/dom/` for DOM
   node patterns and `reference-browsers/firefox/servo/components/style/dom.rs`
   for the trait definitions being implemented.
   3. `vixen-core/src/style.rs`: load `<style>` / `<link rel=stylesheet>`
   into `Stylesheet` list → `Stylist::update_stylist` → cascade via
   Stylo's `SharedStyleContext` / traversal. Expose
   `computed_values_for(NodeId) -> Arc<ComputedValues>`.
4. CSS-wide keywords, `@layer`, `@property`, `@import`, `@supports`,
   `@media`, `@keyframes`, custom properties + `var()` all come free
   from Stylo. Verify via WPT fixtures.

**Gate:** `vixen-headless --url fixtures/css/at-property.html
--extract-selector '[style]'` returns correctly cascaded styles.
vixen-wpt runs the CSS fixtures; pass rate recorded as baseline.

---

## Phase 4 — Layout (≈ 1–2 weeks)

Turn cascade output into a positioned box tree.

**Steps:**

1. Add Servo `layout_2020` crate to `vixen-core` (per ADR-001 and
   `docs/REFERENCES.md`). Confirm dependency closure is workable; if
   `layout_2020` coverage proves too sparse for real sites, narrow the
   v1.0 layout scope and document the gap in `docs/COMPAT.md` rather
   than swapping crates.
2. `vixen-core/src/layout.rs`: feed the cascade output + DOM into the
   layout engine, produce a positioned box tree.
3. CSS Grid, Flexbox, block layout, all `position` values, scroll
   containers — all from the upstream `layout_2020` crate.

**Gate:** Visual-hash WPT check on 20+ fixtures matches reference
baseline within tolerance. Specifically, nested-flex/grid + padding +
margins + gaps must produce correct absolute coordinates *without* any
post-pass coordinate fixup (`layout_2020` doesn't need it).

---

## Phase 5 — Paint: WebRender + EGL surfaceless (≈ 2 weeks)

Make the engine produce pixels via a single WebRender paint path bound
to two `GlContext` implementations.

**Steps:**

1. `vixen-core/src/paint.rs`: single `DisplayList` type + a WebRender
   `Renderer` that consumes a `&dyn GlContext` (trait defined in
   `vixen-api`, see ADR-006). One paint path; the two `GlContext`
   implementations are the only thing that varies between GUI and
   headless.
2. `GlAreaSurface` (in `vixen-shell`): implements `GlContext` around
   `gtk4::GLArea`. Per the GTK4 idiom, GL work happens inside the
   `render` signal callback, where GTK has already made the
   `gdk::GLContext` current; `proc_address` dispatches through Gdk's GL
   loader. This is the GUI path.
3. `SurfacelessSurface` (in `vixen-headless`): implements `GlContext`
   via `EGL_MESA_platform_surfaceless` (or `EGL_KHR_surfaceless` +
   pbuffer fallback). Renders into an FBO; `glReadPixels` extracts RGBA.
   This is the headless/CI path.
4. Display-list builder enforces the invariants from `SPEC.md`:
   z-index sorting, clip stacking (content clipped, borders not),
   opacity group multiplication, visibility skip-paint, background
   clip/origin/attachment.
5. `vixen-shell/src/engine_factory.rs`: creates the `gtk4::GLArea`,
   wraps it as `GlAreaSurface` (the shell's `GlContext` impl), and
   returns it as the content widget alongside the tab's `EngineWorker`.
   The worker's engine renders to the screen via that `GlContext`.
6. CI: verify `LIBGL_ALWAYS_SOFTWARE=1` produces working screenshots
   via `llvmpipe` so headless runs anywhere.

**Gate:** `just run` shows a real web page in the window.
`vixen-headless --screenshot out.png --url fixtures/css/border-rendering.html`
produces a PNG matching the GUI's render within 1 % pixel diff on 5
fixtures (both renders going through the same WebRender paint path).

---

## Phase 6 — Host bindings (≈ 2 weeks)

Register the DOM/Event/Storage/Network host hooks the modern web needs.
Priority order:

1. **DOM Core**: `document`, `Node`, `Element`, `HTMLElement`, attribute
   accessors, `querySelector*`, `getElementsByTagName`, `classList`,
   `dataset`.
2. **Events**: `Event`, `EventTarget`, `addEventListener`,
   `removeEventListener`, dispatch, capture/target/bubble, focus/click/
   input/submit/change, `composedPath()`, composed event dispatch order
   per `docs/SPEC.md`.
3. **Forms**: `HTMLInputElement`, `HTMLFormElement`, `HTMLSelectElement`,
   `HTMLTextAreaElement`, `HTMLButtonElement`, `ValidityState` (11
   flags per `docs/SPEC.md`), `checkValidity`, `reportValidity`,
   `setCustomValidity`, form submission algorithm.
4. **Storage**: `localStorage`, `sessionStorage` against `vixen-store`,
   per-origin partitioning.
5. **Network**: `fetch`, `XMLHttpRequest`, `Request`/`Response`,
   `Headers`, `URL`, `URLSearchParams`, `TextEncoder`/`TextDecoder`.

Each family lands with its WPT fixtures passing before moving on.

**Gate:** `fixtures/dom/`, `fixtures/events/`, `fixtures/forms/`,
`fixtures/storage/`, `fixtures/network/` all pass.

---

## Phase 7 — Security hardening (≈ 1 week)

Wire every trust boundary from `docs/ARCHITECTURE.md`.

**Steps:**

1. CSP enforcement at `script.rs::evaluate` (script-src / unsafe-inline /
   nonce / hash) and at fetch (per-directive URL matching).
2. Cookie validation already done in Phase 1; confirm document-side
   boundary (`document.cookie` cannot set HttpOnly).
3. URL policy re-applied at every fetch, including JS-initiated fetch /
   XHR.
4. Origin isolation confirmed across storage, scripts, cookies.
5. Permissions API behaves per spec.
6. `cargo audit` clean. `cargo deny` checks pass.
7. Fuzz targets: `url_policy`, `csp::parse`, `html5ever` parse, the
   cookie parser. Each runs 1 M iterations without panic.

**Gate:** Every security test in `vixen-net` and `vixen-core` green.
Zero `cargo audit` advisories. Fuzz targets stable.

---

## Phase 8 — Headless CDP + tooling polish (≈ 1 week)

Implement the full headless tool surface.

**Steps:**

1. Implement CDP server (tokio + tokio-tungstenite) in `vixen-headless`.
   Command handlers call into `vixen-core` via the `EngineInspector`
   trait.
2. Implement every CLI flag from `docs/SPEC.md` "Headless CLI surface".
   Stable error codes preserved exactly.
3. Implement `--memory-stats`, `--paint-stats`, `--incremental`,
   `--list-fonts`, `--cdp`. (Note: `--gpu` is omitted per ADR-003 —
   every render path is GPU-backed.)
4. `--cdp` responds to: `Browser.getVersion`, `Target.createTarget`,
   `Target.attachToTarget`, `Page.navigate`, `Page.loadEventFired`,
   `Runtime.evaluate`.

**Gate:** Every CLI flag works. CDP responds to required methods.

---

## Phase 9 — Release hardening (≈ 1 week)

Final tock before v1.0.

**Steps:**

1. Module size audit: no non-test module > 1,000 lines. Decompose where
   needed.
2. Dead-code removal pass: `cargo machete`, fix every clippy warning,
   audit `#[allow(dead_code)]` annotations.
3. Performance baselines: establish criterion baselines for
   `benches/{parse,style,layout,render}` as each lands (Phase 3+).
   Future releases gate on no > 10 % regression vs the most recent
   release.
4. Binary size measurement: `just size-fp`. Confirm targets per
   `docs/ACCEPTANCE.md`.
5. WPT fixture share ≥ 70 % for CSS+DOM. Migrate remaining Rust tests.
6. Write `docs/COMPAT.md` — the honest capability matrix (what works,
   what's partial, what's missing, what's planned for v1.1/v1.2).
7. Write user-facing release notes.

**Gate:** every release gate in `docs/ACCEPTANCE.md` green. Tag `v1.0.0`.

---

## Total: ~12–14 weeks of focused work.

---

## Binary size strategy

Concrete levers, in priority order:

1. **System-link libmozjs** in the production Flatpak. Saves ~3 MiB
   stripped. Per ADR-005.
2. **`[profile.release]`** is already optimal (see `docs/ARCHITECTURE.md`).
3. **Feature-gate aggressively**: CDP, devtools UI, keychain integration.
   Each behind a feature. Default build includes none of them.
4. **System Cairo/Pango/HarfBuzz/fontconfig** from the GNOME SDK.
   WebRender uses the system GL stack; glyph rasterisation goes through
   fontconfig + freetype (system) into WebRender's own atlas.
5. **One paint path, not N.** ADR-003/ADR-006 enforce this: no
   `tiny-skia`, no `fontdue`, no parallel CPU rasterizer, no
   `PaintBackend` trait.
6. **Per-release measurement** in `docs/ACCEPTANCE.md`.

**Targets:**

| Binary              | Target (system mozjs) | Target (static mozjs) |
|---------------------|-----------------------|-----------------------|
| `vixen` (GUI)       | ≤ 10 MiB              | ≤ 14 MiB              |
| `vixen-headless`    | ≤ 8 MiB               | ≤ 14 MiB              |

---

## Testing strategy

**WPT-first.** Every CSS/DOM/Layout/Paint feature is tested via a WPT
fixture in `fixtures/`, not a Rust unit test. Rust tests cover only pure
logic (CSS length arithmetic, URL parsing, cookie validation, CSP
parsing, redb storage round-trip).

**13 check types** in `vixen-wpt` (per `docs/SPEC.md`): 12 inherited
from the upstream WPT assertion model plus `ref-equivalent`, the 13th —
Vixen's addition: a rendered page compared against a reference HTML
fixture, like upstream WPT reftests.

**Snapshot tests against Firefox reference.** A `fixtures/reftest-baseline/`
directory contains reference renderings. Each visual WPT fixture
compares against the baseline with a perceptual hash and 1 % pixel-diff
tolerance. Failures dump a side-by-side diff to `target/reftest-diff/`.

**Performance regression.** `benches/{parse,style,layout,render}`
criterion benches run on every release. Gate: no > 10 % regression vs
previous release.

---

## Risk register

| Risk                                              | Likelihood | Impact | Mitigation                                                                          |
|---------------------------------------------------|:----------:|:------:|-------------------------------------------------------------------------------------|
| Stylo integration harder than estimated           | Medium     | High   | Time-box Phase 3 to 2 weeks; if traversal conformance blocks, narrow v1.0 CSS scope and document gaps in `docs/COMPAT.md`. |
| `mozjs` build complexity                          | Medium     | High   | Use system libmozjs where possible. Vendor mozjs as a Flatpak module (ADR-005).     |
| EGL surfaceless unavailable on some CI runners    | Low        | Medium | `LIBGL_ALWAYS_SOFTWARE=1` + Mesa `llvmpipe` covers every Linux runner.              |
| `gtk4::GLArea` context sharing with WebRender     | Medium     | High   | Validate in Phase 5 first week. Fallback: render to FBO, blit to GLArea with a tex. |
| `layout_2020` coverage too sparse for real sites  | Medium     | Medium | Narrow v1.0 layout scope; document gaps in `docs/COMPAT.md`. No fallback crate (per ACCEPTANCE.md hard gate). |
| SpiderMonkey GC + Rust ownership friction         | Medium     | Medium | Follow `reference-browsers/firefox/servo/components/script/bindings/` patterns.     |
| Real-world pages regress vs Servo/Firefox         | Low        | Medium | Upstream issues; report and work around. Document in `docs/COMPAT.md`.              |
| WPT migration backlog grows during build          | Medium     | Medium | Per-phase gate: each phase deletes Rust tests at the rate it adds WPT fixtures.     |
| Relm4 breaking change in `Factory`/`Worker` API   | Low        | Medium | Pin Relm4 version per release; consult `reference-browsers/relm4/` on upgrades.     |

---

## Per-phase gate summary

| Phase                             | Gate                                                                                             |
|-----------------------------------|--------------------------------------------------------------------------------------------------|
| 0 — Scaffolding                   | `cargo check --workspace` passes; `cargo test -p vixen-api` passes                               |
| 1 — Net + store crown jewels      | `cargo test -p vixen-net -p vixen-store` green; fuzz 1 M iters stable                            |
| 2 — SpiderMonkey                  | `vixen-headless --url <file> --eval '1+2'` returns `3`                                            |
| 3 — HTML + Stylo                  | WPT CSS fixtures pass; cascade output correct                                                    |
| 4 — Layout                        | 20+ visual-hash fixtures match reference                                                         |
| 5 — Paint                         | `just run` shows a page; headless PNG within 1 % of GUI on 5 fixtures                            |
| 6 — Host bindings                 | `fixtures/{dom,events,forms,storage,network}/` all pass                                          |
| 7 — Security                      | `cargo audit` clean; all security tests green; fuzz stable                                       |
| 8 — Headless CDP                  | Every CLI flag works; CDP responds to required methods                                           |
| 9 — Release                       | All `docs/ACCEPTANCE.md` gates green; tag `v1.0.0`                                               |
