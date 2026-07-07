# Vixen build plan

Phased execution runbook. Each phase ends in a green test suite, a
working binary, and a measured size. Do not start the next phase until
the previous one's gate passes.

Tick-tock discipline applies throughout: each phase is a *tick*
(capability lands); the post-phase cleanup is the *tock* (dead-code
removal, module ≤ 1 kLOC, references cited). See `docs/ACCEPTANCE.md`
for the per-phase gates.

For the executable vertical slice order, use [`docs/MILESTONES.md`](MILESTONES.md):
large browser features should extend `vixen_engine::page::Page` and prove the
slice with a `just gate-*` command, not land only as isolated prep modules.
For larger alpha/dev batches, follow [`docs/DEVELOPMENT.md`](DEVELOPMENT.md):
partial capability is acceptable only when it is visible, tested, fail-closed,
and bounded by a named maintainability follow-up.

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
5. `justfile` adapted: `check-all-host` builds the workspace; `test-api`
   runs the API crate; `gate-phase0` bundles the phase's executable proof.
6. `.gitignore`, `LICENSE` (Apache 2.0), `data/`, `build-aux/` skeleton,
   `fixtures/` (empty), `benches/` (empty).
7. `.mise.toml` pins the dev toolchain (`rust`, `just`, `cargo-binstall`) so
   `mise bootstrap --yes` converges a fresh machine by delegating project work
   to `just setup`. The library MSRV (1.88) is in each crate's
   `rust-version`; the developer toolchain is pinned in `.mise.toml`. The
   **GNOME 50 SDK is not installed
   on the host** — it is managed inside a flatpak-builder container
   (`just flatpak-update-sdk` / `just flatpak-build`); see
   [`docs/guidance/gnome-sdk-flatpak-builder.md`](guidance/gnome-sdk-flatpak-builder.md)
   and [`mise bootstrap`](https://mise.jdx.dev/bootstrap.html).

**Gate:** `just gate-phase0` passes (the workspace builds and `vixen-api` DTO /
trait tests pass). The shell's
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

**Gate:** `just gate-phase1` passes (`vixen-net` / `vixen-store`, `just audit`,
and the `just fuzz-security` 1 M iteration targets).

---

## Phase 2 — JavaScript runtime (≈ 1 week)

Stand up the JS engine.

**Steps:**

1. **`deno_core` implementation landed:** `deno_core`/V8 powers
   `vixen-engine::script::JsRuntime` and the Phase 2 eval gate.
2. **Stable Vixen seam:** keep the public `JsRuntime` / `JsValue` seam so
   headless, CDP, and Page tests do not depend on runtime internals.
3. Host hook registration, minimum viable: `console.log`, `fetch`
   (delegating to `vixen-net::Network`), `document.title` getter.
   Defer the full DOM/Event/Storage surface to Phase 6.
4. Host internals follow ADR-014: package Vixen-owned Web API bindings as
   `deno_core` extensions — small feature modules, explicit registration, local
   JS bootstrap, resource/permission boundaries, and focused tests per host
   family.

**Gate:** `just gate-phase2` passes (basic engine tests and
`vixen-headless --url file:///.../hello.html --eval '1+2'` returns `3`). Binary
size recorded.

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
   The shared `vixen-engine::page::Page` facade now owns URL + parsed document
   state for headless and WPT; cascade/layout/paint slices extend that facade
   in order.
3. **Milestone 1 computed-style cascade projection (done) —
   `Page::computed_style`** maps the stable selector `node_id` back to the
   element and returns a compact author/inline cascade. `vixen-engine::style_cascade`
   loads `<style>` blocks, matches selectors through Stylo's selector engine,
   applies specificity, source order, cascade layers, media/supports conditions,
    custom-property `var()` resolution, inherited custom properties, CSS-wide
    keywords, and author/inline `!important`, and keeps the WPT `computed-style`
    check vertical behind `Page`. `Page::evaluate_dom_expression` also projects
    small CSSOM smoke seams for `getComputedStyle(document.querySelector(...))`,
    `CSS.supports()`, `document.styleSheets`, and read-only
    CSSStyleRule/CSSStyleDeclaration state while full computed values and
    stylesheet host objects land.
4. `vixen-engine/src/style.rs` (next slice): replace the compact projection
   with full Stylo style data: load `<style>` / `<link rel=stylesheet>` into
   `Stylesheet` list → `Stylist::update_stylist`
   → cascade via Stylo's `SharedStyleContext` / traversal. Expose
   `computed_values_for(NodeId) -> Arc<ComputedValues>`. Requires
   implementing the full `TNode` / `TElement` / `TDocument` traits;
   budget 3–4 days for trait conformance. Consult
   `.tmp/ref/firefox/dom/base/` for DOM API behavior and
   `.tmp/ref/firefox/servo/components/style/dom.rs` for the Stylo trait
   definitions being implemented.
5. CSS-wide keywords, `@layer`, `@supports`, `@media`, and custom properties +
   `var()` are now covered in the compact `Page::computed_style` projection for
   the v1 layout/paint seam. Full Stylo still owns the long tail (`@property`,
   `@import`, `@keyframes`, shorthand expansion, full computed-value
   serialisation). Verify via WPT fixtures.

**Pure-logic foundation landed (testing-strategy item).**
`vixen-engine::length` implements CSS Values 4 `<length>` parsing + the
absolute/relative unit conversions the cascade and layout resolves against
(`px`/`em`/`rem`/`%`/`vh`/`vw`/`vi`/`vb`/`vmin`/`vmax`/`sv*`/`lv*`/`dv*`/
`ex`/`ch`/`pt`/`pc`/`in`/`cm`/`mm`/`Q`).
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

**Gate:** `just gate-phase3` passes; `fixtures/css/computed-advanced.html`
proves the Milestone 1 cascade seam (`@media`, `@supports`, `@layer`, inherited
custom properties, `var()` fallback, and CSS-wide keywords) through the WPT
`computed-style` check. Full Stylo computed values remain the implementation
replacement behind the same `Page` facade (step 4), not a new public seam.

---

## Phase 4 — Vixen-owned Rust layout (≈ 4–8 weeks for v1 subset)

Turn cascade output into a positioned box tree.

**Steps:**

1. Build Vixen's Rust layout engine per ADR-013. The architecture reference is
   Ladybird LibWeb at `0de15a5dd2a9`, especially
   `.tmp/ref/ladybird/Libraries/LibWeb/Layout/TreeBuilder.cpp` and
   `.tmp/ref/ladybird/Libraries/LibWeb/Layout/*FormattingContext*`.
2. `vixen-engine/src/layout_tree.rs`: convert the Stylo-computed DOM into an
   arena-backed layout tree with stable `LayoutNodeId`s, explicit dirty bits,
   and no cross-crate pointers.
3. `vixen-engine/src/layout.rs` / `formatting_context.rs`: run block, inline,
   flex, and grid formatting-context passes over that tree and produce
   positioned fragments.
4. Feed positioned fragments into the existing display-list builder; layout
   never owns a paint backend.
5. Tables, floats, full vertical writing, page fragmentation, and advanced
   intrinsic sizing are post-v1 unless a WPT/real-site gate promotes them.

**Implementation crate note.** Keep the layout engine Vixen-owned, but use
small helper crates where they reduce risk without taking over web layout
semantics: `smallvec` for common-case child/fragment lists, `bitflags` for
dirty/invalidation state, `slotmap`/`thunderdome` if raw arena ids become
error-prone, and `euclid` when replacing ad-hoc geometry with typed units.
Defer text-specific crates (`rustybuzz`, `fontdb`, `unicode-linebreak`,
`unicode-bidi`, `unicode-segmentation`) until the inline formatting slice.
Do not use generic UI layout engines (`taffy`, `stretch`, etc.) for CSS layout
without a new ADR.

**Vertical layout-tree + fragment slice landed.** `vixen-engine::layout_tree` now builds
the first arena-backed Vixen layout tree behind `Page::layout_tree`, and
`vixen-headless --dump-layout-tree` exposes a deterministic dump. The first
block formatting-context slice consumes cascade-projected `width`/`height`,
`margin`, `border-width`/`border`, `padding`, and `box-sizing` through the
existing `box_model` resolver, so authored block dimensions now affect node
boxes. The existing `Page::dump_lines` projection derives visible text from the
tree instead of raw body text, keeping the line-layout and paint surfaces on the
same spine. `Page::layout_fragments(viewport)` now projects block backgrounds
and wrapped text lines from that tree into paint-consumable fragments; the
display-list builder consumes that seam instead of re-walking layout nodes.
Next slices replace the deterministic average-width text metric with styled
glyph fragments and enrich grid/inline placement without changing the CLI seam.

**Gate:** Visual-hash WPT check on 20+ fixtures matches reference
baseline within tolerance. Specifically, nested-flex/grid + padding +
margins + gaps must produce correct absolute coordinates *without* any
post-pass coordinate fixup. `docs/COMPAT.md` records the achieved WPT profile
for the shipped subset.

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
`flex-basis` + `grow`/`shrink` + `min`/`max` per item. Cross-axis alignment
and line packing stay in Vixen's formatting-context pass where they compose
against real text metrics.

**CSS Grid track sizing landed (Phase 4 prep).**
`vixen-engine::grid_resolve` implements CSS Grid 1 § 12.5 "Distribute
Extra Space" + § 11.7 "Maximize Tracks" — the natural complement to
`flex_resolve` for grid columns / rows. [`GridTrack`] carries the
§ 11.2 min track size (caller-resolved definite base) + the § 11.3 growth
limit + the `Nfr` flex factor; [`resolve_tracks`] distributes the
container's leftover to flex tracks proportionally to their flex factor,
freezes any track that hits its growth limit, redistributes the excess to
the remaining flex tracks (iterative, the same freeze-on-violation pattern
`flex_resolve` uses), then grows non-flex tracks up to their growth limits
equally when leftover remains (§ 11.7). The constructors ([`GridTrack::fr`]
for `1fr`, [`GridTrack::minmax`] for `minmax(min, max, fr)`,
[`GridTrack::length`] for fixed) cover the common authoring forms. Pure
given definite base sizes; content-based sizing (`min-content`/`max-content`/
`auto`) and multi-track spanning items stay in Vixen's formatting-context pass
where they compose against real text-shaping (the caller folds each spanning
item's contribution into the track `base` before calling).

[`GridTrack`]: ../../crates/vixen-engine/src/grid_resolve.rs
[`GridTrack::fr`]: ../../crates/vixen-engine/src/grid_resolve.rs
[`GridTrack::minmax`]: ../../crates/vixen-engine/src/grid_resolve.rs
[`GridTrack::length`]: ../../crates/vixen-engine/src/grid_resolve.rs
[`resolve_tracks`]: ../../crates/vixen-engine/src/grid_resolve.rs

**Pure-logic foundation landed for CSS Writing Modes + logical properties (Phase 4 prep).**
The `writing-mode` / `direction` → block + inline axis + the logical →
physical side mapping the box model, the logical insets, the logical-size →
width/height swap, and the flex/grid main-axis selection resolve against.
`#![forbid(unsafe_code)]`, Rust-unit-tested.
- `vixen-engine::writing_modes` — CSS Writing Modes 3 § 3 + CSS Logical
  Properties 1. [`WritingMode`] is the five § 3.1 values (`horizontal-tb` /
  `vertical-rl` / `vertical-lr` + the CSS WM 4 `sideways-rl` / `sideways-lr`);
  [`Direction`] is the § 2.1 `ltr` / `rtl` inline-base direction. [`Flow`]
  bundles the pair and projects the derived geometry: [`Flow::block_axis`] /
  [`Flow::inline_axis`] (which physical axis each logical axis runs along) +
  [`Flow::block_start`] / [`Flow::block_end`] / [`Flow::inline_start`] /
  [`Flow::inline_end`] → [`PhysicalSide`] (the § 7 side mapping table, with
  the `sideways-*` reusing the `vertical-*` axis mapping per § 3.1).
  [`LogicalSize::to_physical`] swaps `inline`/`block` → `width`/`height` for
  vertical modes; [`LogicalInsets::to_physical`] resolves the four logical
  edges to `(top, right, bottom, left)`; [`LogicalRect::to_physical`]
  resolves a layout-produced logical rect to a physical `(x, y, w, h)` rect
  given the containing block (the rtl / vertical-rl inline-start flip from
  the right/bottom edge folded in). The `unicode-bidi` algorithm + the
  `text-orientation` glyph rotation stay in the text-shaping / paint path;
  this module is the pure axis + side mapping.

[`WritingMode`]: ../../crates/vixen-engine/src/writing_modes.rs
[`Direction`]: ../../crates/vixen-engine/src/writing_modes.rs
[`Flow`]: ../../crates/vixen-engine/src/writing_modes.rs
[`Flow::block_axis`]: ../../crates/vixen-engine/src/writing_modes.rs
[`Flow::inline_axis`]: ../../crates/vixen-engine/src/writing_modes.rs
[`Flow::block_start`]: ../../crates/vixen-engine/src/writing_modes.rs
[`Flow::block_end`]: ../../crates/vixen-engine/src/writing_modes.rs
[`Flow::inline_start`]: ../../crates/vixen-engine/src/writing_modes.rs
[`Flow::inline_end`]: ../../crates/vixen-engine/src/writing_modes.rs
[`PhysicalSide`]: ../../crates/vixen-engine/src/writing_modes.rs
[`LogicalSize::to_physical`]: ../../crates/vixen-engine/src/writing_modes.rs
[`LogicalInsets::to_physical`]: ../../crates/vixen-engine/src/writing_modes.rs
[`LogicalRect::to_physical`]: ../../crates/vixen-engine/src/writing_modes.rs

**Pure-logic foundation landed for CSS Multi-column resolution (Phase 4 prep).**
The `column-width` / `column-count` / `column-gap` § 3.4 resolution the
layout layer's column-row distribution reduces to. `#![forbid(unsafe_code)]`,
Rust-unit-tested.
- `vixen-engine::multicol` — CSS Multi-column Layout 1 § 3. [`ColumnWidth`]
  (`auto` or px) + [`ColumnCount`] (`auto` or ≥ 1) + the [`ColumnSpec`]
  `(column-width, column-count, gap)` triple. [`ColumnSpec::resolve`] runs
  the § 3.4 pseudo-algorithm end-to-end: the four branches (both auto ⇒
  single column; count set + width auto ⇒ even distribution; width set +
  count auto ⇒ `⌊(avail+gap)/(width+gap)⌋` count; both set ⇒
  `min(count, fit)` + the § 3.4 (11)–(12) single-column-authored-wider-
  than-available clamp), with a final `max(0, width)` guard so a too-large
  count never produces a negative column. [`ResolvedColumns::column_x`] is
  the `i * (column_width + gap)` stride the box model feeds off;
  [`ResolvedColumns::total_width`] + [`ResolvedColumns::overflows`] report
  the row geometry (the gaps-alone-overflow case). The `column-gap: normal`
  → `1em` length resolution, the § 8 `column-fill: balance` height
  balancing, the `column-rule` paint, and `column-span: all` stay in Vixen's
  formatting-context / paint path (they compose against real text metrics).

[`ColumnWidth`]: ../../crates/vixen-engine/src/multicol.rs
[`ColumnCount`]: ../../crates/vixen-engine/src/multicol.rs
[`ColumnSpec`]: ../../crates/vixen-engine/src/multicol.rs
[`ColumnSpec::resolve`]: ../../crates/vixen-engine/src/multicol.rs
[`ResolvedColumns::column_x`]: ../../crates/vixen-engine/src/multicol.rs
[`ResolvedColumns::total_width`]: ../../crates/vixen-engine/src/multicol.rs
[`ResolvedColumns::overflows`]: ../../crates/vixen-engine/src/multicol.rs

**Pure-logic foundation landed for CSS Scroll Snap (Phase 4 prep).**
The § 5 snap-position computation + the `scroll-snap-type` axis/strictness
model the scroll container's snap targeting reduces to.
`#![forbid(unsafe_code)]`, Rust-unit-tested.
- `vixen-engine::scroll_snap` — CSS Scroll Snap 1 § 5. [`ScrollSnapType`]
  (`none` or `(axis, strictness)`; axis `x`/`y`/`block`/`inline`/`both`,
  strictness `proximity`/`mandatory`, parsed in either order per the § 5.1
  grammar) + [`SnapAlign`] (`none`/`start`/`end`/`center`, the 1–2 value
  `(block, inline)` form) + [`SnapStop`] (`normal`/`always`).
  [`compute_axis`] is the § 5 snap position for one axis: the
  `start ⇒ O`, `end ⇒ O + A − S`, `center ⇒ O + A/2 − S/2` formula clamped
  to `[0, max(0, overflow − S)]`. [`compute_snap`] produces the `(x, y)`
  pair (the block/inline → x/y mapping via the writing-mode flow flag);
  [`should_snap`] is the strictness policy (mandatory always; proximity iff
  within a threshold). The scrollable-overflow computation, the scroll
  animation, the `scroll-padding`/`scroll-margin` insets, and the
  content-change resnap (§ 5.4) stay in the layout/input layers.

[`ScrollSnapType`]: ../../crates/vixen-engine/src/scroll_snap.rs
[`SnapAlign`]: ../../crates/vixen-engine/src/scroll_snap.rs
[`SnapStop`]: ../../crates/vixen-engine/src/scroll_snap.rs
[`compute_axis`]: ../../crates/vixen-engine/src/scroll_snap.rs
[`compute_snap`]: ../../crates/vixen-engine/src/scroll_snap.rs
[`should_snap`]: ../../crates/vixen-engine/src/scroll_snap.rs

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

   **Vertical display-list slice landed.** `Page::display_list` now turns the
   Phase 4 layout fragments into the single `DisplayListBuilder` command stream:
   viewport background first, then fragment-backed backgrounds/text commands,
   exposed through `vixen-headless --dump-display-list`. `--paint-stats` now
   aggregates command counts and painted area from that same stream. This is not
   a renderer or CPU paint fallback; WebRender consumes the same `PaintCommand`
   stream once the GL surfaces land.
5. `vixen-shell/src/engine_factory.rs`: creates the `gtk4::GLArea`,
   wraps it as `GlAreaSurface` (the shell's `GlContext` impl), and
   returns it as the content widget alongside the tab's `EngineWorker`.
   The worker's engine renders to the screen via that `GlContext`.

   **Surface scaffolding landed.** `vixen-shell::surface::GlAreaSurface`
   (behind `gtk-shell`) and `vixen_headless::surface::SurfacelessSurface` now
   implement `vixen_api::GlContext`. Headless construction still fails closed
   with `unsupported.screenshot` until EGL context creation, WebRender command
   submission, `glReadPixels`, and PNG encoding land; no CPU fallback or second
   paint path was introduced.
6. CI: verify `LIBGL_ALWAYS_SOFTWARE=1` produces working screenshots
   via `llvmpipe` so headless runs anywhere.

**Pure-logic foundation landed for radial + conic gradients (Phase 5 prep).**
The CSS Images 4 § 4.2.3 + § 4.3.3 colour-sampling siblings of `gradient`,
completing the three gradient families the paint path samples against. All
three `#![forbid(unsafe_code)]`, Rust-unit-tested, reusing the
[`crate::gradient::resolve_stop_positions`] + linear-sRGB interpolation the
linear-gradient surface already owns.
- `vixen-engine::radial_gradient` — CSS Images 4 § 4.2.3 `radial-gradient`.
  [`RadialShape`] (`Circle`/`Ellipse`) + [`RadialSize`] (the four § 4.2.4
  keywords `closest-side`/`farthest-side`/`closest-corner`/`farthest-corner`
  + the explicit `Length`/`LengthPair` forms, with `farthest-corner` the
  spec default). [`compute_radius`] is the § 4.2.4 radius-resolution step
  for one of the four keyword forms against a known `(width, height)`
  reference box centred at `(cx, cy)`, returning `(rx, ry)` so circle +
  ellipse share the call site (the `closest-corner`/`farthest-corner`
  ellipse cases keep the closest-side/farthest-side `rx/ry` ratio and scale
  to the corner per the § 4.2.4 corner-scaling rule). [`project_to_t`] is
  the per-pixel `(dx, dy)` → `t` distance projection (Euclidean for circle,
  ellipse-norm for ellipse). [`RadialGradient::sample`] is the colour at a
  projected `t` (with the `repeating-radial-gradient()` wrap via the shared
  [`sample_resolved`] helper). The `<position>` centre and the
  `<geometry-box>` reference-box resolution stay in the layout/paint layer;
  this module receives `(cx, cy, width, height)` already resolved.
- `vixen-engine::conic_gradient` — CSS Images 4 § 4.3.3 `conic-gradient`.
  [`ConicGradient`] carries the stop list + the `from <angle>` start angle
  (radians) + the `repeating` flag. [`project_angle_to_t`] is the per-pixel
  `(dx, dy)` → `t ∈ [0, 1)` projection (CSS-clockwise from 12 o'clock, in
  turns — one full revolution = `1.0`); [`add_from_angle`] folds in the
  `from` angle and reduces modulo 1.0. [`ConicGradient::sample`] is the
  colour at a projected `t` (with the `repeating-conic-gradient()` wrap).
  The `<angle>` grammar + the `<position>` centre stay in the cascade /
  layout layer.

[`RadialShape`]: ../../crates/vixen-engine/src/radial_gradient.rs
[`RadialSize`]: ../../crates/vixen-engine/src/radial_gradient.rs
[`compute_radius`]: ../../crates/vixen-engine/src/radial_gradient.rs
[`project_to_t`]: ../../crates/vixen-engine/src/radial_gradient.rs
[`RadialGradient::sample`]: ../../crates/vixen-engine/src/radial_gradient.rs
[`ConicGradient`]: ../../crates/vixen-engine/src/conic_gradient.rs
[`project_angle_to_t`]: ../../crates/vixen-engine/src/conic_gradient.rs
[`add_from_angle`]: ../../crates/vixen-engine/src/conic_gradient.rs
[`ConicGradient::sample`]: ../../crates/vixen-engine/src/conic_gradient.rs
[`sample_resolved`]: ../../crates/vixen-engine/src/gradient.rs
[`crate::gradient::resolve_stop_positions`]: ../../crates/vixen-engine/src/gradient.rs

**Pure-logic foundation landed for CSS Geometry Interfaces (Phase 5/6 prep).**
The `DOMPoint` / `DOMRect` / `DOMQuad` / `DOMMatrix` value family the
geometry-bearing host hooks reduce to. `#![forbid(unsafe_code)]`,
Rust-unit-tested, complementing [`crate::transform`] (which owns the 2D
subset of the matrix surface).
- `vixen-engine::geometry` — CSS Geometry Interfaces L1. [`DOMPoint`] is
  the 2D/3D/homogeneous `(x, y, z, w)` point (§ 2; the perspective divide
  normalises `w` to `1` when projecting). [`DOMRect`] is the
  `(x, y, width, height)` rectangle (§ 3) with the derived
  `top`/`right`/`bottom`/`left` accessors + the negative-dimension
  [`DOMRect::normalized`] flip + the `contains_point` / `intersects` /
  `union` predicates `getBoundingClientRect()` + `IntersectionObserver`
  consult. [`DOMQuad`] is the four-corner quadrilateral (§ 4) with the
  `from_rect` constructor + the [`DOMQuad::bounds`] axis-aligned bounding
  rectangle (§ 4.4). [`DOMMatrix`] is the § 6 4×4 homogeneous matrix (the
  2D `matrix(a,b,c,d,e,f)` subset folds into the upper-left 2×3 + the
  `[0 0 1 0]`/`[0 0 0 1]` bottom rows) with every § 6.3 transform
  (`translate`/`scale`/`scale_non_uniform`/`rotate`/`rotate_axis_angle`/
  `skew_x`/`skew_y`/`multiply`/`flip_x`/`flip_y`/`inverse`) + the § 6.4
  [`DOMMatrix::transform_point`] homogeneous-coordinate projection + the
  `is_2d` predicate + the `to_4x4_column_major` round-trip. Matrix
  decomposition / interpolation (the CSS Transforms 2 § 16 pipeline the
  animation interpolation layer reduces to) and the full `transform`
  property parser land with the 3D WebRender plumbing; this module is the
  arithmetic those slices reduce to.
- `Page::evaluate_dom_expression` now exposes the first geometry host seam:
  `Element.getBoundingClientRect()` returns the Page layout box as a DOMRect
  projection (`x`/`y`/`width`/`height`/`left`/`top`/`right`/`bottom`), and
  `getClientRects().length` reports whether layout produced a box. It also
  projects the read-only Geometry Interfaces value constructors
  (`DOMPoint`, `DOMRect.fromRect()`, `DOMQuad.fromRect()` / `getBounds()`, and
  `DOMMatrix` transform/`transformPoint()` smoke) until real JS host wrappers
  replace the string projection.

[`DOMPoint`]: ../../crates/vixen-engine/src/geometry.rs
[`DOMRect`]: ../../crates/vixen-engine/src/geometry.rs
[`DOMRect::normalized`]: ../../crates/vixen-engine/src/geometry.rs
[`DOMQuad`]: ../../crates/vixen-engine/src/geometry.rs
[`DOMQuad::bounds`]: ../../crates/vixen-engine/src/geometry.rs
[`DOMMatrix`]: ../../crates/vixen-engine/src/geometry.rs
[`DOMMatrix::transform_point`]: ../../crates/vixen-engine/src/geometry.rs
[`crate::transform`]: ../../crates/vixen-engine/src/transform.rs

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

**Paint compositing pure-logic foundations landed (Phase 5 prep).**
The pixel-mixing family the paint path's `mix-blend-mode` / `filter` /
`border-image` surfaces reduce to. All three `#![forbid(unsafe_code)]`,
Rust-unit-tested, consuming `vixen-engine::color`'s linear-sRGB arithmetic.
- `vixen-engine::blend` — CSS Compositing 1 § 5 + § 10: the 13 Porter-Duff
  compositing operators ([`blend::CompositingOperator`] with the § 5.1
  general formula + per-operator Fa/Fb factors) and the 16 § 10 blend modes
  ([`blend::BlendMode`] — `normal` + 11 separable § 10.1 + 4 non-separable
  § 10.2, with the `SetLum`/`SetSat`/`ClipColor` helpers). [`blend::composite`]
  evaluates one operator; [`blend::blend`] applies one mode to a pixel;
  [`blend::composite_blend`] runs the § 5.2 combined pipeline (isolation
  blend against the backdrop, then the Porter-Duff operator) that
  `mix-blend-mode` actually performs. All arithmetic is in linear sRGB via
  [`blend::LinColor`] (reusing `color::Color::to_linear_f32`).
- `vixen-engine::filter` — CSS Filter Effects 1 § 5: the `<filter-function-
  list>` grammar + the per-pixel colour-matrix family. [`filter::FilterList`]
  parses a chain (tolerant of parenthesised-argument whitespace); the 10 § 5
  functions (`blur`/`brightness`/`contrast`/`drop-shadow`/`grayscale`/
  `hue-rotate`/`invert`/`opacity`/`saturate`/`sepia`) carry their § 5
  default-argument rules. The per-pixel family folds into one
  [`filter::ColorMatrix`] (SVG `feColorMatrix`-shaped 4×5) via
  [`filter::compose_color_matrix`] so the paint path runs a single matrix
  multiply per pixel; `blur`/`drop-shadow` keep their geometry for the
  paint path's spatial pass (`drop-shadow` reuses `box_shadow::BoxShadow`).
- `vixen-engine::border_image` — CSS Backgrounds 3 § 6: the four longhands
  (`border-image-slice`/`-width`/`-outset`/`-repeat`) with full 1–4 TRBL
  expansion + parse, the 3×3 nine-region carving
  ([`border_image::source_regions`] / [`border_image::destination_regions`]),
  and the `border-image-repeat` tiling primitive ([`border_image::tile_edge`]
  — `stretch`/`repeat`/`round`/`space`, with the `round` integer-count
  rescale and the `space` even-gap distribution).

[`blend::CompositingOperator`]: ../../crates/vixen-engine/src/blend.rs
[`blend::BlendMode`]: ../../crates/vixen-engine/src/blend.rs
[`blend::composite`]: ../../crates/vixen-engine/src/blend.rs
[`blend::blend`]: ../../crates/vixen-engine/src/blend.rs
[`blend::composite_blend`]: ../../crates/vixen-engine/src/blend.rs
[`blend::LinColor`]: ../../crates/vixen-engine/src/blend.rs
[`filter::FilterList`]: ../../crates/vixen-engine/src/filter.rs
[`filter::ColorMatrix`]: ../../crates/vixen-engine/src/filter.rs
[`filter::compose_color_matrix`]: ../../crates/vixen-engine/src/filter.rs
[`border_image::source_regions`]: ../../crates/vixen-engine/src/border_image.rs
[`border_image::destination_regions`]: ../../crates/vixen-engine/src/border_image.rs
[`border_image::tile_edge`]: ../../crates/vixen-engine/src/border_image.rs

**Pure-logic foundation landed for clip-path + mask (Phase 5 prep).**
The masking family the paint path's per-pixel clip + the masked-element
alpha/luminance sampling reduce to. Both `#![forbid(unsafe_code)]`,
Rust-unit-tested, consuming [`crate::border_radius`] + [`crate::blend`].
- `vixen-engine::clip_path` — CSS Masking 1 § 5 `clip-path` basic shapes.
  [`ClipPath`] is the typed family ([`ClipPath::Inset`] /
  [`ClipPath::Circle`] / [`ClipPath::Ellipse`] / [`ClipPath::Polygon`] /
  [`ClipPath::None`]); [`Coord`] is the `at <position>` coordinate (px /
  percent / keyword) with [`Coord::resolve`] against a reference box;
  [`GeometryBox`] is the `<geometry-box>` reference selector.
  [`parse_clip_path`] parses the four basic shapes (case-insensitive
  function name, parenthesised args, the `inset(… round <radius>)` form
  reuses [`BorderRadius`], the `polygon(<fill-rule>, …)` form carries
  [`FillRule::NonZero`] / [`FillRule::EvenOdd`]). [`ClipPath::contains`] is
  the point-in-shape test the paint path calls per pixel — the inset corner
  rounding via quarter-ellipse containment, the polygon winding rules
  (non-zero + even-odd, the SVG § 8.4 ray-crossing algorithm). The `path()`
  SVG-path form is deferred (the four geometric shapes cover the common
  HTML surface).
- `vixen-engine::mask` — CSS Masking 1 § 6 `mask` shorthand per-layer
  model. [`MaskMode`] (`alpha`/`luminance`/`match-source`), [`MaskRepeat`]
  (the 6 repeat styles, `repeat-x`/`repeat-y` collapsed), [`MaskBox`] (the
  shared `mask-clip` + `mask-origin` keyword set, `no-clip` clip-only),
  and [`MaskLayer`] (one layer's resolved longhands). [`parse_mask`] splits
  comma-separated layers (paren-aware, so a gradient's commas don't split a
  layer), fills the per-longhand slots in any order, recognises the
  `<position> / <size>` slash form, and applies the "first unrecognised
  token is the image source" rule. The mask-image fetch + the per-pixel
  sampling is the paint path.

[`ClipPath`]: ../../crates/vixen-engine/src/clip_path.rs
[`ClipPath::Inset`]: ../../crates/vixen-engine/src/clip_path.rs
[`ClipPath::Circle`]: ../../crates/vixen-engine/src/clip_path.rs
[`ClipPath::Ellipse`]: ../../crates/vixen-engine/src/clip_path.rs
[`ClipPath::Polygon`]: ../../crates/vixen-engine/src/clip_path.rs
[`ClipPath::None`]: ../../crates/vixen-engine/src/clip_path.rs
[`ClipPath::contains`]: ../../crates/vixen-engine/src/clip_path.rs
[`Coord`]: ../../crates/vixen-engine/src/clip_path.rs
[`Coord::resolve`]: ../../crates/vixen-engine/src/clip_path.rs
[`GeometryBox`]: ../../crates/vixen-engine/src/clip_path.rs
[`parse_clip_path`]: ../../crates/vixen-engine/src/clip_path.rs
[`BorderRadius`]: ../../crates/vixen-engine/src/border_radius.rs
[`FillRule::NonZero`]: ../../crates/vixen-engine/src/clip_path.rs
[`FillRule::EvenOdd`]: ../../crates/vixen-engine/src/clip_path.rs
[`MaskMode`]: ../../crates/vixen-engine/src/mask.rs
[`MaskRepeat`]: ../../crates/vixen-engine/src/mask.rs
[`MaskBox`]: ../../crates/vixen-engine/src/mask.rs
[`MaskLayer`]: ../../crates/vixen-engine/src/mask.rs
[`parse_mask`]: ../../crates/vixen-engine/src/mask.rs
[`crate::border_radius`]: ../../crates/vixen-engine/src/border_radius.rs
[`crate::blend`]: ../../crates/vixen-engine/src/blend.rs

**Pure-logic foundation landed for the Web Animations timing model (Phase 5 prep).**
The § 5 timing-model pipeline the CSS `transition` / `animation` drivers +
the `Animation` / `KeyframeEffect` host hooks reduce to.
`#![forbid(unsafe_code)]`, Rust-unit-tested, consuming [`crate::easing`].
- `vixen-engine::animation` — Web Animations § 5. [`EffectTiming`] carries
  the § 5.4 timing properties (`delay` / `end_delay` / `fill` /
  `iteration_start` / `iterations` / `duration` / `direction`); [`Fill`] is
  the `none`/`forwards`/`backwards`/`both` fill mode; [`PlaybackDirection`]
  is the `normal`/`reverse`/`alternate`/`alternate-reverse` direction.
  [`active_duration`] + [`end_time`] are the § 5.3 derived times;
  [`phase`] is the § 5.5 `before`/`active`/`after` classification;
  [`simple_iteration_progress`] + [`current_iteration`] are the § 5.5
  iteration progress + index (the after-phase `iterations = 0` and
  integer-boundary `progress = 1` rules folded in); [`directed_progress`]
  is the § 5.6 direction-aware progress; [`apply_easing`] is the § 5.7
  transformed progress (consumes [`crate::easing::Easing`]);
  [`compute_timing`] ties the pipeline together into a [`ComputedTiming`]
  with the fill-mode before/after resolution (backwards/both ⇒ the
  iteration-0 start in before; forwards/both ⇒ the end state in after; else
  `None`). The keyframe value interpolation + the animation-frame
  scheduling + the `auto` duration resolution stay in the paint /
  event-loop layer (this module produces the `progress` they sample at).

[`EffectTiming`]: ../../crates/vixen-engine/src/animation.rs
[`Fill`]: ../../crates/vixen-engine/src/animation.rs
[`PlaybackDirection`]: ../../crates/vixen-engine/src/animation.rs
[`active_duration`]: ../../crates/vixen-engine/src/animation.rs
[`end_time`]: ../../crates/vixen-engine/src/animation.rs
[`phase`]: ../../crates/vixen-engine/src/animation.rs
[`simple_iteration_progress`]: ../../crates/vixen-engine/src/animation.rs
[`current_iteration`]: ../../crates/vixen-engine/src/animation.rs
[`directed_progress`]: ../../crates/vixen-engine/src/animation.rs
[`apply_easing`]: ../../crates/vixen-engine/src/animation.rs
[`compute_timing`]: ../../crates/vixen-engine/src/animation.rs
[`ComputedTiming`]: ../../crates/vixen-engine/src/animation.rs
[`crate::easing`]: ../../crates/vixen-engine/src/easing.rs
[`crate::easing::Easing`]: ../../crates/vixen-engine/src/easing.rs

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
- `Page::evaluate_dom_expression` now projects the read-only document/navigator
  state shape (`readyState`, `compatMode`, visibility, `documentURI`/`baseURI`,
  focus/active element, viewport/window/screen state, language/userAgent) plus
  an empty `localStorage`/`sessionStorage` smoke seam through the same storage-key
  validation boundary the persistent host object will use. The DOM query seam
  now also covers core node/ancestry properties (`nodeName`/`nodeType`,
  `isConnected`, `ownerDocument`, and `Element.closest()`) before full Node /
  Element JS host wrappers replace the string projection.
- `vixen-engine::script::JsRuntime::evaluate_with_page` now installs the first
  `deno_core` `document` snapshot host-object seam for focused evals:
  `document.title`, `document.body.textContent`, simple
  `querySelector`/`getElementById` element properties and attributes, and
  `querySelectorAll().length`, plus read-only `DOMTokenList` (`classList` /
  `relList` / `sandbox`) and `DOMStringMap` (`dataset`) property reads. The
  remaining broad DOM smoke surface still fails closed through
  `Page::evaluate_dom_expression` until each family moves behind real wrappers.
- `Page::evaluate_dom_expression` now projects constructor smoke for `Event` /
  `CustomEvent` and element `dispatchEvent(new Event(...))` so event-object and
  EventTarget wiring has fixture coverage before listener queues and full capture
  / bubble dispatch land.
- `vixen-engine::form_submission` — the three WHATWG HTML § 4.10.21 form-
  submission encoders (`application/x-www-form-urlencoded`,
  `multipart/form-data`, `text/plain`) plus the `FormEntry` / `FormEntryValue`
  data model + `FormEnctype` selector. The URL-encoder uses the URL Standard's
  space→`+` + uppercase-hex percent-encoding; the multipart encoder handles
  RFC 7578 § 4.2 `Content-Disposition` quoting + `filename` + `Content-Type`
  per part, with CRLF discipline; the boundary generator is RFC 2046-capped.
- `Page::evaluate_dom_expression` now reuses the same form entry-list builder
  for a read-only `FormData(form)` smoke seam (`get`/`getAll`/`has`, iterator
  first-entry shape, plus file `name`/`type`/`size`) before mutable `FormData`
  and submitter-aware form host objects land.
- `vixen-engine::dataset` — WHATWG HTML § 3.2.6.9 `data-*` attribute ↔ dataset
  property-name bidirectional mapping (deserialise, serialise, collect),
  with the anti-collision rule (`-` followed by uppercase ⇒ not exposed).
- `Page::evaluate_dom_expression` and the transitional `document` snapshot now
  use that same dataset mapper for the read-only smoke surface
  (`element.dataset.fooBar` / bracket access), proven by `fixtures/dom/dataset.html`
  `js-eval` checks while the WPT adapter continues to use the Page projection.
- `Page::evaluate_dom_expression` also projects `ValidityState` flags,
  `willValidate`, and `checkValidity()` / `reportValidity()` from the pure
  forms module for fixture smoke coverage until the Phase 6 form host objects
  land.
- `Page::evaluate_dom_expression` projects `Element.innerHTML` / `outerHTML`
  getters through `vixen-engine::html_serialize`, proving the HTML
  serialisation host-object seam with WPT `js-eval` checks before mutation
  setters and Trusted Types enforcement land.
- `Page::evaluate_dom_expression` also reflects simple security-relevant element
  properties (`HTMLMetaElement.content` / `.charset`) so CSP/referrer meta
  fixtures cover the DOM host seam before Phase 7 enforcement consumes those
  declarations.
- `fixtures/forms/validation.html` — exercises every form pseudo-class
  `style_dom` resolves today (`:checked`/`:disabled`/`:enabled`/`:required`/
  `:optional`/`:read-only`/`:read-write`) plus the Page-backed validity eval
  seam; wired into `fixtures/manifest.json`.
- `fixtures/dom/dataset.html` — exercises the canonical `data-foo-bar` →
  `fooBar` surface the host-hook layer will reflect; wired into
  `fixtures/manifest.json`.
- `fixtures/forms/submission.html` — fixes the form-DOM input shape the
  three encoders and `FormData` projection walk; wired into
  `fixtures/manifest.json`.

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
  distinction, the multi-value `<link rel>` form) and now asserts the
  read-only `classList` / `relList` eval seam; headless/CDP focused evals run
  through the current JS runtime seam while the WPT adapter continues to use the Page
  projection; wired into `fixtures/manifest.json`.
- `fixtures/security/sandbox.html` now also asserts the Page-backed
  `iframe.sandbox` DOMTokenList projection (`length`/`contains`/`item`), with
  focused headless/CDP evals now backed by runtime `DOMTokenList` snapshots,
  before real framed-document sandbox enforcement consumes the same tokens.

**Pure-logic foundation landed for Network host hooks (Phase 6 prep).**
- `vixen-engine::url_search_params` — WHATWG URL Standard `URLSearchParams`
  (§ 5.2 parse + § 5.3 serialize) plus the full mutating surface
  (`get`/`getAll`/`has`/`has_pair`/`append`/`set`/`delete`/`delete_pair`/
  `sort`/`entries`/`keys`/`values`) the `new URLSearchParams()` JS host hook
  reflects. The parser handles leading-`?` stripping, `+`→SPACE,
  percent-decode with U+FFFD on ill-formed UTF-8, and empty-tuple dropping;
  the serializer shares the `application/x-www-form-urlencoded` byte set with
  `form_submission::encode_urlencoded` (kept separate because the specs are).
- `Page::evaluate_dom_expression` now exposes a read-only `URL.canParse()` /
  `new URL()` / `URLSearchParams` smoke seam (`href`/`origin`/components /
  `toString()` / `searchParams`, record-list constructor, two-argument `has`,
  and first iterator entry/key/value shape) from `whatwg_url` +
  `url_search_params`, proven by `fixtures/network/url-parsing.html`.
- `vixen-engine::mime` — WHATWG MIME Sniffing § 2.1 `MimeType::parse` + § 2.2
  `serialize` + the `essence()` accessor. Tolerant whitespace + case handling,
  quoted-string parameter values (RFC 9110 § 3.2.6 backslash-pair escaping),
  first-occurrence-wins on duplicate parameter names. Every network layer
  (`Content-Type`), `fetch()`/`XHR` (`.type`/`overrideMimeType`), and
  `<object>`/`<embed>` plugin negotiation consults this one parser.
- `vixen-engine::text_codec` — WHATWG Encoding API (`TextEncoder` +
  `TextDecoder`). `encode_into` reports UTF-16-code-unit `read` + byte
  `written` without splitting a scalar value; the Page seam and the current
  runtime host-constructor pilot both parse the `TextDecoder`
  constructor label/options dictionary (`fatal`, `ignoreBOM`); `decode` does
  the BOM sniff (`ignoreBOM` opt-out), the `fatal`-flag UTF-8 validation, the
  WHATWG § 4.6 one-U+FFFD-per-maximal-subpart replacement (via
  `from_utf8_lossy`, which agrees with the WHATWG count), and the § 7.1
  `CRLF`/lone-`CR` → `LF` line-break normalisation. v1 ships UTF-8 only;
  unknown labels fail closed.
- The same fixture set now asserts the Page-backed `TextEncoder` /
  `TextDecoder`, `atob`/`btoa`, and `DOMParser.parseFromString(..., "text/html")`
  smoke seams (`encoding`, encoded byte length, `encodeInto` `read`/`written`,
  `TextDecoder` label/options, decode, base64 round-trip, parsed document query)
  while `vixen-headless --eval` / CDP `Runtime.evaluate` now exercise
  op-backed runtime `TextEncoder` / `TextDecoder` constructors plus focused
  `document` / `Element` / read-only `DOMTokenList` / `DOMStringMap` snapshot
  extension objects backed by the same Page data. The DOM snapshot data now
  crosses the `deno_core` op boundary via `op_vixen_dom_snapshot`; selector
  lookup and `Element.matches()` now use finer-grained DOM ops, and element
  record data is loaded through `op_vixen_dom_element_snapshot`. Element
  text/attribute reads plus read-only DOMTokenList/dataset data now use focused
  DOM ops. Focused `CSS.supports`, `getComputedStyle`, and
  CSSStyleSheet/CSSRule smoke evals now use the op-backed `script::cssom`
  extension, leaving geometry/forms/events/history/storage/fetch as the next
  host-object replacement targets.

**Pure-logic foundation landed for the fetch host-hook data model (Phase 6 prep).**
The `Headers` object, `Blob`/`File` metadata, read-only `Request`/`Response`
state, static `Response.error()` / `Response.redirect()`, and
`AbortController`/`AbortSignal` primitives are the sync data surfaces the
`fetch()` / `XMLHttpRequest` / streaming host hooks reduce to. The pure modules
remain `#![forbid(unsafe_code)]`, Rust-unit-tested.
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
- `Page::evaluate_dom_expression` now projects read-only `Headers`,
  `Blob`/`File`, `Request`, `Response`, `AbortController`/`AbortSignal`, and
  `URLPattern` smoke seams from these pure modules (`get`/`has`, iterator shape,
  forbidden-header filtering in Request/Response init, byte-size/type/name/
  method/status/header state, `Response.json()`, timeout/any snapshots, pathname
  test/exec groups) while mutable fetch and routing host objects land.

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
- `Page::evaluate_dom_expression` now projects Performance API smoke checks for
  `typeof performance.now()`, non-negative `performance.now()`, and monotonic
  `timeOrigin + now` shape through this pure clock model while the real
  per-global timer host object lands.
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
- The Page eval seam exposes that pathname subset through `new URLPattern({
  pathname })` `test()` / `exec().pathname.groups` smoke checks.

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
  width/height, DPR, derived orientation, output context (`screen`/`print`),
  colour depth, primary hover/pointer, aggregate `any-hover`/`any-pointer`,
  `prefers-color-scheme`, `prefers-reduced-motion`). The § 4 features
  implemented: `width`/`height`/`aspect-ratio`/`orientation`/`resolution`/
  `color`/`hover`/`pointer`/`any-hover`/`any-pointer`/
  `prefers-color-scheme`/`prefers-reduced-motion`,
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
  min/max-width, orientation, output-context, and aggregate input-device media
  queries); wired into
  `fixtures/manifest.json`.
- `Page::evaluate_dom_expression` now projects the read-only `<img>.currentSrc`
  smoke surface for plain `srcset`/`sizes` images from `responsive_select` plus
  a `matchMedia()` `.matches` / `.media` seam from `media_query`, proving
  selected-image URL reflection and MediaQueryList shape until the full
  `HTMLImageElement` / `MediaQueryList` host objects and resource fetch path land.

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

**Pure-logic foundation landed for structured clone + MessagePort (Phase 6 prep).**
The serialisation + entangled-port model `postMessage()`,
`new MessageChannel()`, worker `postMessage()`, `BroadcastChannel`, and
IndexedDB / `history.pushState()` reduce to. Both
`#![forbid(unsafe_code)]`, Rust-unit-tested, composing with the cross-origin-
isolation gate ([`coep::is_cross_origin_isolated`]) for `SharedArrayBuffer`
exposure.
- `vixen-engine::structured_clone` — HTML § 2.7.5 structured clone algorithm.
  [`StructuredCloneValue`] is the type-tagged tree of serialisable values
  (primitives, `Date`, `Array`, `Object`, `Map`, `Set`, `ArrayBuffer`,
  `MessagePort`, `Error` with the [`ErrorKind`] subclass family, and the
  `PlatformObject` slot reserved for `File`/`Blob`/`ImageData` &c.). [`clone`]
  deep-clones the tree honouring the transfer list: every transferred handle
  must be reachable ([`DataCloneError::UnreachableTransferable`]), the list
  may not carry duplicates ([`DataCloneError::DuplicateTransferable`]), a
  detached buffer is rejected ([`DataCloneError::DetachedTransferable`]), and
  a `SharedArrayBuffer` *clone* (not transfer) requires a cross-origin-
  isolated context ([`DataCloneError::SharedBufferRequiresIsolation`] — the
  gate `is_cross_origin_isolated` feeds). [`detach_transferred`] flips the
  transferred `ArrayBuffer`s to detached in the source tree; `SharedArrayBuffer`s
  stay shared. [`is_cloneable`] is the partial-check a host hook calls before
  walking (so a `DataCloneError` surfaces before any transfer side-effect).
  Shared-reference identity preservation (the spec's "memory" map) lives at
  the host hook where real JS object identities exist; this is the faithful
  tree-clone for tree inputs.
- `vixen-engine::message_port` — HTML § 9.5 `MessagePort` / `MessageChannel`.
  [`MessagePort`] is one end of an entangled pair (the [`PortId`] handle
  appears in `StructuredCloneValue::MessagePort` and the transfer list);
  [`MessageChannel::new`] constructs the pair. [`MessagePort::post_message`]
  runs the § 9.5.4 steps: structured-clone the value (honouring the transfer
  list), and return the clone + the partner id + the transferred ports in a
  [`PostOutcome`] (the host hook routes the enqueue to the partner — the two
  ports may live in different compartments / workers). [`MessagePort::enqueue`]
  / [`MessagePort::drain`] are the receiver-side inbox + the event-loop
  hand-off; `start()` / `close()` carry the § 9.5.3 / § 9.5.5 lifecycle (a
  detached port drops `postMessage` and drains nothing).
- `Page::evaluate_dom_expression` now projects a small `structuredClone()`
  smoke seam for primitive strings, arrays, shallow objects, Date, Map, Set, and
  Error name/message shape through the same clone function that `postMessage()` /
  history state will call.

[`StructuredCloneValue`]: ../../crates/vixen-engine/src/structured_clone.rs
[`ErrorKind`]: ../../crates/vixen-engine/src/structured_clone.rs
[`clone`]: ../../crates/vixen-engine/src/structured_clone.rs
[`DataCloneError::UnreachableTransferable`]: ../../crates/vixen-engine/src/structured_clone.rs
[`DataCloneError::DuplicateTransferable`]: ../../crates/vixen-engine/src/structured_clone.rs
[`DataCloneError::DetachedTransferable`]: ../../crates/vixen-engine/src/structured_clone.rs
[`DataCloneError::SharedBufferRequiresIsolation`]: ../../crates/vixen-engine/src/structured_clone.rs
[`detach_transferred`]: ../../crates/vixen-engine/src/structured_clone.rs
[`is_cloneable`]: ../../crates/vixen-engine/src/structured_clone.rs
[`MessagePort`]: ../../crates/vixen-engine/src/message_port.rs
[`PortId`]: ../../crates/vixen-engine/src/message_port.rs
[`MessageChannel::new`]: ../../crates/vixen-engine/src/message_port.rs
[`MessagePort::post_message`]: ../../crates/vixen-engine/src/message_port.rs
[`PostOutcome`]: ../../crates/vixen-engine/src/message_port.rs
[`MessagePort::enqueue`]: ../../crates/vixen-engine/src/message_port.rs
[`MessagePort::drain`]: ../../crates/vixen-engine/src/message_port.rs
[`coep::is_cross_origin_isolated`]: ../../crates/vixen-net/src/coep.rs

**Pure-logic foundation landed for Range + Selection (Phase 6 prep).**
The boundary-point model the `Range` / `Selection` host hooks + the
editing-command surface (`document.execCommand`, `beforeinput` dispatch)
reduce to. `#![forbid(unsafe_code)]`, Rust-unit-tested.
- `vixen-engine::range` — DOM § 5.2 `Range` + § 5.4 `Selection`.
  [`NodeRef`] is an opaque DOM-node handle carrying a [`DocumentOrder`]
  index (the pre-order DFS position the caller assigns) so two boundaries
  compare in document order by pure arithmetic. [`Boundary`] is the
  `(node, offset)` pair (child index for elements, UTF-16 index for text
  nodes); [`Boundary::compare`] is the § 5.2 relative position
  ([`Ordering::Before`] / [`Ordering::Equal`] / [`Ordering::After`]).
  [`Range`] carries the `(start, end)` pair with [`Range::new`] re-ordering
  to the § 5.2 `start ≤ end` invariant, [`Range::is_collapsed`] +
  [`Range::collapse`] + [`Range::contains_boundary`] +
  [`Range::intersect`]. [`Selection`] carries the `Range` list + the
  anchor/focus (direction-aware) + `add_range` / `collapse_to` /
  `extend_to` / `remove_all_ranges` + the [`SelectionDirection`]
  (`Forward` / `Backward` / `None` — the focus-before-anchor "backward"
  selection state). The live tree mutation (`surroundContents` /
  `insertNode` / `extractContents` — the § 5.3 algorithms) is the host
  hook; this module is the pure boundary model.
- `Page::evaluate_dom_expression` now projects the read-only initial Range /
  Selection smoke seam (`document.createRange().collapsed` / offsets and
  empty `getSelection()` accessors) through this pure model while the live DOM
  mutation and selection host objects remain the Phase 6 swap-in.

[`NodeRef`]: ../../crates/vixen-engine/src/range.rs
[`DocumentOrder`]: ../../crates/vixen-engine/src/range.rs
[`Boundary`]: ../../crates/vixen-engine/src/range.rs
[`Boundary::compare`]: ../../crates/vixen-engine/src/range.rs
[`Ordering::Before`]: ../../crates/vixen-engine/src/range.rs
[`Ordering::Equal`]: ../../crates/vixen-engine/src/range.rs
[`Ordering::After`]: ../../crates/vixen-engine/src/range.rs
[`Range`]: ../../crates/vixen-engine/src/range.rs
[`Range::new`]: ../../crates/vixen-engine/src/range.rs
[`Range::is_collapsed`]: ../../crates/vixen-engine/src/range.rs
[`Range::collapse`]: ../../crates/vixen-engine/src/range.rs
[`Range::contains_boundary`]: ../../crates/vixen-engine/src/range.rs
[`Range::intersect`]: ../../crates/vixen-engine/src/range.rs
[`Selection`]: ../../crates/vixen-engine/src/range.rs
[`SelectionDirection`]: ../../crates/vixen-engine/src/range.rs

**Pure-logic foundation landed for session history + pushState (Phase 6 prep).**
The HTML § 7.1 session-history entry-stack + the `history.pushState` /
`replaceState` / `back` / `forward` / `go` surface the `History` host hook
+ the navigation layer reduce to. `#![forbid(unsafe_code)]`,
Rust-unit-tested.
- `vixen-engine::history` — HTML § 7.1. [`ScrollRestoration`] is the
  `auto`/`manual` `history.scrollRestoration` mode; [`HistoryEntry`] is one
  session-history entry (URL string + opaque structured-clone `state` blob
  + the `scrollRestoration` mode + the optional title). [`SessionHistory`]
  is the entry stack + the current-entry cursor with the § 7.1 surface:
  `push` (truncates the forward branch per the § 7.1 "remove all entries
  after the current one" rule, appends, advances the cursor), `replace`
  (swaps the current entry, length unchanged), `back`/`forward`/`go(delta)`
  (cursor movement with out-of-range ⇒ no-op), `length`/`index`/`url`/
  `state`/`scroll_restoration`, and the `with_entries` escape hatch for the
  host hook that restores a persisted session history. The document
  load/unload for a traversal (the § 7.5 "traverse the history" algorithm),
  the same-origin URL check for `pushState`/`replaceState`, and the
  structured-clone serialisation of the `state` value stay in the
  navigation layer / host hook (the host hook serialises via
  [`crate::structured_clone`] before calling `pushState`).
- `Page::evaluate_dom_expression` now projects the read-only initial History
  smoke seam (`history.length`, `history.state`, `history.scrollRestoration`)
  from `SessionHistory::new(HistoryEntry::navigation(_))` before mutable
  `pushState`/traversal host objects land.

[`ScrollRestoration`]: ../../crates/vixen-engine/src/history.rs
[`HistoryEntry`]: ../../crates/vixen-engine/src/history.rs
[`SessionHistory`]: ../../crates/vixen-engine/src/history.rs
[`crate::structured_clone`]: ../../crates/vixen-engine/src/structured_clone.rs

**Pure-logic foundation landed for MutationObserver (Phase 6 prep).**
The DOM § 4.3 mutation-queue + the § 4.3.1 match predicate the
`MutationObserver` host hook + the microtask-delivery step reduce to.
`#![forbid(unsafe_code)]`, Rust-unit-tested.
- `vixen-engine::mutation_observer` — DOM § 4.3. [`MutationType`] is the
  three `childList`/`attributes`/`characterData` record types; [`MutationRecord`]
  is one record (target + added/removed nodes + siblings for `childList` +
  attribute name/namespace + `oldValue`); [`MutationObserverInit`] is the
  § 4.3.1 `observe()` options (`childList`/`attributes`/`attributeFilter`/
  `attributeOldValue`/`characterData`/`characterDataOldValue`/`subtree`).
  [`Relation`] (`Target`/`Descendant`) + [`should_observe`] is the § 4.3.1
  match predicate (the options vs the mutation type + the target/subtree
  relation + the attribute filter). [`MutationObserver`] carries the
  record queue + the registrations + `observe` (re-observing replaces per
  § 4.3.1, invalid options rejected) / `disconnect` (clears registrations,
  keeps pending records) / `takeRecords` / `drain_for_delivery` (the
  microtask-checkpoint batch). The live-DOM-tree relation classification +
  the microtask checkpoint scheduling + the callback invocation stay in the
  host hook / event-loop layer.
- `Page::evaluate_dom_expression` now projects the initial MutationObserver
  lifecycle smoke seam (`takeRecords().length`, `disconnect()`) through this
  pure queue model while live DOM mutation delivery remains in the host hook.

[`MutationType`]: ../../crates/vixen-engine/src/mutation_observer.rs
[`MutationRecord`]: ../../crates/vixen-engine/src/mutation_observer.rs
[`MutationObserverInit`]: ../../crates/vixen-engine/src/mutation_observer.rs
[`Relation`]: ../../crates/vixen-engine/src/mutation_observer.rs
[`should_observe`]: ../../crates/vixen-engine/src/mutation_observer.rs
[`MutationObserver`]: ../../crates/vixen-engine/src/mutation_observer.rs

**Pure-logic foundation landed for TreeWalker + NodeIterator (Phase 6 prep).**
The DOM § 6 filtered traversal model the two `NodeFilter`-based iterators
reduce to. `#![forbid(unsafe_code)]`, Rust-unit-tested, over a [`Tree`]
trait the host hook implements on the real DOM.
- `vixen-engine::traversal` — DOM § 6. [`NodeType`] (the DOM `nodeType`
  codes) + [`WhatToShow`] (the § 6.1 `whatToShow` bitmask, `SHOW_*`
  constants + `SHOW_ALL`) + [`FilterResult`] (`FILTER_ACCEPT`/
  `FILTER_REJECT`/`FILTER_SKIP`) + the [`NodeFilter`] trait (the JS callback
  the host hook implements) + the [`Tree`] trait (the host hook's tree
  access). [`TreeWalker`] is the § 6.2 rooted stateful walker with the
  seven methods (`parent_node`/`first_child`/`last_child`/`next_sibling`/
  `previous_sibling`/`next_node`/`previous_node`); `FILTER_REJECT` skips
  the rejected node's subtree, `FILTER_SKIP` traverses into it. [`NodeIterator`]
  is the § 6.3 flat preorder iterator (`next_node`/`previous_node`) where
  `REJECT` == `SKIP` (the flat cursor has no subtree state), plus the
  `adjust_for_removal` step the host hook consults when a node is removed
  from the tree (the reference moves to the removed subtree's previous
  sibling's last descendant, else the parent). The real-DOM tree walk +
  the JS `NodeFilter` callback invocation stay in the host hook.
- `Page::evaluate_dom_expression` now projects the whatToShow-only element
  traversal smoke seam for `document.createTreeWalker()` and
  `document.createNodeIterator()` by adapting the parsed document to the
  traversal module's `Tree` trait.

[`NodeType`]: ../../crates/vixen-engine/src/traversal.rs
[`WhatToShow`]: ../../crates/vixen-engine/src/traversal.rs
[`FilterResult`]: ../../crates/vixen-engine/src/traversal.rs
[`NodeFilter`]: ../../crates/vixen-engine/src/traversal.rs
[`Tree`]: ../../crates/vixen-engine/src/traversal.rs
[`TreeWalker`]: ../../crates/vixen-engine/src/traversal.rs
[`NodeIterator`]: ../../crates/vixen-engine/src/traversal.rs

**Pure-logic foundation landed for the WHATWG URL parser (Phase 6 prep).**
The URL Standard § 4 parse + serialize + relative-resolution model the
fetch / navigation / `new URL()` host hooks consult. `#![forbid(unsafe_code)]`,
Rust-unit-tested.
- `vixen-engine::whatwg_url` — WHATWG URL Standard. [`Url`] carries the
  parsed components (scheme / username / password / host / port / path /
  query / fragment); [`is_special_scheme`] + [`default_port`] encode the
  § 3.1 special-scheme family (`http`/`https`/`ws`/`wss`/`file`).
  [`parse`] parses an absolute URL; [`parse_with_base`] is the § 4.6
  relative-resolution parser (absolute-path / relative-segment merge /
  query-only / fragment-only / scheme-relative against a base [`Url`]).
  [`Url::serialize`] is the § 4.1 canonical serialiser (the default port
  omitted, IPv6 re-wrapped in `[...]`, the opaque-path no-slash form for
  non-special schemes); [`Url::origin`] is the § 4.5 `(scheme, host, port)`
  tuple the fetch / storage layers partition on. [`percent_encode`] + the
  [`EncodeSet`]s (C0 control / fragment / query / path / userinfo) cover
  the § 4.2 percent-encoding family. IDNA, full IPv6, and the opaque-path
  long tail are the deferred slices (non-ASCII hosts fail closed; the IPv6
  literal is captured verbatim). The module is named `whatwg_url` (not
  `url`) so it doesn't shadow the extern `url` crate the rest of the
  engine consumes.

[`Url`]: ../../crates/vixen-engine/src/whatwg_url.rs
[`is_special_scheme`]: ../../crates/vixen-engine/src/whatwg_url.rs
[`default_port`]: ../../crates/vixen-engine/src/whatwg_url.rs
[`parse`]: ../../crates/vixen-engine/src/whatwg_url.rs
[`parse_with_base`]: ../../crates/vixen-engine/src/whatwg_url.rs
[`Url::serialize`]: ../../crates/vixen-engine/src/whatwg_url.rs
[`Url::origin`]: ../../crates/vixen-engine/src/whatwg_url.rs
[`percent_encode`]: ../../crates/vixen-engine/src/whatwg_url.rs
[`EncodeSet`]: ../../crates/vixen-engine/src/whatwg_url.rs

**Pure-logic foundation landed for HTML fragment serialisation (Phase 6 prep).**
The DOM → HTML string pipeline the `Element.innerHTML` getter, `outerHTML`,
`document.write`, `DOMParser` round-trip, and `XMLHttpRequest.responseText`
(HTML documents) host hooks read from. `#![forbid(unsafe_code)]`,
Rust-unit-tested, operating over the `markup5ever_rcdom::Handle` the parse
side (`crate::doc`) already owns.
- `vixen-engine::html_serialize` — WHATWG HTML § 13.2.9 "Serializing HTML
  fragments". [`serialize_children`] is the `Element.innerHTML` getter
  (the § 13.2.9 fragment serializer over a node's children); [`serialize_node`]
  is the `Element.outerHTML` getter (one node + descendants). [`escape_text`]
  (§ 13.2.9 step 8: `&` → `&amp;`, `<` → `&lt;`, `>` → `&gt;`, NBSP →
  `&nbsp;`) and [`escape_attribute`] (§ 13.2.9 step 5: `&`, `"`, NBSP) are
  the escape rules exposed standalone for the editing-command surface.
  [`Scripting`] is the scripting-flag toggle (the `noscript` element is
  raw-text when scripting is enabled, the production case; normal-text
  otherwise, the `DOMParser` / print case). The void-element table
  (`area`/`base`/`br`/`col`/`embed`/`hr`/`img`/`input`/`link`/`meta`/`param`/
  `source`/`track`/`wbr`) + the raw-text table (`script`/`style`/`xmp`/
  `iframe`/`noembed`/`noframes`/`plaintext` + conditional `noscript`) are
  the § 13.2.9 step 3 classification. The pre-serialisation tree mutation
  for the `innerHTML` *setter* (the parse side) and the foreign-content
  (SVG/MathML) CDATA escapes stay in the parse layer.

[`serialize_children`]: ../../crates/vixen-engine/src/html_serialize.rs
[`serialize_node`]: ../../crates/vixen-engine/src/html_serialize.rs
[`escape_text`]: ../../crates/vixen-engine/src/html_serialize.rs
[`escape_attribute`]: ../../crates/vixen-engine/src/html_serialize.rs
[`Scripting`]: ../../crates/vixen-engine/src/html_serialize.rs

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
6. `just audit` passes.
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

**Pure-logic foundation landed for the cross-origin isolation gate (Phase 7 prep).**
The COOP + COEP response-header pair that, together, make a browsing context
"cross-origin isolated" — the gate the high-resolution timers
([`vixen_engine::high_res_time::coarsen`]), `SharedArrayBuffer` exposure,
and the other Spectre-hardened APIs consult. Both `#![forbid(unsafe_code)]`,
Rust-unit-tested.
- `vixen-net::coop` — HTML § 7.8 `Cross-Origin-Opener-Policy` parser. The
  three § 7.8.4 policy values ([`coop::Coop`] — `unsafe-none` default /
  `same-origin-allow-popups` / `same-origin`) via the § 7.8.1 structured-header
  item parse (case-insensitive token, unknown ⇒ `UnsafeNone` fail-closed,
  `report-to` parameter captured). [`coop::Coop::isolates_opener`] is the § 7.8.4
  opener-isolation predicate the navigation layer consults before reusing a
  browsing-context group.
- `vixen-net::coep` — Fetch § 3.2 `Cross-Origin-Embedder-Policy` parser. The
  three § 3.2 policy values ([`coep::Coep`] — `unsafe-none` default /
  `require-corp` / `credentialless`) via the structured-header item parse.
  [`coep::is_cross_origin_isolated`] is the HTML § 7.2 combined gate: `true`
  iff the COOP is `same-origin` **and** the COEP is `require-corp` or
  `credentialless`. This is the boolean `MonotonicClock::now`'s
  `cross_origin_isolated` parameter receives, removing the `100µs` coarsening
  floor when the context is fully hardened.

[`coop::Coop`]: ../../crates/vixen-net/src/coop.rs
[`coop::Coop::isolates_opener`]: ../../crates/vixen-net/src/coop.rs
[`coep::Coep`]: ../../crates/vixen-net/src/coep.rs
[`coep::is_cross_origin_isolated`]: ../../crates/vixen-net/src/coep.rs
[`vixen_engine::high_res_time::coarsen`]: ../../crates/vixen-engine/src/high_res_time.rs

**Pure-logic foundation landed for Subresource Integrity + X-Content-Type-Options (Phase 7 prep).**
The two response-header boundaries the fetch layer consults before
executing a subresource — the tampering-resistance surface (SRI) + the
MIME-confusion surface (nosniff). Both `#![forbid(unsafe_code)]`,
Rust-unit-tested.
- `vixen-net::integrity` — W3C SRI § 3.2.2 `<script integrity>` /
  `<link integrity>` metadata parse + § 3.3.4 verify.
  [`HashAlgorithm`] is the three SRI-mandated algorithms (`sha256` /
  `sha384` / `sha512`); SHA-1/MD5 are collision-broken and dropped at parse
  time per spec. [`parse_integrity`] splits ASCII-whitespace-separated
  `<algo>-<base64>` entries (+ the optional `?<options>` tail, parsed but
  not enforced in v1). [`verify`] computes each hash over the raw response
  body via the vetted `sha2` crate + a constant-time compare (a timing
  oracle can't recover the digest); **any** match passes (the spec's "best
  candidate" rule). The [`IntegrityOutcome`] (`NoMetadata` / `Verified` /
  `Mismatch` / `NoKnownAlgorithms`) drives the fetch layer's block.
- `vixen-net::nosniff` — Fetch § 2 `X-Content-Type-Options: nosniff`
  enforcement. [`is_nosniff`] is the case-insensitive token parse (the
  parameterised historical form is rejected); [`is_javascript_mime`] is the
  Fetch § 3.7 16-entry JavaScript-MIME-type predicate; [`Destination`]
  collapses the § 3.1.7 request destination to the two nosniff-relevant
  categories (`Script` / `Style` / `Other`); [`enforce`] blocks a `Script`
  destination whose MIME is not a JavaScript MIME type and a `Style`
  destination whose MIME is not `text/css`, returning the
  [`NosniffOutcome`] (`Allow` / `BlockScript` / `BlockStyle`) the fetch
  layer surfaces as a network error. Other destinations are unaffected
  (the spec intentionally limits `nosniff`'s scope).

[`HashAlgorithm`]: ../../crates/vixen-net/src/integrity.rs
[`parse_integrity`]: ../../crates/vixen-net/src/integrity.rs
[`verify`]: ../../crates/vixen-net/src/integrity.rs
[`IntegrityOutcome`]: ../../crates/vixen-net/src/integrity.rs
[`is_nosniff`]: ../../crates/vixen-net/src/nosniff.rs
[`is_javascript_mime`]: ../../crates/vixen-net/src/nosniff.rs
[`Destination`]: ../../crates/vixen-net/src/nosniff.rs
[`enforce`]: ../../crates/vixen-net/src/nosniff.rs
[`NosniffOutcome`]: ../../crates/vixen-net/src/nosniff.rs

**Pure-logic foundation landed for Cross-Origin-Resource-Policy (Phase 7 prep).**
The Fetch § 4.5.3 CORP header + the combined COEP + CORP gate the fetch
layer consults before applying a no-cors subresource response into a
COEP-hardened document. `#![forbid(unsafe_code)]`, Rust-unit-tested,
reusing [`crate::coep::Coep`] + [`crate::origin::Origin`].
- `vixen-net::corp` — Fetch § 4.5.3. [`Corp`] is the `same-origin` /
  `same-site` / `cross-origin` value ([`parse_corp`] case-insensitive,
  `None` for an absent / unparseable header). [`is_same_site`] is the
  § 4.5.3 same-site predicate (same scheme + matching registrable domain;
  the last-two-labels eTLD+1 heuristic the PSL refines later);
  [`check_corp`] is the § 4.5.3 check (`Allow` / `Block`, opaque origins
  fail closed). [`coep_corp_gate`] is the combined gate: `unsafe-none` ⇒
  allow; CORS ⇒ allow (the alternative opt-in); `require-corp` ⇒
  same-origin allow, cross-origin no-CORP block, cross-origin-with-CORP
  `check_corp`; `credentialless` ⇒ same-origin allow, cross-origin
  `AllowWithoutCredentials`. The CORS check itself + the COEP parse stay
  in [`crate::cors`] / [`crate::coep`].

[`Corp`]: ../../crates/vixen-net/src/corp.rs
[`parse_corp`]: ../../crates/vixen-net/src/corp.rs
[`is_same_site`]: ../../crates/vixen-net/src/corp.rs
[`check_corp`]: ../../crates/vixen-net/src/corp.rs
[`coep_corp_gate`]: ../../crates/vixen-net/src/corp.rs
[`crate::coep::Coep`]: ../../crates/vixen-net/src/coep.rs
[`crate::origin::Origin`]: ../../crates/vixen-net/src/origin.rs
[`crate::cors`]: ../../crates/vixen-net/src/cors.rs
[`crate::coep`]: ../../crates/vixen-net/src/coep.rs

**Pure-logic foundation landed for Trusted Types (Phase 7 prep).**
The W3C Trusted Types `trusted-types` + `require-trusted-types-for` CSP
directive boundary the DOM injection-sink host hooks (`.innerHTML`,
`eval()`, `document.write()`, `script.src = …`, &c.) consult before
accepting a string. `#![forbid(unsafe_code)]`, Rust-unit-tested.
- `vixen-net::trusted_types` — W3C TT. [`TrustedTypeKind`] is the three
  `TrustedHTML`/`TrustedScript`/`TrustedScriptURL` value kinds;
  [`AllowedNames`] is the `trusted-types` directive's policy-name set
  (`None`/`Explicit(list)`/`Wildcard`); [`TrustedTypesPolicyNames`] carries
  the set + the `allow-duplicates` flag; [`RequireFor`] is the
  `require-trusted-types-for 'script'` flag (the only sink-group in v1,
  covering every TT sink). [`parse_trusted_types`] +
  [`parse_require_trusted_types_for`] parse the two directives;
  [`policy_creation_allowed`] is the § 3.2.3 `createPolicy(name)` gate (the
  allowed-name match + the duplicate-name block); [`evaluate_sink`] is the
  § 3.3.5 injection-sink decision (a Trusted\* value ⇒ `Allow`; a string at
  a TT-requiring sink ⇒ `ApplyDefaultPolicy` if a `default` policy exists
  else `Block`; a string at a non-TT sink ⇒ `Allow`). The JS
  `TrustedTypePolicy` factory + the `createHTML`/`createScript`/
  `createScriptURL` sanitisers + the violation-reporting surface stay in
  the host hook.

[`TrustedTypeKind`]: ../../crates/vixen-net/src/trusted_types.rs
[`AllowedNames`]: ../../crates/vixen-net/src/trusted_types.rs
[`TrustedTypesPolicyNames`]: ../../crates/vixen-net/src/trusted_types.rs
[`RequireFor`]: ../../crates/vixen-net/src/trusted_types.rs
[`parse_trusted_types`]: ../../crates/vixen-net/src/trusted_types.rs
[`parse_require_trusted_types_for`]: ../../crates/vixen-net/src/trusted_types.rs
[`policy_creation_allowed`]: ../../crates/vixen-net/src/trusted_types.rs
[`evaluate_sink`]: ../../crates/vixen-net/src/trusted_types.rs

**Gate:** Every security test in `vixen-net` and `vixen-engine` green.
`just audit` passes. Fuzz targets stable.

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
5. WPT target profile from `docs/COMPAT.md` is green. Migrate remaining
   end-to-end CSS+DOM assertions out of Rust tests where an HTML fixture can
   cover the behavior.
6. Update `docs/COMPAT.md` with measured pass counts, known gaps, and the
   next-release WPT expansion plan.
7. Write user-facing release notes.

**Gate:** every release gate in `docs/ACCEPTANCE.md` green. Tag `v1.0.0`.

---

## Total: ~16–22 weeks of focused work.

---

## Binary size strategy

Concrete levers, in priority order:

1. **Re-measure with the ADR-014 `deno_core` runtime.** V8 packaging changed the
   pre-migration size assumptions; current release promises must come from
   measured binaries.
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

| Binary              | Target |
|---------------------|--------|
| `vixen` (GUI)       | TBD after `deno_core`/V8 measurement |
| `vixen-headless`    | TBD after `deno_core`/V8 measurement |

---

## Testing strategy

**WPT-first.** Every CSS/DOM/Layout/Paint feature is tested via a WPT
fixture in `fixtures/`, not a Rust unit test. Rust tests cover only pure
logic (CSS length arithmetic, URL parsing, cookie validation, CSP
parsing, redb storage round-trip).

**WPT-style check types** in `vixen-wpt` (per `docs/SPEC.md`): DOM/style
assertions, `layout-box` coordinate assertions, `display-list-contains` paint
dump assertions, visual hashes, and `ref-equivalent` rendered-page comparisons
against reference HTML fixtures.

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
| `deno_core` / V8 packaging size and cache churn   | Medium     | High   | Keep `JsRuntime` seam stable, measure release binaries with V8, and document cache/pinning in guidance. |
| EGL surfaceless unavailable on some CI runners    | Low        | Medium | `LIBGL_ALWAYS_SOFTWARE=1` + Mesa `llvmpipe` covers every Linux runner.              |
| `gtk4::GLArea` context sharing with WebRender     | Medium     | High   | Validate in Phase 5 first week. Fallback: render to FBO, blit to GLArea with a tex. |
| Vixen-owned layout takes longer than planned      | High       | High   | Keep Phase 4 vertical through `Page`; ship only the WPT-profiled v1 subset and document gaps in `docs/COMPAT.md` (ADR-013). |
| JS host-extension churn                           | Medium     | Medium | Keep the public `JsRuntime`/`JsValue` seam stable while converting bootstrap surfaces to `deno_core` ops/resources; cite `.tmp/ref/deno/core/`, `.tmp/ref/deno/runtime/`, and `.tmp/ref/deno/ext/`. |
| Real-world pages regress vs Servo/Firefox         | Low        | Medium | Upstream issues; report and work around. Document in `docs/COMPAT.md`.              |
| WPT migration backlog grows during build          | Medium     | Medium | Per-phase gate: each phase deletes Rust tests at the rate it adds WPT fixtures.     |
| Relm4 breaking change in `Factory`/`Worker` API   | Low        | Medium | Pin Relm4 version per release; consult `.tmp/ref/relm4/` on upgrades.               |

---

## Per-phase gate summary

| Phase                             | Gate                                                                                             |
|-----------------------------------|--------------------------------------------------------------------------------------------------|
| 0 — Scaffolding                   | `just gate-phase0` passes                                                                         |
| 1 — Net + store crown jewels      | `just gate-phase1` passes                                                                         |
| 2 — JS runtime                    | `just gate-phase2` passes; runtime is `deno_core` per ADR-014                         |
| 3 — HTML + Stylo                  | WPT CSS fixtures pass; cascade output correct                                                    |
| 4 — Layout                        | 20+ visual-hash fixtures match reference                                                         |
| 5 — Paint                         | `just run` shows a page; headless PNG within 1 % of GUI on 5 fixtures                            |
| 6 — Host bindings                 | `fixtures/{dom,events,forms,storage,network}/` all pass                                          |
| 7 — Security                      | `just audit` clean; all security tests green; fuzz stable                                        |
| 8 — Headless CDP                  | Every CLI flag works; CDP responds to required methods                                           |
| 9 — Release                       | All `docs/ACCEPTANCE.md` gates green; tag `v1.0.0`                                               |
