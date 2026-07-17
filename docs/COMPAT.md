# Vixen compatibility target

This is the honest v1.0 target matrix. It is not a claim of full Firefox or full
WPT compatibility. Vixen delegates focused parser, cascade, runtime, and native
rendering primitives where that improves correctness and size. All measured
rendered compatibility now uses Flutter-owned formatting, geometry, semantics,
and scenes. R7 deleted the native/Rust renderer path. Every supported CSS
semantic remains fixture/WPT-gated.

---

## Current measured committed fixture baseline

As of 2026-07-16, `fixtures/manifest.json` contains 70 local fixtures plus
200 imported smoke fixtures:

| Category | Fixtures |
|----------|---------:|
| css      | 17 |
| css-cascade/css-values | 50 |
| cssom-view | 1 |
| dom      | 25 |
| dom-core | 50 |
| events | 1 |
| flexbox | 5 |
| forms    | 28 |
| grid | 5 |
| layout   | 9 |
| layout block/inline/position | 6 |
| network  | 2 |
| paint    | 4 |
| paint/ref-equivalent | 8 |
| security | 9 |
| selectors | 50 |
| **Total** | **270** |

Total manifest checks: **2027**.

Current check mix:

| Check type | Count |
|------------|------:|
| `selector-count` | 398 |
| `selectors-exact` | 223 |
| `title` | 269 |
| `js-eval` | 597 |
| `computed-style` | 173 |
| `element-attribute` | 132 |
| `layout-box` | 104 |
| `body-contains` | 68 |
| `visual-hash` | 25 |
| `no-critical-diagnostics` | 22 |
| `ref-equivalent` | 11 |
| `dom-nodes-range` | 1 |
| `min-nodes` | 1 |
| `selector-match` | 3 |

This local fixture set is release-blocking and must remain **100 % green**.
R8 native-path reproduction on clean revision `e224bf6` ran `just
compat-report`: all 270 fixtures and all 1,868 native-safe BrowserCore checks
passed. R8 release-host reproduction then ran `just flutter-fixture-manifest`'s
exact command against the clean post-R7/Yaru release bundle: all 270 fixtures and
all 2,027 checks passed. The host summary reports 140 direct rendered checks
(104 `layout-box`, 25 `visual-hash`, and 11 `ref-equivalent`); the remaining 19
`flutter-js-eval` checks also run only in that Flutter host. None is relabeled as
native evidence.
The layout category currently includes normal-flow, inline-flow, positioned,
flex row/column, grid, overflow coordinate/paint, and fragment-backed text paint
fixtures with Flutter-commit `layout-box` and visual assertions. The paint
category includes 11 `ref-equivalent` checks against exact Flutter scene pixels.
The harness now reports overall, per-category,
and local/imported source×category pass rates. Its adapter now creates production
BrowserCore contexts, so fixture snapshots/selectors/styles/evaluation/reference
rendering/pixel capture share typed document/runtime generations and persistent
per-context V8 realms rather than constructing harness-owned Pages or runtimes.
Imported upstream WPT
layout/paint coverage is still tracked separately below. Imported selector smoke
has reached the
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
Local Phase 6 fixtures now also assert runtime/Page-backed `js-eval` projections for
`getComputedStyle()`, document/navigator state (`documentURI`/`baseURI`,
focus, and active-element shape included), op-backed in-memory Web Storage
mutation with key/value validation and quota errors,
`Event`/`CustomEvent`/`dispatchEvent()` smoke, the pinned
`focusout` → `focusin` → `blur` → `focus` transition with `relatedTarget`,
Page-owned active-element restore, CSSOM `CSS.supports()` /
`document.styleSheets` plus CSSStyleRule / CSSStyleDeclaration read-only shape,
viewport/window state, DOMRect geometry via `getBoundingClientRect()` /
`getClientRects()`, client/offset/scroll metrics, `getBoxQuads()`, Range
rectangles, Geometry
Interfaces value constructors (`DOMPoint`/`DOMRect`/`DOMQuad`/`DOMMatrix`), DOM
ancestry/core-node projections (`closest()`, `nodeName`/`nodeType`,
`ownerDocument`), anchor URL decomposition/reflection, `DOMParser`, `atob`/`btoa`, `classList`/
`relList`/`sandbox`, `dataset`, `ValidityState`/`checkValidity()`, `FormData`
entry-list and iterator projection plus runtime/CDP form submission by page node
id with successful submitter overrides, runtime form reset/default-state restore,
meta/content reflection,
`innerHTML`/`outerHTML`,
`URL.canParse()`, `data:` URL parsing, `new URL()`/`URLSearchParams` constructor and iterator seams,
`TextEncoder`/`TextDecoder` (`encodeInto` and constructor options included),
`<img>.currentSrc` plus image alt/dimension/loading/decode reflection, inert
media element state (`HTMLMediaElement`/audio/video constants included),
resource element reflection (`link`/`style`/`script`/`source`), single-range
`Range`/`Selection` state with Page-owned element-boundary restore, direction,
point queries, same-container clone/extract/delete/insert/surround operations,
and `selectionchange` delivery, read-only `history` accessors,
details/dialog open-state reflection,
miscellaneous HTML reflected attributes for lists, quotes, embedded content, and
table cells,
progress/meter numeric state,
inert Canvas 2D context smoke,
form-associated reflected attributes and editing helpers,
read-only table collections/indexes,
HTMLElement interaction/global reflected attributes,
text track / track-element state,
inert OffscreenCanvas/ImageData/ImageBitmap/Path2D APIs,
minimal ShadowRoot/DocumentFragment smoke,
template content and slot assignment shape,
DOM construction/serialization helpers,
`structuredClone`, CDP `Runtime.awaitPromise` over stored promise handles,
MutationObserver lifecycle, TreeWalker/NodeIterator traversal, `Headers`
iteration, `Blob`/`File`, read-only `Request`/`Response` state with forbidden
header filtering, `Response.error()` / `Response.redirect()` / `Response.json()`,
op-backed `fetch()` HTTP(S) status/header/body reads plus URL-policy/private-host
rejection with CDP `Network.loadingFailed` diagnostics, credential-correct CORS,
bounded origin/target/credentials-partitioned preflight caching (including
effective CDP extra headers), strongest-algorithm Request SRI verification before
exposure/cache insertion, `AbortSignal`,
`URLPattern`, CDP lifecycle opt-in (`init`/`commit`/`DOMContentLoaded`/`load`),
Performance timing shape, `matchMedia()`, Permissions API query state,
Notification permission state, and StorageManager estimate/persisted state backed
by profile/storage records before the remaining host-object swap; Encoding API constructors,
Web Storage mutation, focused `fetch()` success/blocking checks, sequential
global/storage persistence across `Runtime.evaluate`, focused `document`/`Element`
snapshot host-object evals and read-only `DOMTokenList` property reads are also
exercised directly through the persistent `deno_core` runtime seam.
`HTMLElement.dataset` has since moved off that frozen projection: its stable live
`DOMStringMap` reflects external attributes and routes assignment/deletion through
the authoritative DOM mutation, Stylo, and renderer-source path.
`Element.classList` now also retains a stable live `DOMTokenList` identity across
external and list-driven `class` writes, with current token reflection and the
same authoritative mutation/cascade path. `HTMLAnchorElement.relList` retains
the same identity and behavior across `rel` writes, as does
`HTMLIFrameElement.sandbox` across valid sandbox-token writes. These are all
attribute-backed token-list families currently hosted by the runtime.
`HTMLElement.style` now likewise retains one live inline `CSSStyleDeclaration`
across external attribute replacement and declaration API writes, using the same
authoritative mutation/cascade path.
`Element.attributes` now retains a live `NamedNodeMap`, and attached `Attr`
objects retain identity, reflect external writes, and write through `Attr.value`.
Detached Attr lifecycle and `setNamedItem`/`removeNamedItem` remain incomplete.

Static parser-discovered PNG `<img src>` has one resource-to-pixel vertical.
BrowserCore applies exact generations, URL/CSP/mixed-content/redirect policy,
cookies/cache, response MIME/status, and compressed/dimension/decoded limits
before exposing accepted bytes to Flutter. A 2×2 four-colour fixture proves exact
Flutter scene pixels. This does not yet claim dynamic image loading, complete
`srcset`/`picture`, animated PNG, JPEG/WebP/GIF, SVG image documents, broad
intrinsic replaced-element sizing, or image events.
Runtime platform smoke now additionally covers secure `crypto.getRandomValues()` /
`randomUUID()`, async Clipboard text and `ClipboardItem` shape, `MessageEvent`,
`MessageChannel`, `BroadcastChannel`, first-callback `IntersectionObserver` /
`ResizeObserver` geometry, and a fail-closed `WebSocket` close path. Imported
smoke fixtures now also seed
block/inline/position layout, flexbox, grid, and Flutter-scene
`ref-equivalent` paint; imported layout smoke covers auto margins, border-box sizing, inline
flow, flex reverse/gaps, and grid `minmax()`/fractional row/gap cases. Imported
paint smoke now covers currentcolor, overflow clipping, positioned boxes,
flex/grid backgrounds, and nested background/text scene equivalence.

---

## Current automation smoke baseline

The external Playwright smoke covers connect/target/page/runtime/DOM/input/
network/dialog/screenshot/history/content/script/style/binding paths plus
browser-context permission grant/reset, bounded Chromium JSON tracing through
CDP `IO` streams, idle stop-loading behavior, and stable protocol errors. CDP
permission overrides are exact-origin or wildcard scoped and do not mutate
persisted user decisions. Trace records contain method/timing/session/success
metadata only, not expressions, request headers, form values, or page text.
R8 reran the release/AOT Flutter-hosted smoke after cutover: two target
viewports remained isolated, Flutter geometry/input and before/after scene pixels
agreed, target switching preserved the first scene, and forced renderer reset
recovered by full resync to byte-identical pixels. The first A1 extension now
also writes a live `dataset` property, observes the attribute-selector-driven
140×32 box synchronously, matches later CDP DOM attributes/geometry, and pins
renderer-specific before/after Flutter scene hashes. The second through sixth A1
extensions retain `classList` through the click mutation, a real anchor's
`relList` through a visible rel-selector mutation, and a real iframe's `sandbox`
through valid token writes, then retain inline `style` through external and API
writes and attached attributes through `Attr.value`. Page and CDP views match
the resulting geometry; every exact scene recovers byte-identically after
renderer reset.

CDP targets now map to independent BrowserCore contexts/runtimes and share only
profile-scoped state. BrowserCore source navigation is asynchronous,
generation-checked, and directly cancellable; deterministic stop/supersede,
redirect/stop, reload, history-traversal, and parser-stage race tests force stale
work and prove no stale document/history/cookie commit or terminal success event.
The CDP WebSocket path uses one event pump while navigation-producing requests are
pending. `Page.navigate`, `Page.reload`, `Target.createTarget`, cross-document
history traversal, and runtime/input-triggered navigation therefore leave the
same connection available for `Page.stopLoading` or unrelated commands. Exact
ordered BrowserCore navigation ids correlate multi-action evaluations, and claimed
abandonment records prevent late outcomes from affecting later requests. Gated
socket tests cover navigate/reload, history, multi-action runtime navigation, and
non-blocking target creation. Configured initial-URL loading still settles before
socket acceptance by design. Configured and parser-discovered scripts yield
between items; a committed author exception emits
`Runtime.exceptionThrown`, later independent scripts continue, and normal load
settlement follows. Individual V8 jobs are deadline-bounded, failed/timed-out
evaluations discard deferred DOM mutations before isolate reuse, and parser-discovered
external classic-script reads are generation-cancellable. Navigate/reload/stop/
close commands snapshot and interrupt the exact active runtime generation before
the deadline; interrupted mutations/effects are discarded, the cancellation is
not reported as a page exception, and the isolate remains reusable. Runtime
`fetch()` and CORS preflight waits also return promptly on that signal; the
worker-local cancellation path drops the in-flight reqwest future, joins the
worker, and cannot commit cookie/cache state. Gated peers observe the fetch and
preflight connections close before sending a response. Runtime construction and
other local native host calls remain open. Parser-discovered non-alternate
`<link rel="stylesheet">` now uses the same cancellable bounded text-resource
worker before author scripts. Relative file and HTTP(S) sheets apply in document
order to Page cascade/renderer source and refreshed runtime computed-style hosts. Redirect hops recheck `style-src`, mixed-content, and URL policy; accepted
HTTP responses pass status/`nosniff` checks before cookie, bounded profile-cache,
or style commit. A checked-in file fixture proves visible red 120×40 output, and
gated HTTP/supersede tests prove request-id events and rejection of late cookie,
cache, and style commits. Link media is currently limited to absent/`all`/
`screen`; alternate sheets, dynamic links, `@import`, SRI, cache reuse/freshness,
and complete external-sheet CSSOM objects remain unsupported. There is still no
HTTP download manager or Playwright context-tracing archive implementation.

---

## Current Flutter shell smoke baseline

The Linux shell uses one BrowserCore and one Flutter renderer under native
Wayland. Normal GUI, page-only automation, rendered CDP, Playwright, and the
fixture manifest share the same formatter/commit/painter implementation.

Current evidence covers exact source revisions, full resync and mutation batches,
block/inline/flex/grid formatting, Paragraph geometry, accepted PNG resources,
commit-bound hit testing, pointer/key/text input, semantic actions and bounds,
root scroll commits, scene PNGs at multiple viewports, renderer reset/recovery,
and same-task element/Range/caret CSSOM geometry. Hidden/stale/missing commits
fail closed with no native pixel fallback.

The C ABI has no frame descriptor or raw coordinate input. Pointer commands must
carry an exact displayed-commit query and optional Flutter hit target. BrowserCore
accessibility snapshots carry semantic meaning but no fabricated layout bounds;
Flutter commits supply displayed semantic geometry.

`fixtures/manifest.json` keeps source and rendered assertions together. Native
WPT runs source/runtime checks only. Renderer-dependent JavaScript is tagged
`flutter-js-eval`; `layout-box`, `visual-hash`, and `ref-equivalent` are also
Flutter-only. `just flutter-fixture-manifest` remains the complete rendered
compatibility measurement.

Known shell gaps remain broader device/IME/AT matrices, non-Linux production
runners, full CSS and text shaping breadth, advanced nested/smooth scrolling,
GPU/compositor recovery on physical systems, process isolation, performance/size
budgets, packaging, and sustained release evidence.

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
| CSS cascade/computed values | Inline plus one external stylesheet vertical green | High after full Stylo slice | Compact cascade is temporary; external-sheet media/import/CSSOM breadth and full Stylo remain. |
| CSS layout: block/inline | v1 visual/ref subset green | Medium | Flutter-hosted Vixen formatter; start with normal flow, margin/border/padding, Paragraph-backed inline lines. |
| CSS layout: flex/grid | Useful common-case subset green | Medium | Vixen Dart formatting contexts over Flutter primitives; full WPT edge coverage is post-v1. |
| CSS layout: tables/floats/fragmentation | Not v1 release-blocking | Low for v1 | Document as unsupported/partial until implemented. |
| DOM Core | Traversal, attributes, token lists, ranges, mutation observer subset green | Medium | Vixen-owned Web APIs over `deno_core` host extensions after the ADR-014 migration. |
| Events/forms/history/storage | Selected behavioral subset green | Medium | Gate by fixtures from SPEC invariants and imported WPT cases. |
| JS language | Use V8/`deno_core` language coverage, not WPT percentage | High for language | Web API exposure remains Vixen-owned and separately gated. |
| Paint/ref tests | Flutter scene/commit visual subset green | Medium | One formatter/Canvas path; pixels, geometry, hit/text/scroll, and semantic bounds share a commit. |
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
