# Vixen executable milestones

This is the short delivery plan for turning the current pure-prep modules into
browser-visible slices. Rule: every large browser milestone extends
`vixen_engine::page::Page` and proves itself with a `just gate-*` command plus a
fixture in `fixtures/manifest.json`. Alpha/dev batch sizing and maintainability
rules live in [`DEVELOPMENT.md`](DEVELOPMENT.md).

Current product priority lives in [`PROJECT_DIRECTION.md`](PROJECT_DIRECTION.md)
and the focused MVP-to-alpha order lives in [`ROADMAP.md`](ROADMAP.md). This file
tracks executable gates and historical milestone slices.

## Gates

| Command | Proves today | Extends next |
|---------|--------------|--------------|
| `just gate-alpha` | fmt, clippy, workspace check, WebIDL/runtime host seam checks, and committed fixture manifest runner | fast alpha-slice baseline before relevant phase gate |
| `just gate-smoke` | fmt, clippy, all host tests | reviewer baseline before commit/push |
| `just gate-push` | alpha + phase-6 runtime + smoke + diff whitespace checks | hk pre-push enforcement point |
| `just gate-webidl` | generated WebIDL constructor/prototype coverage plus headless/CDP runtime host seams | expand manifest/import coverage while keeping host-family implementations on generated prototypes |
| `just gate-phase2` | `vixen-headless --eval '1+2'` through the `deno_core` JS runtime seam | grow host modules behind the same `JsRuntime`/`JsValue` seam |
| `just gate-phase3` | HTML parse + Stylo selector matching + author/inline computed-style cascade through `Page` + WPT fixtures | full Stylo `Stylist`/computed values behind `Page::computed_style` |
| `just gate-phase4` | layout pure-logic prep + Page-backed layout tree / text line boxes plus `layout-box` fixture assertions | richer inline/flex/grid formatting contexts |
| `just gate-phase5` | display-list + paint prep + Page-backed layout-tree display list/stats through `vixen-headless --dump-display-list` + `--paint-stats` | WebRender screenshot path through `Page` |
| `just gate-phase6` | full engine host-family tests plus `gate-webidl` coverage for generated WebIDL prototypes and headless/CDP runtime seams | convert remaining Page string projections to explicit `deno_core` op/resource extensions, then widen CSSOM/geometry/forms/events/history/storage/fetch |

## Six-milestone execution roadmap

These labels are **ordering**, not calendar promises. Use `Milestone N` in issue
titles, commits, and release notes so work does not imply a specific calendar
slot or date.

1. **Milestone 1 — Cascade seam.** Keep `Page::computed_style(node_id)` as the
   single public seam, broaden the author/inline cascade enough for layout and
   WPT fixture growth (`@media`, `@supports`, `@layer`, custom properties via
   `var()`, inherited custom properties, and CSS-wide keyword handling), then
   keep the full Stylo `Stylist` replacement as an implementation swap behind
   the same facade. Proof: `just gate-phase3 && just gate-smoke`.
2. **Milestone 2 — Layout fragments.** Replace text-width estimates with
   positioned fragments for normal-flow block/inline, common flex/grid,
   positioned descendants, and overflow clipping. The first fragment seam is now
   live as `Page::layout_fragments(viewport)`: block backgrounds and wrapped text
   lines project from the layout tree into paint-consumable fragments while real
   shaping remains the next formatter swap. Proof:
   `just gate-phase4 && just gate-smoke` plus the imported layout WPT profile.
3. **Milestone 3 — WebRender screenshots.** Consume `Page::display_list` through
   one WebRender path over `vixen_api::GlContext`; make headless
   `--screenshot` write PNGs and keep GUI/headless on the same path. Proof:
   `just gate-phase5`, screenshot/visual-hash fixtures, and `just gate-smoke`.
4. **Milestone 4 — JS host bindings.** The runtime is now `deno_core`; replace
   string-smoke DOM evals with explicit `deno_core` extensions for
   document/query/element attributes,
   DOMTokenList/dataset, events/forms/history, fetch/cookie, and storage. The
   first compatibility seam now reflects `getComputedStyle()`, document/navigator
   state, empty Web Storage, viewport/window state,
   `Event`/`CustomEvent`/`dispatchEvent()` smoke, CSSOM `CSS.supports()` /
   `document.styleSheets` / CSSStyleRule shape, DOMRect geometry via
   `getBoundingClientRect()`, DOM ancestry/core-node state (`closest()`,
   `nodeName`/`nodeType`, `ownerDocument`), `DOMParser`, `atob`/`btoa`,
   Geometry Interfaces value constructors, `classList`/`relList`/`sandbox`,
   `dataset`, `ValidityState`/`checkValidity()`, `FormData` iteration,
   meta/content reflection, HTML serialisation getters, URL/URLSearchParams iteration,
   TextEncoder/TextDecoder (`encodeInto` and constructor options included),
   `<img>.currentSrc`, initial `Range`/`Selection`,
   read-only history accessors, structured clone, MutationObserver lifecycle,
   TreeWalker/NodeIterator, `Headers` iteration, `Blob`/`File`, read-only
   `Request`/`Response` state, static `Response.error()` / `Response.redirect()` / `Response.json()`,
   `AbortSignal`, `URLPattern`, Performance timing shape, and
   `matchMedia()` through `Page::evaluate_dom_expression`
   against WPT manifest `js-eval` checks, while TextEncoder/TextDecoder now run
   through the first op-backed `deno_core` host extension. Focused
   `document.title`, simple `querySelector`/`getElementById`,
   `querySelectorAll().length`, and read-only DOMTokenList/dataset evals run on
   the `deno_core` DOM snapshot extension on generated WebIDL prototype chains,
   with page data crossing an explicit op boundary. Selector lookup and
   `Element.matches()` now cross finer-grained
   ops, element record data is loaded through an element snapshot op, and
   text/attribute/token/dataset reads now delegate through focused DOM ops.
   Element `getBoundingClientRect()` / `getClientRects()` reads now cross a
   focused DOM rect op.
   Focused `CSS.supports`, `getComputedStyle`, and CSSStyleSheet/CSSRule evals
   now run through an explicit CSSOM extension/op boundary too.
   Proof: `just gate-phase6`, relevant WPT fixtures, and `just gate-smoke`.
5. **Milestone 5 — Browser shell vertical.** Wire URL entry, one-tab navigation,
   reload/stop/back/forward, visible page content, and tab diagnostics through
   the engine trait. Proof: `just shell-check`, manual GUI smoke, and
   `just gate-smoke`.
6. **Milestone 6 — Release hardening.** Publish measured WPT profiles in
   `docs/COMPAT.md`, reduce dependency/LOC budget pressure, keep modules under
   1 kLOC, add benches for landed vertical paths, and run audit/size gates.
   Proof: `just audit`, `just size-fp`, and all release gates.

## Forward-looking executable slices

These are the next reviewable pushes, in preferred order. Each should keep the
same rule as above: one Page/headless-visible seam, one fixture path, one gate.

1. **Cascade replacement slice** — author `<style>` blocks and inline `style`
   attributes now flow through `Page::computed_style(node_id)` with Stylo
   selector matching, specificity, source order, cascade layers, media/supports
   conditions, custom-property `var()` resolution, inherited custom properties,
   CSS-wide keywords, `!important`, and read-only CSSStyleRule/CSSStyleDeclaration
   smoke. Next: replace the compact projection with Stylo `Stylist` computed
   values behind the same facade when the `TNode` / `TElement` / `TDocument`
   implementation is ready. Proof:
   `just gate-phase3 && just gate-smoke`.
2. **Layout realism slice** — `Page::dump_layout_tree` now emits the first
   arena-backed Vixen layout tree, basic block box-model styles
   (`width`/`height`/`margin`/`border`/`padding`/`box-sizing`) influence node
   boxes, inline/text children in blocks flow horizontally for the first inline
   formatting-context slice, basic relative/absolute positioned descendants get
   coordinate coverage, fixed/grow flex-row items use the shared flex resolver,
   fixed/grow flex-row and flex-column items use the shared flex resolver,
   fixed/fr grid tracks use the shared grid resolver, overflow containers clip
   descendant display-list paint, `layout-box` manifest checks assert element
   coordinates, `Page::dump_lines` derives line boxes from that tree, and
   `Page::layout_fragments` now gives paint a fragment seam for block backgrounds
   plus wrapped text lines. Next: replace average-width text metrics with shaped
   glyph fragments, then extend grid placement / intrinsic sizing only behind new
   imported WPT fixtures. Proof:
   `just gate-phase4 && just gate-smoke`.
3. **Renderer screenshot slice** — `Page::display_list` now converts the first
   line layout fragments into invariant-enforced paint commands and exposes
   `vixen-headless --dump-display-list`; `--paint-stats` reports command counts
   and painted area from the same stream. Next: consume that display list via
   WebRender behind `Page::render(&dyn GlContext)` and make headless
   `--screenshot` produce PNGs. Proof:
   `just gate-phase5 && just gate-smoke`.
4. **Host-object replacement slice** — `JsRuntime` is now backed by `deno_core`
   while keeping `JsRuntime`/`JsValue`, headless `--eval`, CDP `Runtime.evaluate`,
   Encoding API constructors, and the current focused document/DOMTokenList/dataset
   evals green. Continue host-object replacement. `Page::evaluate_dom_expression` now
   projects the `getComputedStyle()`, document/navigator state, empty Web
   Storage, viewport/window state, Event/CustomEvent/dispatch smoke, CSSOM
   `CSS.supports()` / `document.styleSheets` / CSSStyleRule shape, DOMRect
   geometry via `getBoundingClientRect()`, DOM ancestry/core-node state
   (`closest()`, `nodeName`/`nodeType`, `ownerDocument`), `DOMParser`,
   `atob`/`btoa`, Geometry Interfaces value constructors, read-only DOMTokenList
    (`classList`/`relList`/`sandbox`), `dataset`, form validity, `FormData` iteration,
    meta/content reflection, HTML serialisation, URL/URLSearchParams iteration
   (`URL.canParse()` included), Encoding API (`encodeInto` and constructor
   options included), responsive-image `currentSrc`, initial Range/Selection,
   read-only history,
    structured clone containers, MutationObserver, traversal, Headers iteration,
    Blob/File, read-only Request/Response state, Response static constructors,
    AbortSignal,
   URLPattern, Performance, and matchMedia smoke
   surfaces from the same pure modules that Phase 6 host objects will use. The
   Encoding API constructors now run through an op-backed `deno_core` extension;
   `script::webidl` now renders generated browser interface/prototype scaffolding
   for the runtime-visible DOM/CSSOM/geometry subset, and host-family bootstraps
   adopt those generated prototypes instead of hand-rolling constructor shape.
   focused document/query/element evals and read-only DOMTokenList/dataset
   property reads run against a DOM snapshot extension whose Page data is served
   by `op_vixen_dom_snapshot`, with selector lookup and `Element.matches()` now
   delegated through explicit DOM ops and element data loaded through
   `op_vixen_dom_element_snapshot`; text/attribute/token/dataset reads also
   cross focused DOM ops. Element `getBoundingClientRect()` / `getClientRects()`
   reads now cross a focused DOM rect op. Focused `CSS.supports`,
   `getComputedStyle`, and CSSStyleSheet/CSSRule evals now run against
   `script::cssom` ops. Next: widen the replacement across Geometry Interface
   value constructors, forms, events, history, storage, and fetch.
   Proof: `just gate-phase6`, relevant WPT fixtures, and `just gate-smoke`.
5. **Browser shell vertical slice** — the first one-window GTK shell now wires a
    URL entry, reload/stop/back/forward, one-page lifecycle, status diagnostics,
    and a visible `gtk4::GLArea` WebRender surface through a shell-side
    `vixen-api::Engine` adapter. Next: replace the synchronous adapter with the
    planned per-tab Relm4 worker/factory architecture and add full tab lifecycle
    affordances. Proof: `just flatpak-build`, one manual GUI smoke, and
    `just gate-smoke`.
6. **Release-measurement slice** — keep `docs/COMPAT.md` generated from the
   manifest runner output, add benches for the landed Page seams, then make
   `just size-fp` and `just audit` routine release gates instead of last-minute
   checks. Proof: all release gates green from a clean checkout.

Keep adapters thin: `vixen-api` owns DTOs/traits, `vixen-engine::page` owns the
pipeline state, `vixen-headless` and `vixen-wpt` only drive the facade.
