# Vixen executable milestones

This is the short delivery plan for turning the current pure-prep modules into
browser-visible slices. Rule: every large browser milestone extends
`vixen_engine::page::Page` and proves itself with a `just gate-*` command plus a
fixture in `fixtures/manifest.json`. Alpha/dev batch sizing and maintainability
rules live in [`DEVELOPMENT.md`](DEVELOPMENT.md).

## Gates

| Command | Proves today | Extends next |
|---------|--------------|--------------|
| `just gate-alpha` | fmt, clippy, workspace check, and committed fixture manifest runner | fast alpha-slice baseline before relevant phase gate |
| `just gate-smoke` | fmt, clippy, all host tests | reviewer baseline before commit/push |
| `just gate-phase2` | `vixen-headless --eval '1+2'` through SpiderMonkey | DOM/document host bindings |
| `just gate-phase3` | HTML parse + Stylo selector matching + author/inline computed-style cascade through `Page` + WPT fixtures | full Stylo `Stylist`/computed values behind `Page::computed_style` |
| `just gate-phase4` | layout pure-logic prep + Page-backed layout tree / text line boxes plus `layout-box` fixture assertions | richer inline/flex/grid formatting contexts |
| `just gate-phase5` | display-list + paint prep + Page-backed layout-tree display list/stats through `vixen-headless --dump-display-list` + `--paint-stats` | WebRender screenshot path through `Page` |
| `just gate-phase6` | DOM/forms/network-host pure prep plus Page-backed getComputedStyle/CSSOM/CSSStyleRule/viewport/DOMRect/Geometry Interfaces/document/navigator/storage/events/DOMTokenList/dataset/validity/FormData/URL/encoding/serialization/currentSrc/range/history/traversal/mutation/fetch `Blob`/`File`/`Request`/`Response`/performance/media eval projections, with Encoding API constructors and the first focused `document`/`Element` evals now in SpiderMonkey | remaining SpiderMonkey host objects, event dispatch, storage persistence |

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
4. **Milestone 4 — Real DOM host bindings.** Replace string-smoke DOM evals
   with SpiderMonkey host objects for document/query/element attributes,
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
   against WPT manifest `js-eval` checks, while focused `document.title`, simple
   `querySelector`/`getElementById`, and `querySelectorAll().length` evals have
   moved onto the first SpiderMonkey `document` / `Element` snapshot objects.
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
4. **Host-object replacement slice** — `Page::evaluate_dom_expression` now
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
   Encoding API constructors and focused document/query/element evals now run in
   SpiderMonkey against those pure-module/Page snapshots. Next: widen the
   replacement across DOMTokenList/dataset/CSSOM/geometry/forms/events/history/
   storage/fetch.
   Proof: `just gate-phase6`, relevant WPT fixtures, and `just gate-smoke`.
5. **Browser shell vertical slice** — once screenshots and host objects have a
   shared Page seam, wire URL entry, reload/stop/back/forward, one-tab lifecycle,
   and visible diagnostics through `vixen-api::Engine`. Proof: `just shell-check`,
   one manual GUI smoke, and `just gate-smoke`.
6. **Release-measurement slice** — keep `docs/COMPAT.md` generated from the
   manifest runner output, add benches for the landed Page seams, then make
   `just size-fp` and `just audit` routine release gates instead of last-minute
   checks. Proof: all release gates green from a clean checkout.

Keep adapters thin: `vixen-api` owns DTOs/traits, `vixen-engine::page` owns the
pipeline state, `vixen-headless` and `vixen-wpt` only drive the facade.
