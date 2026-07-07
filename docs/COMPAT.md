# Vixen compatibility target

This is the honest v1.0 target matrix. It is not a claim of full Firefox or full
WPT compatibility. Vixen delegates CSS cascade/selectors, HTML parsing, JS
execution, and paint where credible upstream Rust/Firefox-family components
exist; layout is Vixen-owned Rust code per ADR-013 and therefore WPT-gated by
feature.

---

## Current measured local fixture baseline

As of 2026-07-07, `fixtures/manifest.json` contains 52 local fixtures plus
199 imported smoke fixtures:

| Category | Fixtures |
|----------|---------:|
| css      | 17 |
| css-cascade/css-values | 50 |
| dom      | 10 |
| dom-core | 50 |
| flexbox | 5 |
| forms    | 27 |
| grid | 5 |
| layout   | 9 |
| layout block/inline/position | 6 |
| network  | 2 |
| paint    | 4 |
| paint/ref-equivalent | 8 |
| security | 8 |
| selectors | 50 |
| **Total** | **251** |

Total manifest checks: **1816**.

Current check mix:

| Check type | Count |
|------------|------:|
| `selector-count` | 364 |
| `selectors-exact` | 223 |
| `title` | 250 |
| `js-eval` | 468 |
| `computed-style` | 170 |
| `element-attribute` | 132 |
| `layout-box` | 104 |
| `body-contains` | 66 |
| `no-critical-diagnostics` | 20 |
| `ref-equivalent` | 11 |
| `display-list-contains` | 3 |
| `dom-nodes-range` | 1 |
| `min-nodes` | 1 |
| `selector-match` | 3 |

This local fixture set is release-blocking and must remain **100 % green**.
The layout category currently includes normal-flow, inline-flow, positioned,
flex row/column, grid, overflow coordinate/paint, and fragment-backed text paint
fixtures with `layout-box` and `display-list-contains` assertions. The paint
category includes three local `ref-equivalent` smoke fixtures against the stable
display-list render projection. The harness now reports overall, per-category,
and local/imported pass rates; imported upstream WPT layout/paint coverage is
still tracked separately below. Imported selector smoke has reached the
50-fixture target,
including focused `:has()` child/descendant/adjacent-sibling/general-sibling and
selector-list smoke plus attribute operators/flags, class/id matching,
structural and typed structural pseudos, link/form/read-write/autofill/defined
pseudos, negation/list pseudos, grouping de-duplication, and document-order
coverage. Local CSS computed-style coverage now includes the Milestone 1
advanced cascade seam: `@media`, `@supports`, `@layer`, inherited custom
properties, `var()` fallback, and CSS-wide keyword projection through `Page`.
Imported css-cascade/css-values smoke has reached the
50-fixture target, including specificity/source order, important and inline
precedence, combinator/attribute-operator matching, structural/link/form pseudo
selectors in cascade, `:is()`/`:where()`/`:not()`/`:has()` selectors,
selector-list splitting, custom properties, declaration recovery, comments,
math/color/gradient/transform/shorthand values, and quoted/nested/function
declaration values. Imported DOM-core smoke has reached the 50-fixture target,
including query/getElementById/querySelectorAll, document/root/body access,
tag/class/wildcard collections, attributes, reflected host properties, text
aggregation, parent/child/sibling traversal, null relation checks, document URL,
forms collection length, `matches()`, logical selectors, and `:has()`-backed
matching. Imported forms smoke has reached the 25-fixture target across
reflected/default form/control properties, labels, radio/checkbox/select states,
textarea text, form tree traversal, repeated names, and `:has()` form selectors.
Local Phase 6 fixtures now also assert Page-backed `js-eval` projections for
`getComputedStyle()`, document/navigator state (`documentURI`/`baseURI`,
focus, and active-element shape included), empty Web Storage,
`Event`/`CustomEvent`/`dispatchEvent()` smoke, CSSOM `CSS.supports()` /
`document.styleSheets` plus CSSStyleRule / CSSStyleDeclaration read-only shape,
viewport/window state, DOMRect geometry via `getBoundingClientRect()`,
Geometry Interfaces value constructors (`DOMPoint`/`DOMRect`/`DOMQuad`/
`DOMMatrix`), DOM ancestry/core-node projections (`closest()`, `nodeName`/
`nodeType`, `ownerDocument`), `DOMParser`, `atob`/`btoa`, `classList`/
`relList`/`sandbox`, `dataset`, `ValidityState`/`checkValidity()`, `FormData`
entry-list and iterator projection, meta/content reflection, `innerHTML`/`outerHTML`,
`URL.canParse()`, `data:` URL parsing, `new URL()`/`URLSearchParams` constructor and iterator seams,
`TextEncoder`/`TextDecoder` (`encodeInto` and constructor options included),
`<img>.currentSrc`, initial `Range`/`Selection`, read-only `history` accessors,
`structuredClone`,
MutationObserver lifecycle, TreeWalker/NodeIterator traversal, `Headers`
iteration, `Blob`/`File`, read-only `Request`/`Response` state with forbidden
header filtering, `Response.error()` / `Response.redirect()` / `Response.json()`,
`AbortSignal`, `URLPattern`, Performance timing shape, and
`matchMedia()` before the remaining SpiderMonkey host-object swap; Encoding API
constructors plus the first focused `document`/`Element` snapshot host-object
evals are also exercised directly through the SpiderMonkey runtime. Imported
smoke fixtures now also seed
block/inline/position layout, flexbox, grid, and display-list `ref-equivalent`
paint; imported layout smoke covers auto margins, border-box sizing, inline
flow, flex reverse/gaps, and grid `minmax()`/fractional row/gap cases. Imported
paint smoke now covers currentcolor, overflow clipping, positioned boxes,
flex/grid backgrounds, and nested background/text display-list equivalence.

---

## WPT target profile

Full upstream WPT is too broad to summarize honestly with one percentage at
v1.0. The release contract is a curated, imported WPT profile with measured
pass counts by category. Small, Vixen-minimized upstream-derived smoke fixtures
may live beside local fixtures and remain recorded in `fixtures/manifest.json`.
Larger upstream slices should use committed WPT profile JSON plus an ignored,
pinned upstream checkout (for example `.tmp/wpt/`) so review diffs contain only
the selected paths/checks/provenance, not vendored WPT source files. Both paths
feed the same `vixen-wpt` check types and reporting.

| Area | v1.0 target | Expected achievability | Notes |
|------|-------------|------------------------|-------|
| HTML parsing/tree construction | Broad smoke subset green | High | `html5ever` carries parser behavior; Vixen must preserve node ids/tree shape. |
| Selectors | Modern selector subset green | High | Backed by Stylo/`selectors`; include combinators, attributes, `:is`, `:where`, `:has`, form/link pseudos. |
| CSS cascade/computed values | Author stylesheet + inline subset green | High after full Stylo slice | Compact cascade is temporary; full Stylo should unlock wider WPT coverage. |
| CSS layout: block/inline | v1 visual/ref subset green | Medium | Vixen-owned layout; start with normal flow, margin/border/padding, inline line boxes. |
| CSS layout: flex/grid | Useful common-case subset green | Medium | Pure helpers exist; full WPT edge coverage is post-v1. |
| CSS layout: tables/floats/fragmentation | Not v1 release-blocking | Low for v1 | Document as unsupported/partial until implemented. |
| DOM Core | Traversal, attributes, token lists, ranges, mutation observer subset green | Medium | Vixen-owned Web APIs over SpiderMonkey bindings. |
| Events/forms/history/storage | Selected behavioral subset green | Medium | Gate by fixtures from SPEC invariants and imported WPT cases. |
| JS language | Use SpiderMonkey/test262, not WPT percentage | High for language | Web API exposure remains Vixen-owned and separately gated. |
| Paint/ref tests | Display-list + WebRender visual subset green | Medium | One paint path; correctness depends on layout fragments and WebRender mapping. |
| Media/WebGPU/WebRTC/service workers | Out of scope for v1 | Not targeted | Deferred by ADRs / acceptance post-v1 scope. |

---

## Release-blocking WPT goals

For v1.0, Vixen should be able to claim:

1. **100 % pass** on local `fixtures/manifest.json`.
2. **Green imported WPT smoke profile** for parser, selectors, cascade, DOM core,
   forms, and the v1 layout subset.
3. **Measured pass counts published here** for every imported category.
4. **No global full-WPT percentage claim** until the harness imports and runs a
   representative upstream WPT checkout.

Initial import targets before v1.0:

| Imported WPT area | Minimum useful target |
|-------------------|----------------------:|
| selectors/css-scoping/css-nesting selector behavior | 50 fixtures |
| css-cascade / css-values computed-value behavior | 50 fixtures |
| dom/nodes + traversal + ranges | 50 fixtures |
| html/semantics/forms basics | 25 fixtures |
| css/css-display + css-box + css-position normal-flow layout | 40 fixtures |
| css-flexbox common cases | 25 fixtures |
| css-grid common cases | 25 fixtures |
| paint/ref-equivalent smoke | 20 fixtures |

These are minimum profile sizes, not final compatibility claims. The measured
pass table below must be filled from `vixen-wpt` output as the fixtures land.

| Imported WPT area | Fixtures run | Checks run | Passed | Pass rate | Notes |
|-------------------|-------------:|-----------:|-------:|----------:|-------|
| selectors | 50 | 232 | 232 | 100.0% | Target reached: `:has()` child/descendant/adjacent-sibling/general-sibling and selector-list smoke, attribute operators/flags, class/id matching, structural and typed structural pseudos, link/form/read-write/autofill/defined pseudos, negation/list pseudos, grouping de-duplication, and document-order coverage. |
| css-cascade/css-values | 50 | 250 | 250 | 100.0% | Target reached: specificity/source order, importance/inline, combinator/attribute operator matching, structural/link/form pseudo cascade, functional pseudo specificity, selector-list splitting, custom properties, declaration recovery, comments, math/color/gradient/transform/shorthand values, and quoted/nested/function declaration values. |
| dom-core | 50 | 250 | 250 | 100.0% | Target reached: query/getElementById/querySelectorAll, document/root/body access, tag/class/wildcard collections, attributes, reflected host properties, text aggregation, parent/child/sibling traversal, null relation checks, document URL, forms collection length, `matches()`, logical selectors, and `:has()`-backed matching. |
| forms | 25 | 134 | 134 | 100.0% | Required/optional/disabled/checked controls, labels/buttons/form attributes, reflected/default input/form/select/option properties, textarea text, tree traversal, repeated names, and `:has()` form selectors. |
| layout block/inline/position | 6 | 30 | 30 | 100.0% | Block flow, margin/padding/border, auto margins, border-box sizing, inline flow, and relative/absolute positioned smoke. |
| flexbox | 5 | 25 | 25 | 100.0% | Row/column grow-basis, gap/padding, and reverse-axis smoke. |
| grid | 5 | 26 | 26 | 100.0% | Fixed, fractional, `minmax()`, row/column gap, and fixed-height fractional-row smoke. |
| paint/ref-equivalent | 8 | 24 | 24 | 100.0% | Display-list reference-equivalent background/text, currentcolor, overflow clipping, positioned, flex/grid, and nested-background smoke. |

---

## Known v1.0 layout gaps

Expected unsupported or partial areas unless promoted by WPT/real-site evidence:

- table layout
- floats and float avoidance
- full vertical writing modes / vertical text shaping
- page fragmentation / pagination / print layout
- advanced intrinsic sizing cycles (`min-content` / `max-content` edge cases)
- complete absolute/fixed/sticky interaction matrix
- full SVG layout integration

Each gap should fail closed where possible, emit diagnostics when visible to
users/tests, and receive a WPT fixture before being marked supported.
