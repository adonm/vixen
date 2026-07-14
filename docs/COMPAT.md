# Vixen compatibility target

This is the honest v1.0 target matrix. It is not a claim of full Firefox or full
WPT compatibility. Vixen delegates CSS cascade/selectors, HTML parsing, JS
execution, and paint where credible upstream Rust/Firefox-family components
exist; layout is Vixen-owned Rust code per ADR-013 and therefore WPT-gated by
feature.

---

## Current measured committed fixture baseline

As of 2026-07-14, `fixtures/manifest.json` contains 70 local fixtures plus
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
| `computed-style` | 170 |
| `element-attribute` | 132 |
| `layout-box` | 104 |
| `body-contains` | 68 |
| `visual-hash` | 25 |
| `no-critical-diagnostics` | 22 |
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
snapshot host-object evals, and read-only `DOMTokenList`/`DOMStringMap` property
reads are also exercised directly through the persistent `deno_core` runtime seam.
Runtime platform smoke now additionally covers secure `crypto.getRandomValues()` /
`randomUUID()`, async Clipboard text and `ClipboardItem` shape, `MessageEvent`,
`MessageChannel`, `BroadcastChannel`, first-callback `IntersectionObserver` /
`ResizeObserver` geometry, and a fail-closed `WebSocket` close path. Imported
smoke fixtures now also seed
block/inline/position layout, flexbox, grid, and display-list `ref-equivalent`
paint; imported layout smoke covers auto margins, border-box sizing, inline
flow, flex reverse/gaps, and grid `minmax()`/fractional row/gap cases. Imported
paint smoke now covers currentcolor, overflow clipping, positioned boxes,
flex/grid backgrounds, and nested background/text display-list equivalence.

---

## Current automation smoke baseline

The external Playwright smoke covers connect/target/page/runtime/DOM/input/
network/dialog/screenshot/history/content/script/style/binding paths plus
browser-context permission grant/reset, bounded Chromium JSON tracing through
CDP `IO` streams, idle stop-loading behavior, and stable protocol errors. CDP
permission overrides are exact-origin or wildcard scoped and do not mutate
persisted user decisions. Trace records contain method/timing/session/success
metadata only, not expressions, request headers, form values, or page text.

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
other local native host calls remain open. There is still no HTTP download
manager or Playwright context-tracing archive implementation.

---

## Current Flutter shell smoke baseline

Flutter is the sole rendered GUI. `just gate-flutter-shell` covers its
BrowserCore controller, tabs, input, texture, and Semantics seams;
`just linux-release-smoke` covers the exact release archive under native
Wayland. `just linux-interaction-smoke` separately drives that release bundle
through Cage's virtual-keyboard and wlr virtual-pointer protocols, with GTK/IBus
preedit/commit and native nested-wheel evidence. These are integration gates
rather than WPT surfaces.

The Flutter semantics projection additionally carries bounded `aria-controls`,
`aria-describedby`, and `aria-details` relationships to retained semantic nodes.
Descriptions resolve bounded referenced text (including hidden referenced
content), then `aria-description`, then an unused `title`, and map to Flutter's
semantic hint. Native range min/max/current/step state and enabled
`input[type=range]` increase/decrease actions execute through the same
generation-checked BrowserCore/runtime path as focus and set-value. This is
widget/native-bridge evidence. Authored `slider`/`spinbutton` roles with a finite
`aria-valuenow` expose bounded min/max/current state and optional
`aria-valuetext`; increase/decrease dispatch the orientation-appropriate
`keydown` to the exact live target so the author remains responsible for
updating ARIA state. This is not native AT or broad ARIA support.

Explicit `aria-live="polite"`/`"assertive"` and the implicit `alert`, `log`,
`marquee`, `status`, and `timer` roles map to Flutter live regions; explicit
`aria-live="off"` disables the implicit mapping. Runtime-effect events for the
active context force a fresh generation-paired frame and full semantics
snapshot, so same-document live mutations are not suppressed by the normal
same-key capture coalescing. This is event-driven full-projection refresh, not a
delta protocol or native assistive-technology proof.

Focused writable native text inputs, textareas, and direct contenteditable
editing hosts project the live runtime's bounded UTF-16 selection base/extent
through BrowserCore and ABI v1 into Flutter's semantics configuration. Selection
changes participate in the source generation, while unfocused controls and
authored ARIA-only textboxes do not fabricate caret state. General document-
range selection remains outside this slice.

Those native controls and contenteditable hosts also attach Flutter's platform
text-input client. Each update carries a value capped at 16 KiB plus selection
and optional composing ranges in UTF-16 units through exact context/document/
runtime ids. BrowserCore validates every range against the value, applies it to
the live focused editing host, and emits composition-shaped events plus
cancelable `beforeinput` and `input`; stale or non-writable targets fail closed.
Widget/wire tests cover the shared transport, and BrowserCore tests cover native
non-ASCII plus contenteditable surrogate-pair composition. The release-process
interaction smoke uses IBus Anthy through Flutter's real Linux text-input client
for native and contenteditable preedit/commit; broader desktop-IME/language
coverage remains open. BrowserCore normalizes all standard
`inputmode` values plus supported native input types into bounded keyboard
intent and all standard `enterkeyhint` values into action intent. Flutter maps
those values to its none/text/multiline/numeric/decimal/telephone/email/URL/search
keyboard configurations and Newline/Done/Go/Next/Previous/Search/Send actions.
`performAction` dispatches Enter down/up through the existing exact-generation
key path; invalid or absent hints retain the multiline/search/single-line
defaults.

Top-level script scrolling now shares the Page-owned offset used by wheel/key
defaults, paint, hit testing, find, and Semantics. The live runtime exposes
numeric and options-object `scroll()`/`scrollTo()`/`scrollBy()`, synchronized
`scrollX`/`scrollY` and `pageXOffset`/`pageYOffset`, and root/body
`scrollTop`/`scrollLeft`; BrowserCore refreshes the CSS viewport and clamps the
offset to current layout overflow on host-view and page-zoom changes. Actual
top-level changes from script, uncanceled input defaults, find traversal,
viewport changes, and zoom changes emit one non-cancelable bubbling document
`scroll` event, coalesced after the current script evaluation, with synchronized
offsets; document and window listeners observe the new value. Canceled defaults
and clamped no-ops stay silent, and recursive synchronous dispatch is
suppressed. Nested `auto`/`scroll` element offsets are now Page-owned and shared
by paint, clipped hit testing, accessibility geometry, script-visible
`scrollTop`/`scrollLeft`, and `scroll()`/`scrollTo()`/`scrollBy()`. Element scroll
events are non-bubbling, non-cancelable, and coalesced; uncanceled wheel input
prefers the nearest scrollport, chains unconsumed deltas through ancestors/root,
and `scrollIntoView()` drives the same nested offsets for CDP/Playwright.
BrowserCore captures the root and up to 1,024 document-identified nested offsets
in each current history entry. Reload and cross-document back/forward restore
that state after layout clamping when `history.scrollRestoration` is `auto`;
`manual` leaves a newly loaded document at zero, and the live history/window/
element state is resynchronized. Smooth scrolling, axis-specific overflow
behavior, DOM touch/pointer events, inertia/multi-touch gestures, restoration
scroll-event ordering, and BFCache-style document preservation are not claimed.
Flutter single-touch drags cross platform touch slop, cancel the pending
synthetic press, and reuse the cancelable physical-delta wheel path.

Bounded `aria-owns` references now reparent only retained later semantic nodes;
the first valid owner wins, parent-before-child ordering remains enforced, and
cycles/backward ownership are ignored. Native `h1`–`h6` and valid authored
`aria-level="1"`–`"6"` map to Flutter heading levels. `aria-checked="mixed"`
maps to Flutter's tri-state semantics rather than being discarded as an invalid
boolean.

The Flutter coordinator stages a refreshed frame and semantics snapshot under
one projection generation and publishes both atomically. Node reconciliation
keys include context/document/node identity plus bounded semantic content, but
not the whole-snapshot generation, so unchanged nodes retain platform identity
while changed nodes are replaced. BrowserCore and the ABI still send a bounded
full authoritative snapshot; wire-level semantic deltas are an optimization,
not required state ownership.

`just linux-at-spi-smoke` launches the real release/AOT Flutter bundle in Cage's
headless Wayland compositor with a fresh BrowserCore profile and
`fixtures/dom/basic.html`, then filters the native AT-SPI tree by the launched
process and requires the BrowserCore-derived `DOM Basic` heading. This is
concrete Linux native-bridge evidence, not a screen-reader interaction matrix or
evidence for non-Linux accessibility backends. The Linux GUI rejects X11 and
XWayland at startup.

`just linux-interaction-smoke` uses AT-SPI only to locate and observe the
release-process controls. Focus/click, IBus composition, and scrolling enter via
Wayland virtual keyboard/pointer protocols; the test does not use AT-SPI
`setText` or direct BrowserCore input commands. It requires composition
start/update/end for both a native input and direct contenteditable host, proves
an uncanceled wheel selects the nested scrollport, verifies authored
`preventDefault()` leaves both offsets unchanged, and proves unconsumed wheel
delta chains to the root at the inner boundary.

Flutter also sends one monotonic BrowserCore-owned host-view state for content
focus, visibility, effective scale, and application lifecycle. Current documents
expose the accepted state through `document.hasFocus()`, `hidden`, and
`visibilityState`, dispatch focus/blur and `visibilitychange`, and reject input
while inactive. CSS-versus-physical scale correction and platform lifecycle/
native surface recovery are not established by this slice.

The Flutter coordinator now retries a failing current-generation BrowserCore
frame or Semantics capture twice while preserving the exact context/document/
viewport/projection keys. The texture presenter also disposes and recreates its
controller after a failed create/publish, with two retries per frame; exhaustion
shows a recovery-failed placeholder rather than looping, and a newer frame gets
a fresh bounded attempt. Deterministic fake-controller/widget tests prove this
policy. Real compositor surface loss, GPU reset, application lifecycle recovery,
and native runner evidence remain open.

The first interactive root-scrolling slice is BrowserCore-owned. Flutter scales
wheel deltas into the physical frame coordinate space; the live runtime receives
a cancelable `wheel` event; and only an uncanceled default action updates the
bounded Page scroll offset. Unmodified Arrow, Page Up/Down, Home/End, and Space
keydown defaults use the same CSS viewport and offset, including page zoom;
`preventDefault()` blocks the scroll and focused native/editing controls retain
their own key handling. The same translated layout projection drives WebRender
paint, hit testing, selector/accessibility bounds, while fixed-position subtrees
remain viewport anchored. A single Flutter touch drag crosses platform touch
slop, cancels the pending synthetic press, and feeds physical deltas into that
same cancelable root path; taps remain clicks and secondary touches are ignored.
Nested scroll containers, element scroll events, and bounded history restoration
use the shared Page-owned offsets described above. DOM touch/pointer-event
fidelity, inertia/multi-touch, smooth scrolling, and restoration-event ordering
remain open. Top-level scroll events and script `scrollTo`/`scrollBy` use the
shared offset as described above.

Flutter Ctrl+F sends a UTF-8-byte-bounded query with the exact active context and
document generation through ABI v1. BrowserCore derives up to 10,000
non-overlapping matches from rendered Page text nodes, excluding hidden,
`display:none`, and title/head content. Enter/F3 and the Previous/Next controls
advance or reverse with wrapping; Page owns the one-based active match and moves
the same clamped root offset used by paint, hit testing, and Semantics just enough
to reveal it. Soft-wrapped phrases remain one logical match while each intersected
text run receives a highlight. The generation-checked result is exposed through a live region and
forces a paired frame/Semantics refresh after traversal. Empty queries clear the
active match; stale documents and queries above 4 KiB fail closed. Range-sized
highlights are inserted before text in the one display list (orange
for the active match, yellow for other matches). Their horizontal geometry uses
the current deterministic text-run metrics; shaped-glyph precision follows the
font-shaping milestone rather than creating a second find paint path.

Page zoom is BrowserCore-owned per top-level context and bounded to 25–500%.
Ctrl++/Ctrl+-/Ctrl+0 and menu actions send zoom intent through ABI v1; the core
derives the CSS layout viewport from the physical frame, scales the same display
list into that frame, maps physical hit-test/wheel coordinates back to CSS
pixels, and scales accessibility bounds into the displayed coordinate space.
Zoom survives document navigation in the context but is not yet persisted in
the profile session. Text shaping quality, advanced scroll behavior,
device-scale correctness, and native surface-loss evidence remain separate gaps.

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
| DOM Core | Traversal, attributes, token lists, ranges, mutation observer subset green | Medium | Vixen-owned Web APIs over `deno_core` host extensions after the ADR-014 migration. |
| Events/forms/history/storage | Selected behavioral subset green | Medium | Gate by fixtures from SPEC invariants and imported WPT cases. |
| JS language | Use V8/`deno_core` language coverage, not WPT percentage | High for language | Web API exposure remains Vixen-owned and separately gated. |
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
