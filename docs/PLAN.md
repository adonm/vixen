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
4. `vixen-net`, `vixen-store`, `vixen-wpt`, `vixen-headless`, `vixen-engine`
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

1. Add `mozjs` to `vixen-engine/Cargo.toml`. Per ADR-005, default to
   static link for development; system-link in the production Flatpak
   manifest.
2. `vixen-engine/src/script.rs`: `JsRuntime` owning a
   `mozjs::rust::Runtime`, with `compartment_for_origin(&Origin)` and
   `evaluate(&mut self, origin, src) -> Result<JsValue>`.
   3. Host hook registration, minimum viable: `console.log`, `fetch`
   (delegating to `vixen-net::Network`), `document.title` getter.
   Defer the full DOM/Event/Storage surface to Phase 6.
4. Rooting discipline: every `JS::Value` / `JSObject*` goes through
   `mozjs::rust::RootedGuard`. No naked handles.

**Gate:** `vixen-headless --url file:///.../hello.html --eval '1+2'`
returns `3`. `cargo test -p vixen-engine` green (basic eval tests).
Binary size recorded.

---

## Phase 3 — HTML parse + Stylo cascade (≈ 2 weeks)

Wire up HTML parsing and CSS cascade.

**Steps:**

1. `html5ever` parse into RcDom. Already a dependency. **Done** — see
   `vixen-engine::doc`.
2. **Selector matching via Stylo (done) — `vixen-engine::style_dom`**
   implements `selectors::Element` over the RcDom (a precomputed
   `ElementArena` keeps the module `forbid(unsafe_code)`). This powers
   `--extract-selector`, the WPT selector checks, and the
   `:valid`/`:invalid`/`:checked` pseudos. Phase 3's gate (WPT CSS
   fixtures) now passes against the selector surface.
3. `vixen-engine/src/style.rs` (next slice): load `<style>` / `<link
   rel=stylesheet>` into `Stylesheet` list → `Stylist::update_stylist`
   → cascade via Stylo's `SharedStyleContext` / traversal. Expose
   `computed_values_for(NodeId) -> Arc<ComputedValues>`. Requires
   implementing the full `TNode` / `TElement` / `TDocument` traits;
   budget 3–4 days for trait conformance. Consult
   `reference-browsers/firefox/servo/components/script/dom/` for DOM
   node patterns and `reference-browsers/firefox/servo/components/style/dom.rs`
   for the trait definitions being implemented.
4. CSS-wide keywords, `@layer`, `@property`, `@import`, `@supports`,
   `@media`, `@keyframes`, custom properties + `var()` all come free
   from Stylo. Verify via WPT fixtures.

**Pure-logic foundation landed (testing-strategy item).**
`vixen-engine::length` implements CSS Values 4 `<length>` parsing + the
absolute/relative unit conversions the cascade and layout resolves against
(`px`/`em`/`rem`/`%`/`vh`/`vw`/`vmin`/`vmax`/`ex`/`ch`/`pt`/`pc`/`in`/`cm`/`mm`/`Q`).
Rust-unit-tested per "Rust tests cover only pure logic (CSS length
arithmetic, …)".

**The rest of the CSS Values 4 dimension family landed.** `<length>` was
the first; the family is now complete for v1.0:
- `vixen-engine::color` — CSS Color 4 sRGB family: 3/4/6/8-digit hex,
  `rgb()/rgba()` (legacy comma + modern space forms), `hsl()/hsla()` with
  hue normalisation, the 148 named colours, `transparent`/`currentcolor`
  keywords, premultiplied-alpha arithmetic, and linear-sRGB interpolation
  (the primitive gradients and transitions reduce to). `oklch/lab/lch/color()`
  fail closed with `UnsupportedColorSpace` (deferred slice).
- `vixen-engine::angle` — `<angle>` (`deg`/`rad`/`grad`/`turn`) with
  degree/radian normalisation, `cos_sin()` for transforms and conic gradients.
- `vixen-engine::time` — `<time>` (`s`/`ms`) with millisecond normalisation
  for transitions/animations.
- `vixen-engine::resolution` — `<resolution>` (`dpi`/`dpcm`/`dppx`/`x`) with
  dots-per-pixel normalisation for media queries. `x` is the historical
  alias for `dppx` (CSS Images 4 § 7.3).
- `vixen-engine::ratio` — CSS Values 4 § 4.4 `<ratio>`
  (`number | number / number`): the numerator/denominator pair with the
  quotient the `aspect-ratio` property and the `aspect-ratio` /
  `device-aspect-ratio` media features reduce to. A zero denominator is the
  § 4.4 "infinite ratio" encoding; the single-number shorthand means `N/1`;
  the legacy Media-Queries-4 integer `width/height` form folds in unchanged.

Each is `#![forbid(unsafe_code)]`, mirrors the `length` parse/resolve shape,
and stays Rust-unit-tested (cascade/paint integration lands when Stylo /
WebRender plug in).

**Note on Stylo sourcing.** Stylo is now published on crates.io as
[`stylo`](https://crates.io/crates/stylo) (lib name `style`); the
"needs a Servo git dependency" caveat from earlier revisions of this
plan no longer applies. See ADR-011.

**Gate:** `vixen-headless --url fixtures/css/at-property.html
--extract-selector '[style]'` returns correctly cascaded styles.
vixen-wpt runs the CSS fixtures; pass rate recorded as baseline.
(Selector surface green today; cascade surface pending step 3.)

---

## Phase 4 — Layout (≈ 1–2 weeks)

Turn cascade output into a positioned box tree.

**Steps:**

1. Add Servo `layout_2020` crate to `vixen-engine` (per ADR-001 and
   `docs/REFERENCES.md`). Confirm dependency closure is workable; if
   `layout_2020` coverage proves too sparse for real sites, narrow the
   v1.0 layout scope and document the gap in `docs/COMPAT.md` rather
   than swapping crates.
2. `vixen-engine/src/layout.rs`: feed the cascade output + DOM into the
   layout engine, produce a positioned box tree.
3. CSS Grid, Flexbox, block layout, all `position` values, scroll
   containers — all from the upstream `layout_2020` crate.

**Gate:** Visual-hash WPT check on 20+ fixtures matches reference
baseline within tolerance. Specifically, nested-flex/grid + padding +
margins + gaps must produce correct absolute coordinates *without* any
post-pass coordinate fixup (`layout_2020` doesn't need it).

**Pure-logic foundation landed (Phase 4 prep).**
`vixen-engine::box_model` implements the CSS2 § 10.3.3 block-level
horizontal-constraint solve (`auto`-width leftover absorption, one/two
`auto`-margin distribution + centering, `box-sizing: border-box` content
subtraction) and the four-box nesting (`margin ⊃ border ⊃ padding ⊃
content`) the layout tree feeds off. Pure given cascade-resolved edges;
Rust-unit-tested per "Rust tests cover only pure logic".

**Flexbox main-axis resolution landed (Phase 4 prep).**
`vixen-engine::flex_resolve` implements CSS Flexbox 1 § 9.7 "Resolving
Flexible Lengths" end-to-end: the used-flex-factor selection (grow if items
under-fill, shrink otherwise), the inflexible-item freeze step, the
proportional free-space distribution (scaled by `shrink × flex_basis` for
the shrink case), and the iterative min/max-violation clamping that
terminates when every item is frozen. Pure given cascade-resolved
`flex-basis` + `grow`/`shrink` + `min`/`max` per item; cross-axis (alignment
+ line packing) stays in `layout_2020` where it composes against real text
metrics.

---

## Phase 5 — Paint: WebRender + EGL surfaceless (≈ 2 weeks)

Make the engine produce pixels via a single WebRender paint path bound
to two `GlContext` implementations.

**Steps:**

1. `vixen-engine/src/paint.rs`: single `DisplayList` type + a WebRender
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

   **Invariant enforcement landed (pure slice).**
   `vixen-engine::display_list` implements all eight `SPEC.md` "Display-list
   invariants" as auditable, individually-tested pure functions
   (`z_tier`, `effective_opacity`, `background_paint_rect`, …) plus a
   `DisplayListBuilder::build` that emits the pruned, z-sorted
   `PaintCommand` stream. The WebRender `Renderer` (this step, next slice)
   consumes that stream; the invariant logic is done and Rust-unit-tested.
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

**Paint-geometry pure-logic foundations landed (Phase 5 prep).**
- `vixen-engine::transform` — CSS Transforms 1 § 13 2D affine algebra:
  `translate`/`scale`/`rotate`/`skew`/`matrix`, `multiply` composition
  (post-multiply ⇒ rightmost-applied-first, matching Firefox/Servo),
  `apply_point`/`apply_rect` (AABB), `determinant`/`inverse`, plus a
  `parse_transform` list parser for the `--computed-style` projection.
  Consumes `vixen-engine::angle` so the full angle unit grammar is shared.
- `vixen-engine::border_radius` — CSS Backgrounds 3 § 5.5 corner shaping:
  the eight authored radii → four shaped corners with the proportional
  scale-down when adjacent radii overflow a side. Pure given px radii +
  px sizes; the cascade resolves percentages first.
- `vixen-engine::gradient` — CSS Images 4 § 4.5 linear-gradient colour
  sampling: stop-position normalisation (first/last defaults, even
  auto-distribution between positioned anchors, monotonicity fix-up, unit-
  interval clamp), linear-sRGB interpolation between stops (via
  `crate::color::interpolate`), and the `repeating-linear-gradient()` wrap
  that tiles the colour function. Angle / direction → gradient-line
  geometry stays in the paint path.
- `vixen-engine::box_shadow` — CSS Backgrounds 3 § 7.2 `box-shadow` geometry:
  the `<shadow>#` grammar parser (offset / blur / spread / colour /
  `inset`, the paren-respecting colour-function tokeniser, negative-blur
  clamping) + the per-shadow paint-rect arithmetic (`outer_paint_rect` for
  display-list culling; `inset_clip_rect` for the inset "hole" with the
  spec's spread-sign-flip + blur-shrinks-hole rule). Pure given px values;
  the cascade resolves percentages / `em` first.
- `vixen-engine::background_position` — CSS Backgrounds 3 § 3.6 +
  § 4.2 `<position>` resolution: the four-value grammar (1/2/3/4 forms,
  keyword / length / percentage mix), the keyword-axis swap rule (`top
  right` ≡ `right top`), and the § 4.2 formula `(container − image) *
  fraction + offset`. Pure given px sizes; the cascade resolves the
  `background-origin`-selected container size first.
- `vixen-engine::stacking_context` — CSS 2.1 § 9.9.1 + CSS Positioned Layout
  3 § 6 + CSS Compositing 1 § 3 stacking-context formation predicate +
  the seven-layer § App. E.2.1 paint-order classification (`classify_descendant`
  slots each descendant into one of `ContextBackgroundAndBorders` /
  `NegativeZChildren` / `InFlowBlockLevel` / `NonPositionedFloats` /
  `InFlowInlineLevel` / `PositionedZeroZ` / `PositiveZChildren`, in
  bottom-to-top paint order). Composes with `display_list::z_tier` for the
  coarse z-bucketing and gives the paint pass the fine-grained in-flow
  layering the CSS 2.1 appendix specifies.

All six `#![forbid(unsafe_code)]`, Rust-unit-tested, ready for WebRender to
consume once the display-list builder feeds them in.

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

**Pure-logic foundation landed for Events + Forms + Storage (Phase 6 prep).**
- `vixen-engine::event_path` — `composedPath()` (shadow-boundary aware via the
  `composed` flag) and the focus-transition ordering
  `focusout → focusin → blur → focus` (bubbling flags per SPEC). The host-hook
  layer invokes these; the ordering is done and unit-tested.
- `vixen-engine::date_units` — the date/time canonical-unit parser
  (`forms.rs` "lives in `date_units` until a proper parser lands" → landed):
  `date`/`time`/`week`/`month`/`datetime-local` → `DateTimeUnit`, so
  `stepMismatch` is now testable end-to-end over real input strings.
- `vixen-engine::storage_key` — Web Storage key/value validation (non-empty
  key, no NUL bytes, ≤ `MAX_KEY_LEN`/`MAX_VALUE_LEN`) + the `(origin, kind)`
  `StoragePartition` key the `vixen-store` redb tables partition under, plus
  the per-partition `StorageQuota` (5 MiB / 8 192 entries) the host hooks
  report `QuotaExceededError` against.
- `vixen-engine::form_submission` — the three WHATWG HTML § 4.10.21 form-
  submission encoders (`application/x-www-form-urlencoded`,
  `multipart/form-data`, `text/plain`) plus the `FormEntry` / `FormEntryValue`
  data model + `FormEnctype` selector. The URL-encoder uses the URL Standard's
  space→`+` + uppercase-hex percent-encoding; the multipart encoder handles
  RFC 7578 § 4.2 `Content-Disposition` quoting + `filename` + `Content-Type`
  per part, with CRLF discipline; the boundary generator is RFC 2046-capped.
- `vixen-engine::dataset` — WHATWG HTML § 3.2.6.9 `data-*` attribute ↔ dataset
  property-name bidirectional mapping (deserialise, serialise, collect),
  with the anti-collision rule (`-` followed by uppercase ⇒ not exposed).
- `fixtures/forms/validation.html` — exercises every form pseudo-class
  `style_dom` resolves today (`:checked`/`:disabled`/`:enabled`/`:required`/
  `:optional`/`:read-only`/`:read-write`); wired into `fixtures/manifest.json`.
- `fixtures/dom/dataset.html` — exercises the canonical `data-foo-bar` →
  `fooBar` surface the host-hook layer will reflect; wired into
  `fixtures/manifest.json`.
- `fixtures/forms/submission.html` — fixes the form-DOM input shape the
  three encoders will walk; wired into `fixtures/manifest.json`.

**Pure-logic foundation landed for the `DOMTokenList` surface (Phase 6 prep).**
- `vixen-engine::class_list` — WHATWG HTML § 4.6.4 `DOMTokenList` + the
  § 2.7.3 "ordered set of unique space-separated tokens" parser +
  validator (empty ⇒ `SyntaxError`; ASCII-whitespace-bearing ⇒
  `InvalidCharacterError`). The full mutating surface (`add` / `remove` /
  `toggle` with the `force` parameter / `replace` with the
  drop-old-if-new-already-present edge case / `contains` / `item` /
  `iter` / `serialize`) with the spec's atomic validate-then-mutate rule
  (any invalid token in a multi-token `add`/`remove` aborts the whole
  call without partial mutation). The optional `SupportedTokens` set is
  the surface `<link>.relList.supports(token)` consults (the only
  `DOMTokenList` with a supported-tokens set per WHATWG § 4.6.5).
- `fixtures/dom/class-list.html` — exercises the canonical classList
  patterns the host-hook layer reflects (duplicate-token collapse,
  whitespace-run collapse, the case-sensitive `Foo`/`foo`/`FOO`
  distinction, the multi-value `<link rel>` form); wired into
  `fixtures/manifest.json`.

**Pure-logic foundation landed for Network host hooks (Phase 6 prep).**
- `vixen-engine::url_search_params` — WHATWG URL Standard `URLSearchParams`
  (§ 5.2 parse + § 5.3 serialize) plus the full mutating surface
  (`get`/`getAll`/`has`/`has_pair`/`append`/`set`/`delete`/`delete_pair`/
  `sort`/`entries`/`keys`/`values`) the `new URLSearchParams()` JS host hook
  reflects. The parser handles leading-`?` stripping, `+`→SPACE,
  percent-decode with U+FFFD on ill-formed UTF-8, and empty-tuple dropping;
  the serializer shares the `application/x-www-form-urlencoded` byte set with
  `form_submission::encode_urlencoded` (kept separate because the specs are).
- `vixen-engine::mime` — WHATWG MIME Sniffing § 2.1 `MimeType::parse` + § 2.2
  `serialize` + the `essence()` accessor. Tolerant whitespace + case handling,
  quoted-string parameter values (RFC 9110 § 3.2.6 backslash-pair escaping),
  first-occurrence-wins on duplicate parameter names. Every network layer
  (`Content-Type`), `fetch()`/`XHR` (`.type`/`overrideMimeType`), and
  `<object>`/`<embed>` plugin negotiation consults this one parser.
- `vixen-engine::text_codec` — WHATWG Encoding API (`TextEncoder` +
  `TextDecoder`). `encode_into` reports UTF-16-code-unit `read` + byte
  `written` without splitting a scalar value; `decode` does the BOM sniff
  (`ignoreBOM` opt-out), the `fatal`-flag UTF-8 validation, the WHATWG § 4.6
  one-U+FFFD-per-maximal-subpart replacement (via `from_utf8_lossy`, which
  agrees with the WHATWG count), and the § 7.1 `CRLF`/lone-`CR` → `LF` line-
  break normalisation. v1 ships UTF-8 only; unknown labels fail closed.

**Pure-logic foundation landed for the fetch host-hook data model (Phase 6 prep).**
The `Headers` object + `AbortController`/`AbortSignal` primitives the
`fetch()` / `XMLHttpRequest` / streaming host hooks reduce to. Both
`#![forbid(unsafe_code)]`, Rust-unit-tested.
- `vixen-engine::headers` — Fetch § 3.2.2 `Headers` object data model:
  [`validate_header_name`] (RFC 9110 § 5.5 `token` + lowercasing) +
  [`validate_header_value`] (OWS trim, NUL/CRLF rejection, code-point-`≤ U+00FF`
  gating); the § 3.2.2 forbidden predicates [`is_forbidden_request_header`]
  (the exact 21-name list + the `proxy-`/`sec-` prefix rules the Request
  constructor strips) + [`is_forbidden_response_header_name`]
  (`set-cookie`/`set-cookie2`); the § 3.2.1.2 CORS-safelist predicate
  [`is_cors_safelisted_request_header`] (the `Accept`/`Accept-Language`/
  `Content-Language`/`Content-Type`(+`Range`) family with the value-byte cap,
  the CORS-unsafe-byte gate, and the MIME-essence + `Range` grammar checks);
  and the normalised [`Headers`] store (append/set/get/getAll/delete/has +
  comma-combine on read + byte-order + insertion-order iteration).
- `vixen-engine::abort` — DOM § 8.1 `AbortController`/`AbortSignal`: the
  `aborted` + `reason` value model (default reason = `"AbortError"`
  `DOMException`), [`AbortController::abort`] (idempotent, first-reason-wins),
  [`abort_any`] (§ 8.1.3.2 `AbortSignal.any()` snapshot — aborted iff any input
  is, taking the first-aborted input's reason; reactive propagation is the
  host-hook event-loop layer's job), and [`TimeoutSignal`] (§ 8.1.3.2
  `AbortSignal.timeout(ms)` request record with the zero-delay-aborts-
  synchronously rule).

[`validate_header_name`]: ../../crates/vixen-engine/src/headers.rs
[`validate_header_value`]: ../../crates/vixen-engine/src/headers.rs
[`is_forbidden_request_header`]: ../../crates/vixen-engine/src/headers.rs
[`is_forbidden_response_header_name`]: ../../crates/vixen-engine/src/headers.rs
[`is_cors_safelisted_request_header`]: ../../crates/vixen-engine/src/headers.rs
[`Headers`]: ../../crates/vixen-engine/src/headers.rs
[`AbortController::abort`]: ../../crates/vixen-engine/src/abort.rs
[`abort_any`]: ../../crates/vixen-engine/src/abort.rs
[`TimeoutSignal`]: ../../crates/vixen-engine/src/abort.rs

**Pure-logic foundation landed for the Performance API + viewport adaptation (Phase 6 prep).**
The `performance.now()` monotonic-clock + `<meta name=viewport>` primitives
the timing host hooks and the mobile layout layer reduce to. Both
`#![forbid(unsafe_code)]`, Rust-unit-tested.
- `vixen-engine::high_res_time` — High Resolution Time § 4:
  [`DOMHighResTimeStamp`] (`f64` ms), the per-global [`TimeOrigin`] (ms since
  Unix epoch that `performance.now()` is relative to), the § 4.4
  [`MonotonicClock`] (non-decreasing across calls + clamped to `≥ 0`), the
  § 4.4 [`coarsen`] effective-time-value coarsening (floor to `100µs` unless
  cross-origin isolated), and the `performance.now()` → Unix-epoch conversion
  (`timeOrigin + now`) the legacy `PerformanceTiming` surface reduces to.
- `vixen-engine::viewport_meta` — WHATWG HTML § 9.3 `<meta name="viewport">`
  `content` parser: the comma-separated `<name>=<value>` declaration set
  (`width`/`height` device-keyword or CSS-px number, `initial-scale`/
  `minimum-scale`/`maximum-scale` clamped to `[0.1, 10]`, `user-scalable`
  yes/no, `viewport-fit` auto/contain/cover). Names ASCII-case-insensitive,
  values use the lenient leading-numeric-prefix extraction, unknown properties
  ignored. The CSS Device Adaptation 1 § 10 defaulting (width=980, &c.) stays
  in the layout layer; this module captures the authored declaration set.

[`DOMHighResTimeStamp`]: ../../crates/vixen-engine/src/high_res_time.rs
[`TimeOrigin`]: ../../crates/vixen-engine/src/high_res_time.rs
[`MonotonicClock`]: ../../crates/vixen-engine/src/high_res_time.rs
[`coarsen`]: ../../crates/vixen-engine/src/high_res_time.rs

**Pure-logic foundation landed for URLPattern (Phase 6 prep).**
- `vixen-engine::url_pattern` — URLPattern API § 2 pathname pattern compile +
  match: the routing primitive client-side routers, service-worker
  `FetchEvent` routing, and the `new URLPattern()` host hook reduce to.
  [`URLPattern::compile`] parses the pathname-grammar subset (literal
  segments, `:name` named captures with the `[A-Za-z_][A-Za-z0-9_]*` name
  rule, `*` rest-of-path wildcard) with duplicate-name detection + the
  wildcard-must-be-trailing rule; [`URLPattern::match_pathname`] is a
  full-match (segment-based, empty-segment-collapsing so `/posts` ≡
  `/posts/`, `:name` captures one non-empty segment, `*` captures the rest
  joined by `/`). The `protocol`/`hostname`/`port`/`search`/`hash` components
  + full-regex custom params (`:name(\\d+)`) land with the host hook; the
  named/`*` subset covers real routing.

[`URLPattern::compile`]: ../../crates/vixen-engine/src/url_pattern.rs
[`URLPattern::match_pathname`]: ../../crates/vixen-engine/src/url_pattern.rs

**Pure-logic foundation landed for HTML attribute microsyntaxes + `data:`/`srcset` URLs (Phase 6 prep).**
- `vixen-engine::microsyntax` — the WHATWG HTML § 2.4 "common parser idioms"
  every attribute-value reflection reduces to: `parse_signed_integer`
  (§ 2.4.4) and `parse_non_negative_integer` (§ 2.4.3) with saturating
  overflow so `colspan`/`rowspan`/`tabindex`/`cols`/`maxlength` never panic;
  `parse_float` (§ 2.4.5) — the lenient leading-numeric-prefix extractor
  (`"100px"` → `100.0`, `"3e999"` → `+∞`) that `<input type=number>` and the
  `value sanitization algorithm` build on; `parse_dimension_value` (§ 2.4.6)
  — the legacy `<td width>` / `<img width>` surface producing either a pixel
  length or a percentage; and `parse_list_of_integers` for `<area coords>`.
  Every HTML attribute-value parser here is deliberately lenient (leading
  whitespace skipped, trailing content ignored for the float surface) per
  the spec's documented browser contract; the stricter value-sanitisation
  layers a trailing-garbage check *on top* of these primitives.
- `vixen-engine::srcset` — WHATWG HTML § 4.8.4.6 "Parsing a srcset attribute":
  the comma-separated image-candidate-string splitter + the § 4.8.4.7
  `Nw`/`Nx` descriptor validator (`Descriptor::Width`/`Density`). Candidates
  carrying ≥ 3 whitespace-separated tokens (a URL can't hold two
  descriptors) and candidates with an unparseable descriptor are dropped per
  spec; survivors keep document order (the § 4.8.4.8 selection algorithm
  prefers the first match on ties). The responsive-image selection step
  itself (composing candidates with the viewport DPR + `<img sizes>`) lands
  with the resource-fetch layer in Phase 1/6.
- `vixen-engine::data_url` — RFC 2397 `data:` URL parsing: the
  case-insensitive scheme check, the `;base64` flag (final-parameter form),
  the mediatype defaulting rules (omitted ⇒ `text/plain;charset=US-ASCII`;
  parameters-only ⇒ `text/plain` + authored parameters), and the payload
  decode (standard-alphabet base64 with ASCII-whitespace skipping + missing-
  padding tolerance, or RFC 3986 § 2.1 percent-decode). The Fetch standard
  does *not* MIME-sniff `data:` URLs, so the declared mediatype is exposed
  verbatim. Base64 decoding uses the vetted `base64` crate (pure-Rust,
  `unsafe`-free), shared by `vixen-engine` (data URLs) and `vixen-net` (CSP
  hash sources); the percent decoder is hand-rolled.
- `fixtures/dom/srcset.html` — exercises every `<img srcset>` / `<source
  srcset>` authoring form the parser handles (width descriptors, density
  descriptors, the bare-URL form, the `<picture>`/`<source>` art-direction
  combination); wired into `fixtures/manifest.json`.

**Pure-logic foundation landed for responsive-image selection (Phase 6 prep).**
The `srcset` parser left the § 4.8.4.8 selection step itself as a TODO; the
family is now complete end-to-end.
- `vixen-engine::media_query` — CSS Media Queries 4 condition evaluation: a
  recursive-descent parser for the `<media-condition>` tree (§ 3) over
  parenthesised `<media-feature>`s (§ 4) with `and`/`or`/`not` combinators,
  the `<media-type>` prefix (`screen`/`print`/`all` with `not`/`only`), and
  the `min-`/`max-` prefix decode into a `Range` constraint (`min-width` ≡
  `width >=`). `MediaQuery::matches` evaluates against a `Viewport` (CSS-px
  width/height, DPR, derived orientation, colour depth, hover/pointer,
  `prefers-color-scheme`, `prefers-reduced-motion`). The § 4 features
  implemented: `width`/`height`/`aspect-ratio`/`orientation`/`resolution`/
  `color`/`hover`/`pointer`/`prefers-color-scheme`/`prefers-reduced-motion`,
  with the § 4.3 boolean form (`(hover)`, `(color)`) and the
  `<general-enclosed>` fail-closed rule (unknown ⇒ `false`).
- `vixen-engine::source_size` — WHATWG HTML § 4.8.4.7 "Parsing a sizes
  attribute": the `<source-size-list>` splitter + per-entry validator. The
  final comma-separated entry is the unconditional default (§ 4.8.4.8: the
  last entry always provides a fallback when reached); a non-last entry
  without a media-condition is a parse error and the whole list falls back to
  the spec's `100vw` default. `resolve_px(&Viewport)` walks the entries in
  document order and returns the first match's length in CSS px.
- `vixen-engine::responsive_select` — WHATWG HTML § 4.8.4.8 "Selecting an image
  source": composes a parsed `srcset` with a resolved source size + the
  viewport DPR. Computes per-candidate pixel density (width ÷ source-size for
  `Nw`, the `x` value for density, implicit `1x` for bare), rejects mixed
  width/density lists (§ 4.8.4.6 parse error), keeps candidates with
  `density ≥ DPR` (falling back to all if that empties the list), and picks
  the smallest surviving density (ties → document order). The `select_source`
  helper walks the `<picture>`/`<source media>` art-direction list: the first
  `<source>` whose media query matches the viewport wins, else the `<img>`
  srcset selects.
- `fixtures/dom/sizes.html` — exercises every `<img sizes>` / `<source media>`
  authoring form (mobile-first + three-tier conditional lists, the bare-length
  default, em-based sizes, the `<picture>` art-direction surface with
  min/max-width and orientation media queries); wired into
  `fixtures/manifest.json`.

**Pure-logic foundation landed for CSS value-resolution + easing (Phase 3/6 prep).**
The calculation + timing-function primitives the cascade (`calc()` reduction,
`var()` substitution, custom-property resolution) and the transition/animation
drivers (`animation-timing-function`) reduce to.
- `vixen-engine::calc` — CSS Values 4 § 10 `calc()` / `min()` / `max()` /
  `clamp()` arithmetic tree + evaluator. A recursive-descent parser produces
  a `CalcNode` AST (`Number` / `Length` / `Percent` / `Add`/`Sub`/`Mul`/`Div` /
  the § 10.1 `Min`/`Max`/`Clamp` math functions); `evaluate` runs the § 10.7
  "argument resolution" pass with full dimension type-checking (`+`/`-`
  require homogeneous operands; `*` requires a number operand; `/` requires a
  number divisor; violations are hard errors). Lengths and percentages mix in
  the classic `calc(50% + 10px)` form, resolving to `(px, percent)` against a
  `LengthContext`. Operator precedence (`*`/`/` over `+`/`-`) and nested
  parenthesised grouping enforced; bare expressions (no `calc()` wrapper) parse
  too, so the `--computed-style` projection re-resolves the unwrapped form.
- `vixen-engine::easing` — CSS Easing 1 § 2-4: the timing-function family that
  maps an input progress (`0..1`) to an output progress. `Easing::parse`
  covers the keyword aliases (`linear`/`ease`/`ease-in`/`ease-out`/`ease-in-out`
  /`step-start`/`step-end`) and the function forms (`cubic-bezier()`,
  `steps()`, `linear()`); `Easing::evaluate` projects cubic-bezier control
  points via Newton-Raphson (8 iterations) with a bisection fallback so it
  converges on every valid curve (incl. overshoot spring curves where the
  y-coordinates exceed `[0, 1]`), implements the § 4.1 step jump-position
  rules (`jump-start`/`jump-end`/`jump-none`/`jump-both` with the
  `jump-none`-requires-`n ≥ 2` validation), and piecewise-linearly
  interpolates the `linear()` multi-stop function (explicit percentage
  positions + the § 3.1 implicit even-distribution rule).

**Pure-logic foundation landed for CSS generated content (Phase 5/6 prep).**
The counter-scope + marker-text primitives the `content` property
(`counter()`/`counters()`), `list-style-type`, and the `::marker` box reduce
to. Both `#![forbid(unsafe_code)]`, Rust-unit-tested.
- `vixen-engine::list_marker` — CSS Lists 3 § 6.1 `<list-style-type>` marker
  text: the predefined counter-style family (`disc`/`circle`/`square` bullet
  glyphs, `decimal`/`decimal-leading-zero` numeric, the `lower-alpha`/
  `upper-alpha` (+ `lower-latin`/`upper-latin` aliases) bijective base-26
  alphabetic, `lower-roman`/`upper-roman` additive, `lower-greek` over the
  24-letter alphabet, `none`). [`ListStyleType::render`] is the `value → text`
  projection per the § 6.1 algorithm table; the § 6.1 fallback rule (out-of-range
  additive/alphabetic values fall back to `decimal`, the default fallback) is
  enforced so a counter value never fails to produce a marker. Aliases
  normalise to the canonical name at parse so the round-trip is canonical.
- `vixen-engine::counter` — CSS2 § 12.4 counter scoping (reset/increment/set,
  with the per-kind default value — `0` for reset/set, `1` for increment) +
  CSS Lists 3 § 5 `counter()` / `counters()` resolution. [`parse_counter_ops`]
  tokenises the `counter-*` declaration value (ASCII-whitespace-separated
  `<custom-ident>` optionally followed by one `<integer>`, the `none` no-op,
  saturating integer overflow, the `--foo` CSS-variable reservation rejected);
  [`resolve_counter`] reads the innermost in-scope value (or `None` → empty
  marker per § 5); [`resolve_counters`] joins every in-scope value
  outermost→innermost with the delimiter string (`"1.1"`, `"1.3.2"`). The DOM
  traversal that pushes/pops scopes + applies the ops in document order stays
  in the Phase 4 layout layer; this module is the pure resolution primitive
  given the already-walked scope stack, and composes with `list_marker` via
  [`render_counter`].

[`ListStyleType::render`]: ../../crates/vixen-engine/src/list_marker.rs
[`parse_counter_ops`]: ../../crates/vixen-engine/src/counter.rs
[`resolve_counter`]: ../../crates/vixen-engine/src/counter.rs
[`resolve_counters`]: ../../crates/vixen-engine/src/counter.rs
[`render_counter`]: ../../crates/vixen-engine/src/counter.rs

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

**Pure-logic foundation landed (Phase 7 prep).**
- `vixen-net::referrer_policy` — Fetch § 3.4 `Referrer-Policy` parser
  (last-known directive wins) + § 4.3.7 `resolve_referrer` covering every
  policy branch (downgrade suppression, same-origin gating, origin-only,
  strict-origin-when-cross-origin default) + the `is_potentially_trustworthy`
  test the downgrade rules reduce to. The network layer attaches the resolved
  `Referer` once wired.
- `vixen-net::strict_transport_security` — RFC 6795 § 6.1 HSTS header
  parser (case-insensitive directives, tolerant whitespace, header ignored
  without valid `max-age`, `max-age=0` cache-deletion signal) + § 8.2
  `HstsEntry::matches` (exact host or, with `includeSubDomains`, a dot-prefixed
  subdomain — the superdomain rule is one-way).
- `vixen-net::cors` — Fetch § 3.2.1 `Access-Control-*` response-header
  parser (case-insensitive names, lowercased + de-duplicated lists, repeated
  origin header first-wins), § 4.1.5 `cors_check` (wildcard + credentials
  forbidden, specific-origin string equality, `null`-origin echo), and
  § 4.1.6 `cors_filtered_headers` (safelist of 7 response headers + named
  exposes, with `Set-Cookie`/`Set-Cookie2` always stripped). The script→fetch
  host hook consults this at every cross-origin response.
- `vixen-net::mixed_content` — W3C Mixed Content L1 § 3 verdict
  (`NotMixed`/`Block`/`Upgrade`) the fetch layer applies at every subresource
  fetch out of a secure context. [`ResourceType`] collapses the fetch
  destination to the three modal categories (active=block, passive=upgrade,
  navigation=allow); `block-all-mixed-content` CSP overrides upgrades.
  Reuses `referrer_policy::is_potentially_trustworthy` for the request-URL
  secure-transport test.
- `fixtures/security/cors-headers.html` — exercises the HTML surface
  (`crossorigin`, `integrity`, `nonce`) the host-hook layer dispatches on
  when constructing the cross-origin fetch; wired into `fixtures/manifest.json`.
- `fixtures/network/mixed-content.html` — exercises every mixed-content
  surface (http:// scripts/stylesheets/iframe/object vs. images/audio/video
  vs. top-level navigation, plus https:// counterparts); wired into
  `fixtures/manifest.json`.

**Pure-logic foundation landed for `<iframe sandbox>` (Phase 7 prep).**
- `vixen-net::sandboxing` — WHATWG HTML § 4.8.5 sandbox-flag parser (the
  full `allow-*` keyword set: forms / modals / orientation-lock /
  pointer-lock / popups / popups-to-escape-sandbox / presentation /
  same-origin / scripts / top-navigation + the user-activation +
  custom-protocols variants / downloads / storage-access /
  unsafe-downloads). Tokenised on ASCII whitespace, case-insensitive,
  unknown flags ignored, empty value ⇒ most-restrictive. The derived
  security predicates the script/navigation/storage layers consult:
  `implies_unique_origin` (the § 4.8.5 opaque-origin rule), and
  `is_dangerous_scripts_plus_same_origin` (the famous "if both
  `allow-scripts` and `allow-same-origin` are present, the sandbox is
  escapable" warning the spec mandates).
- `fixtures/security/sandbox.html` — exercises every `sandbox` variant
  the parser handles (empty / scripts-only / scripts+same-origin
  dangerous combination / top-nav family / popups family / mixed legacy
  flags / unknown-token tolerance / case-insensitivity); wired into
  `fixtures/manifest.json`.

**Pure-logic foundation landed for `Sec-Fetch-*` + Permissions Policy (Phase 7 prep).**
- `vixen-net::sec_fetch` — Fetch § 3.1 `Sec-Fetch-*` request-metadata parsing:
  [`SecFetchSite`] / [`SecFetchMode`] / [`SecFetchDest`] / [`SecFetchUser`]
  typed enums (case-sensitive token parse, fail-closed to [`Default`] on
  unknown values) + a bundled [`SecFetchHeaders::parse`] over a `(name,
  value)` iterator (case-insensitive names, last-wins combine). The § 3.2.4
  [`classify_site`] classifier resolves the embedder↔target relationship
  (`same-origin` / `same-site` / `cross-site` / `none`) the fetch layer
  attaches and that servers consult for the § 3.2 Cross-Origin gates; the
  `same-site` registrable-domain comparison uses the last-two-labels
  heuristic (documented limitation; the PSL lands when the cookie `domain`
  matcher needs it too). `SecFetchDest::is_navigation` / `is_embed` predicate
  the § 4.4 navigation and § 3.2 COEP checks.
- `vixen-net::permissions_policy` — Permissions Policy 1 § 3.3
  `Permissions-Policy` response-header parser + the § 5.2 `<iframe allow>`
  attribute parser. The [`Allowlist`] enum covers every § 3.3 source-list
  form (`Everyone *` / `Self_ self` / `Src src` / `Origins(list)` /
  `None ()`-deny-all); [`PermissionsPolicy::allows`] is the § 4 evaluation
  the host hooks consult before exposing `navigator.geolocation`/`camera`/
  &c. (features not in the policy default to embedder-only per § 3.3). The
  structured-field parser is paren/quote-aware (handles
  `geolocation=(self "https://partner.test")` and the iframe shorthand
  `camera 'self'`), tolerant of whitespace, and drops malformed items per
  the spec's "parse error ⇒ item dropped" rule.
- `fixtures/security/permissions-policy.html` — exercises every `<iframe
  allow>` authoring form (bare feature names, the `self`/`src` keywords,
  explicit origin lists, the empty `()` deny-all, the camera/geolocation/
  microphone/fullscreen/autoplay family); wired into `fixtures/manifest.json`.

[`SecFetchSite`]: ../../crates/vixen-net/src/sec_fetch.rs
[`SecFetchMode`]: ../../crates/vixen-net/src/sec_fetch.rs
[`SecFetchDest`]: ../../crates/vixen-net/src/sec_fetch.rs
[`SecFetchUser`]: ../../crates/vixen-net/src/sec_fetch.rs
[`classify_site`]: ../../crates/vixen-net/src/sec_fetch.rs
[`SecFetchHeaders::parse`]: ../../crates/vixen-net/src/sec_fetch.rs
[`Allowlist`]: ../../crates/vixen-net/src/permissions_policy.rs
[`PermissionsPolicy::allows`]: ../../crates/vixen-net/src/permissions_policy.rs

**Pure-logic foundation landed for the WebSocket protocol boundary (Phase 6/7 prep).**
- `vixen-net::websocket` — RFC 6455 pure-logic boundary: [`compute_accept`] (§ 4.2.2
  `Sec-WebSocket-Accept` = `base64(SHA1(key + GUID))`, via the `sha1` crate —
  already transitively present), [`validate_client_handshake`] (§ 4.1 the
  server-side `Upgrade`/`Connection`/`Sec-WebSocket-Version: 13`/16-byte-key
  enforcement) + [`validate_server_response`] (§ 4.2.2 the client-side
  `101` + Accept-matches-sent-key check), [`parse_frame_header`] (§ 5.2 the
  2–14-byte frame decoder — FIN/RSV/opcode/mask/length, with the § 5.2
  reserved-RSV/opcode rejection + the non-canonical-length rule + the § 5.5
  control-frame `≤ 125` bytes + FIN-set invariants), [`apply_mask`] (§ 5.3 the
  XOR demask) + [`validate_close_code`] (§ 7.4 the status-code range + reserved-
  band rule). The framed TCP+TLS transport + the `WebSocket` JS host hook sit
  on top; `permessage-deflate` is deferred.

[`compute_accept`]: ../../crates/vixen-net/src/websocket.rs
[`validate_client_handshake`]: ../../crates/vixen-net/src/websocket.rs
[`validate_server_response`]: ../../crates/vixen-net/src/websocket.rs
[`parse_frame_header`]: ../../crates/vixen-net/src/websocket.rs
[`apply_mask`]: ../../crates/vixen-net/src/websocket.rs
[`validate_close_code`]: ../../crates/vixen-net/src/websocket.rs

**Gate:** Every security test in `vixen-net` and `vixen-engine` green.
Zero `cargo audit` advisories. Fuzz targets stable.

---

## Phase 8 — Headless CDP + tooling polish (≈ 1 week)

Implement the full headless tool surface.

**Steps:**

1. Implement CDP server (tokio + tokio-tungstenite) in `vixen-headless`.
   Command handlers call into `vixen-engine` via the `EngineInspector`
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
