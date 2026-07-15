# Vixen

[![CI](https://github.com/adonm/vixen/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/adonm/vixen/actions/workflows/ci.yml)
[![Pages](https://github.com/adonm/vixen/actions/workflows/pages.yml/badge.svg?branch=main)](https://github.com/adonm/vixen/actions/workflows/pages.yml)
[![Docs](https://img.shields.io/badge/docs-vixen.adonm.dev-blue)](https://vixen.adonm.dev/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](Cargo.toml)
[![GUI target](https://img.shields.io/badge/GUI-Flutter%203.47%20beta-02569B.svg)](docs/FLUTTER_SHELL.md)

A focused cross-platform Firefox replacement: one Flutter web renderer and GUI
targeting Linux, macOS, Windows, Android, and the Apple Silicon iOS Simulator,
plus first-class chrome-less Flutter/CDP automation and the most web capability
per byte.

**Linux is Vixen's highest-priority GUI and release target.** Browser usability,
host integration, packaging, accessibility evidence, and performance gates land
on Linux first. macOS, Windows, Android, and the iOS Simulator remain committed
targets and reuse the proven BrowserCore/Flutter contract after the Linux path.
FlatPark publishing is deliberately deferred until the Linux Flutter shell is a
basic usable browser: visible navigation, scrolling, text input/IME,
back/forward/reload/stop, find/zoom, and bounded failure recovery take priority
over package-registry work.

The hard, spec-heavy primitives are delegated where that keeps Vixen smaller and
more correct: **Stylo/selectors** for CSS matching and cascade,
**deno_core/V8** for JavaScript, **html5ever** for HTML, and Flutter's
**Paragraph/Canvas/scene/Semantics** substrate for cross-platform rendering.
BrowserCore owns DOM/runtime/navigation/network/security/persistence, computed
styles, accepted resources, web events, and accessibility meaning. The Vixen
renderer hosted in Flutter owns CSS formatting, text/image measurement, paint,
hit testing, scroll geometry, semantic bounds, and scene capture through bounded
revision/mutation/commit/query protocols. Flutter's public scene APIs run over
explicitly enabled Impeller; a Skia-backed launch is not accepted renderer or
release evidence. See
[`docs/PROJECT_DIRECTION.md`](docs/PROJECT_DIRECTION.md) for the current focus.

**Renderer migration is the immediate priority.** The currently implemented
WebRender/EGL/RGBA texture and native-headless path is a frozen transitional
baseline. After the Flutter vertical, chrome-less fixture/CDP/Playwright host,
synchronous layout, cancellation, resync, and renderer-loss gates pass, Vixen
will cut over once and delete WebRender/EGL/frame transport and superseded Rust
layout/paint rather than carry two renderers.

The R1 protocol core, R2 dedicated C/Dart broker, R3 Canvas/Paragraph/PNG
formatter vertical, and R4 displayed-commit interaction/scroll/Cage proof are
implemented. R5 now has a first release-process chrome-less host checkpoint: the
same Linux bundle accepts one bounded explicit URL/viewport/output invocation,
omits Flutter and native browser chrome, disables the legacy frame capture, and
writes the exact acknowledged Flutter scene as PNG. `just linux-automation-smoke`
checks two viewport sizes under Cage. Fixture-manifest, CDP/input, multi-target,
and renderer-loss migration remain the next R5 work; WebRender/RGBA remains the
explicit fallback until the one-time R7 cutover.

The checked-in transitional Linux Flutter alpha shell uses handwritten Dart FFI over the
one-owner `vixen-ffi` controller and presents real BrowserCore/WebRender output
through a bounded RGBA pixel-buffer texture. Its chrome uses the locked Yaru
widget suite with the Adwaita-blue variant, Yaru icons/controls, and a
Flutter-rendered titlebar containing the BrowserCore-backed tab strip. The
native GTK headerbar remains only as a startup fallback and is hidden after the
Yaru window plugin initializes. The Linux GUI is native-Wayland-only; X11 and
XWayland startup fail explicitly. Its fake-controller, worker,
coordinator, texture, input, and live native smoke tests run with
`just gate-flutter-shell`; a relocatable debug bundle has also compiled. Physical
viewport plus pointer/wheel/keyboard input are routed through BrowserCore;
uncanceled wheel input and uncanceled navigation-key defaults now update a
bounded Page-owned root scroll offset. Arrow, Page Up/Down, Home/End, and Space
scrolling respect focused controls and page `preventDefault()` handlers; paint,
hit testing, and accessibility bounds consume that same offset while fixed-
position content remains viewport anchored. Live page scripts use that same
offset through bounded `scroll()`/`scrollTo()`/`scrollBy()`, `scrollX`/`scrollY`,
and root-element `scrollTop`/`scrollLeft` projections; actual offset changes from
script, input defaults, find traversal, viewport clamps, and zoom clamps emit a
non-cancelable document `scroll` event observable at `window`, while canceled
and clamped no-ops stay silent;
Ctrl+F opens a bounded BrowserCore-backed find bar; Enter/F3 and Previous/Next
traverse up to 10,000 case-insensitive rendered-text matches, wrap in document
order, use the Page-owned root offset to reveal the active match, and paint
active/other range highlights through the same display-list/WebRender path;
per-tab zoom from 25–500% derives a CSS viewport inside BrowserCore and applies
one matching transform to WebRender commands, hit testing, wheel coordinates,
and Semantics bounds;
current-generation frame and Semantics captures retry twice before surfacing a
structured error, while texture create/publish failures dispose and recreate
the bounded Linux texture twice before showing a recovery-failed placeholder.
Flutter lifecycle detach/hidden/paused now invalidates the texture epoch and
serializes native disposal behind any in-flight publish; resume/inactive waits
for disposal before recreating. A deterministic widget/controller fault proves
an in-flight stale frame cannot become visible and a newer frame survives one
injected post-resume texture loss within the same two-retry bound;
single-touch drags cross platform touch slop, cancel the pending synthetic press,
and reuse BrowserCore's cancelable physical-delta scroll path; Page-owned nested
scrollports now share paint, clipped hit testing, Semantics bounds, wheel
cancellation/boundary chaining, element scroll events, and CDP scroll-into-view;
the bounded BrowserCore-derived Semantics hierarchy includes parent structure,
`aria-controls`, text editing, and native range adjustment. Monotonic host-view
updates also drive document focus/visibility and Flutter lifecycle input policy.
Bounded accessible descriptions plus retained `aria-describedby` and
`aria-details` relationships now cross the same BrowserCore/ABI projection.
Authored ARIA sliders/spinbuttons with finite numeric state also expose live,
generation-checked adjustment actions. Explicit `aria-live` plus implicit live
roles project into Flutter live regions, and runtime-effect events trigger a
fresh frame/semantics pair for the active document. Focused writable native text
controls now carry live UTF-16 selection offsets into Flutter Semantics. A
focused writable native text control or contenteditable host also attaches
Flutter's platform text-input client and sends bounded full text, UTF-16
selection, and composing ranges through the exact BrowserCore document/runtime
generation; the live editing host receives composition-shaped plus
`beforeinput`/`input` events. BrowserCore also projects single-line versus
multiline intent plus normalized `inputmode`, input-type, and `enterkeyhint`
intent so Flutter configures the corresponding platform keyboard and action;
performed actions reuse the exact-generation Enter key path.
`aria-owns` reparenting, heading levels, and mixed checkbox state also map into
the platform tree. Same-document refreshes replace frame/semantics atomically
and reconcile only changed semantic nodes. A release-bundle AT-SPI smoke observes
BrowserCore's `DOM Basic` heading through the native Linux tree. Broader Linux
IME/device evidence, complete accessibility/screen-reader coverage, basic-browser
parity, and host services remain open. Flutter is the only rendered frontend
target; there is no secondary Rust/GTK shell or fallback renderer.

---

## Status

Pre-v1.0. This repository contains the specification, architecture, plan,
and reference material, plus:
- **Phase 0** — scaffolding (workspace, engine/adapters, and native Flutter bridge core).
- **Phase 1** — networking/security "crown jewels" (`vixen-net`, `vixen-store`).
- **Phase 2** — the `deno_core` JS runtime seam (`vixen-engine::script`) and the
  `vixen-headless` CLI; `vixen-headless --url <file> --eval '1+2'` → `3` passes
  behind the stable `JsRuntime`/`JsValue` seam.
- **Phase 3 (in progress)** — HTML parsing (`vixen-engine::doc`,
  html5ever → RcDom) with `--dump-dom`/`--extract-text`; **selector matching
  via Stylo** (`vixen-engine::style_dom` implementing `selectors::Element` over
  the RcDom), driving `--extract-selector` and the WPT selector fixtures;
  the shared `vixen-engine::page::Page` facade; author `<style>` blocks and
  inline `style` declarations now project through `Page::computed_style(node_id)`
  with specificity/source-order/`!important`, cascade layers, `@media`,
  `@supports`, inherited custom properties, `var()` fallback, and CSS-wide
  keyword coverage through WPT `computed-style` checks plus a Page-backed
  `getComputedStyle()` eval smoke seam; and the **WPT harness**
  (`vixen-wpt`: manifest + runner + all 15 check types). The full Stylo cascade
  (`TNode`/`TElement`/`TDocument` + `Stylist::update_stylist` +
  `computed_values_for(node_id)`) remains the implementation replacement behind
  the same `Page` facade; Stylo arrives via the crates.io-published `stylo`
  crate per ADR-011 (no Servo git dep). Parser-discovered non-alternate external
  stylesheets now load through BrowserCore's cancellable bounded text-resource
  path before author scripts, with redirect CSP/mixed-content/URL policy,
  status/`nosniff`, cookie/cache, exact-generation, live cascade/runtime-host,
  layout, and paint checks. A checked-in file fixture plus gated HTTP/supersede
  tests prove visible style and reject stale persistence/application; dynamic,
  alternate, imported, broad-media, and complete CSSOM sheet behavior remain.
- **Phase 4 prep** — `vixen-engine::box_model` implements the CSS2 § 10.3.3
  block-level horizontal-constraint solve (`auto`-width leftover absorption,
  one/two `auto`-margin centering, `box-sizing: border-box` content
  subtraction) and the four-box nesting. `vixen-engine::flex_resolve`
  implements CSS Flexbox 1 § 9.7 main-axis distribution (grow/shrink factor
  selection, inflexible-item freezing, min/max violation clamping, iterative
  free-space distribution).   `vixen-engine::grid_resolve` implements CSS Grid
  1 § 12.5 fr-factor distribution + § 11.7 track maximization (the natural
  complement to `flex_resolve` for grid columns/rows, with the iterative
  growth-limit clamp-and-redistribute pattern). `vixen-engine::writing_modes`
  implements CSS Writing Modes 3 § 3 + CSS Logical Properties 1 — the
  `writing-mode` / `direction` → block + inline axis + the logical → physical
  side mapping (`block/inline-start/end` → `top/right/bottom/left`, the
  `inline-size`/`block-size` → `width`/`height` swap for vertical modes) the
  box model, the logical insets, and the flex/grid main-axis selection
  resolve against. `vixen-engine::multicol` implements CSS Multi-column
  Layout 1 § 3 — the `column-width` / `column-count` / `column-gap` § 3.4
  resolution (the four-branch pseudo-algorithm + the single-column overflow
  clamp) the column-row distribution reduces to. `vixen-engine::scroll_snap`
  implements CSS Scroll Snap 1 § 5 — the snap-position computation
  (`start`/`end`/`center` per axis, clamped to the scrollable range) + the
  `scroll-snap-type` axis/strictness + `scroll-snap-align`/`scroll-snap-stop`
  model the scroll layer's snap targeting reduces to. These six pure helpers and
  the vertical layout surface are part of the transitional Rust renderer:
  `vixen-engine::line_layout` + `Page::dump_lines` power
  `vixen-headless --dump-lines`, and `Page::layout_fragments(viewport)` now
  projects block backgrounds plus wrapped text lines into paint-consumable
  fragments with deterministic text metrics. R7 deletes them unless the Dart
  formatter consumes a pure algorithm through an explicit stable contract.
- **Phase 5 transitional path** — `vixen-engine::display_list` has a live surface:
  `Page::display_list` turns layout fragments into invariant-enforced paint
  commands and `vixen-headless --dump-display-list` dumps them; `--paint-stats`
  reports command counts and painted area from the same stream. Parser-discovered
  PNG `<img>` resources now reuse BrowserCore's cancellable redirect/cookie/cache/
  policy path, enforce 8 MiB body, 2048-pixel-axis, 16 MiB decoded-RGBA, static-
  image, status, and `image/png` limits, and enter that same display list as
  WebRender image resources. The checked-in four-colour fixture has exact-pixel
  headless and Flutter-frame capture proof. Dynamic/responsive images, animated
  PNG, other formats, and complete replaced-element sizing remain open. The
  frozen Rust path also contains this paint-geometry family:
  `vixen-engine::transform` (CSS Transforms 1 § 13 2D affine algebra +
  list parser), `vixen-engine::border_radius` (CSS Backgrounds 3 § 5.5
  corner shaping), `vixen-engine::gradient` (CSS Images 4 § 4.5
  linear-gradient colour-stop resolution + linear-sRGB sampling, with the
  `repeating-linear-gradient()` wrap), `vixen-engine::radial_gradient`
  (CSS Images 4 § 4.2.3–§ 4.2.4 `radial-gradient` colour sampling — the
  four size keywords `closest-side`/`farthest-side`/`closest-corner`/
  `farthest-corner` + the circle/ellipse distance projection), and
  `vixen-engine::conic_gradient` (CSS Images 4 § 4.3.3 `conic-gradient`
  colour sampling — the per-pixel angle → `t` projection, the
  `from <angle>` start offset, and the `repeating-conic-gradient()` wrap),
  `vixen-engine::box_shadow` (CSS
  Backgrounds 3 § 7.2 outer/inset shadow geometry + the `<shadow>#`
  parser), `vixen-engine::background_position` (CSS Backgrounds 3 § 3.6 +
  § 4.2 `<position>` resolution: keyword/length/percentage mix, the 1–4
  value forms, the keyword-axis swap rule), and `vixen-engine::stacking_context`
  (CSS 2.1 § 9.9.1 + Positioned Layout 3 § 6 stacking-context formation +
  the seven-layer § App. E.2.1 paint-order classification). The paint
  compositing family: `vixen-engine::blend` (CSS Compositing 1 § 5 + § 10 —
  the 13 Porter-Duff operators + the 16 blend modes operating in linear
  sRGB, with the § 5.2 combined isolation-blend pipeline `mix-blend-mode`
  runs), `vixen-engine::filter` (CSS Filter Effects 1 § 5 — the
  `<filter-function-list>` grammar + the per-pixel `feColorMatrix`-shaped
  4×5 matrix family the paint path folds into one multiply), and
  `vixen-engine::border_image` (CSS Backgrounds 3 § 6 — the four longhands
  `border-image-slice`/`-width`/`-outset`/`-repeat`, the 3×3 nine-region
  carving, and the `stretch`/`repeat`/`round`/`space` edge tiling). The
  clip-path + mask family: `vixen-engine::clip_path` (CSS Masking 1 § 5
  `clip-path` basic shapes — `inset`/`circle`/`ellipse`/`polygon` with the
  per-pixel point-in-shape test + the polygon nonzero/evenodd winding
  rules) + `vixen-engine::mask` (CSS Masking 1 § 6 `mask` shorthand
  per-layer model — `mask-mode`/`mask-repeat`/`mask-clip`/`mask-origin` +
  the paren-aware comma-separated layer parse). The animation timing model:
  `vixen-engine::animation` (Web Animations § 5 — the phase classification,
  the simple iteration progress + current iteration, the `direction`-aware
  directed progress, the easing-transformed progress via `easing::Easing`,
  and the `fill`-mode before/after resolution the transition/animation
  drivers reduce to). The geometry-interfaces surface: `vixen-engine::geometry`
  (CSS Geometry Interfaces L1 — `DOMPoint`/`DOMRect`/`DOMQuad`/`DOMMatrix`
  with the full 4×4 matrix algebra + the perspective divide
  `Element.getBoundingClientRect()`/`IntersectionObserver`/`DOMMatrixReadOnly`
  reduce to). All
  `#![forbid(unsafe_code)]` and Rust-unit-tested.
- **Phase 6 prep** — pure form-constraint validation in `vixen-engine::forms`
  (email/URL formats, step arithmetic, range/length flags) ready for the
  script-layer host hooks and exposed through the `deno_core` DOM host-object
  seam for `ValidityState` / `checkValidity()` smoke checks;
  `vixen-engine::form_submission` (the three WHATWG HTML § 4.10.21 encoders:
  `application/x-www-form-urlencoded`, `multipart/form-data`, `text/plain`,
  now also feeding the Page-backed `FormData` entry-list + iterator smoke seam
  and runtime/CDP form submission by node id, including idless forms,
  successful submitter entries, and submitter `formaction` / `formmethod` /
  `formenctype` overrides; runtime form reset restores default value/checked/
  selected state and honors cancelable `reset` events);
  `vixen-engine::dataset` (WHATWG HTML
  § 3.2.6.9 `data-*` ↔ `dataset` property-name bidirectional mapping, with
  the anti-collision rule, now reflected by the Page-backed `dataset` eval
  seam); `vixen-engine::storage_key` (Web Storage key/value
  validation + origin-partitioned redb keys + the 5 MiB quota, now used by the
  runtime-backed in-memory `localStorage` / `sessionStorage` mutation seam);
  document and
  navigator state, DOM ancestry/core-node shape (`closest()`, `nodeName` /
  `nodeType`, `ownerDocument`), plus `Event` / `CustomEvent` / `dispatchEvent()`
  smoke, CSSOM `CSS.supports()` / `document.styleSheets` / CSSStyleRule shape,
  viewport/window state (`innerWidth`, `visualViewport`, `screen`), DOMRect
  geometry via `getBoundingClientRect()` / `getClientRects()` plus
  client/offset/scroll metrics, `getBoxQuads()`, and Range rectangles,
  Geometry Interfaces value constructors
  (`DOMPoint`/`DOMRect`/`DOMQuad`/`DOMMatrix`), `DOMParser`, and `atob` / `btoa`
  are exposed through `deno_core` runtime host objects and value bootstraps.
  Focused document/query/element evals (`document.title`, simple
  `querySelector`/`getElementById`, `querySelectorAll().length`) plus read-only
  `classList`/`relList`/`sandbox` and `dataset` property reads use the
  transitional Page-backed document snapshot. Every evaluation adapter routes
  through BrowserCore and `JsRuntime`; there is no string-expression fallback.
  `JsRuntime` owns a
  persistent realm, so sequential evals retain globals, storage, and pending
  promise/event-loop state until switching between non-page and page realms or
  navigating to a new page snapshot; the next JS-runtime milestone widens these
  bootstrap surfaces through explicit `deno_core` op/resource extensions.
  The network
  host-hook family: `vixen-engine::url_search_params` (WHATWG URL Standard
  `URLSearchParams` parse/serialize + the full mutating surface; both now feed
  Page-backed `URL.canParse()` / `new URL()` / `URLSearchParams` constructor,
  lookup, and iterator eval smoke checks),
  `vixen-engine::mime` (WHATWG MIME Sniffing § 2.1/§ 2.2 parse/serialize +
  `essence()`), and `vixen-engine::text_codec` (WHATWG Encoding API
  `TextEncoder`/`TextDecoder` with constructor label/options, `encodeInto`, the
  `fatal` flag, BOM sniff, and § 7.1 line-break normalisation, now exposed
  through the Page-backed Encoding API eval seam and `deno_core` global
  constructors). `vixen-engine::headers`,
  `abort`, `mime`, and `url_pattern` now also feed Page-backed `Headers`
  iteration, `Blob`/`File`,
  read-only `Request`/`Response` state with forbidden-header filtering, static
  `Response.error()` / `Response.redirect()` / `Response.json()`, an op-backed
  `fetch()` MVP that routes HTTP(S) through `vixen-net` for request headers,
  request bodies, status/headers/body, URL-policy/private-host rejection, page CSP `connect-src`,
  referrer-policy header generation, active mixed-content blocking,
  `same-origin`/`cors`/`no-cors` mode enforcement, CORS preflights for non-simple
  methods/headers, credential authorization, bounded `Access-Control-Max-Age`
  caching partitioned by source/target/credentials, CORS response filtering,
  strongest-algorithm Request SRI verification before exposure/cache insertion,
  bounded profile cache reads/writes, and `cache: 'no-cache'` ETag /
  Last-Modified revalidation, with stable request/redirect/response/failure event
  traces drained by `JsRuntime` and surfaced as CDP `Network.*` notifications
  (including `Network.loadingFailed` with a blocked reason for policy failures),
  a minimal fetch-backed `XMLHttpRequest`, `AbortController`/`AbortSignal`, and
  `URLPattern` eval smoke checks. The
  DOM-serialisation surface:
  `vixen-engine::html_serialize` (WHATWG HTML § 13.2.9 fragment serialisation
  — the `Element.innerHTML` / `outerHTML` / `document.write` getter pipeline,
  with the void-element + raw-text + text-escape + attribute-escape tables,
  now projected through `innerHTML` / `outerHTML` eval smoke checks).
  The `vixen-engine::class_list` (WHATWG HTML
  § 4.6.4 `DOMTokenList` + § 2.7.3 ordered-set parser: `add`/`remove`/
  `toggle`/`replace`/`contains` with the spec's atomic validate-then-mutate
  rule, the supported-tokens surface for `<link>.relList`) backs every
  `element.classList` / `relList` / `sandbox` host-hook reflection and now
  feeds the Page-backed WPT adapter plus focused `deno_core` `classList` /
  `relList` / `sandbox` evals. Security
  meta `content` / `charset` reflection is likewise covered by Page-backed
  eval checks before CSP/referrer enforcement consumes it, and
  `navigator.permissions.query()`, `Notification.permission` /
  `requestPermission()`, and `navigator.storage.persisted()` now cross a
  profile-store permission op with unknown decisions staying `prompt` /
  `default`; `navigator.storage.estimate()` reports bounded local-storage usage;
  anchor URL decomposition (`href`,
  `origin`, `protocol`, `host`, `pathname`, `search`, `hash`) is Page-backed for
  link/navigation fixtures; `HTMLImageElement` reflection now covers
  alt/dimensions/loading/decoding/complete/decode smoke; and inert
  `HTMLMediaElement` / audio / video state reflection covers media-control
  automation probes without claiming decode support. Resource element reflection
  now covers `link` / `style` / `script` / `source` attributes plus
  `HTMLScriptElement.supports()` smoke, and details/dialog open-state reflection
  covers dialog show/close automation probes. Miscellaneous HTML reflected
  attributes now cover lists, quotes/time edits, image maps, embedded content,
  table-cell spans/headers, progress/meter numeric state, and an inert Canvas 2D
  context for automation smoke. Form-associated reflection now covers submitter
  override attributes, text-control editing helpers, numeric stepping, and
  custom validity messages, and table collection/index properties now expose
  read-only row/body/cell structure. HTMLElement interaction/global attributes
  (`tabIndex`, access keys, editing hints, drag/spellcheck/translate, popover)
  are reflected for automation probes, and text-track state now exposes
  `HTMLTrackElement.track` plus media `textTracks` lists. Inert canvas adjuncts
  now cover `ImageData`, `OffscreenCanvas`, `ImageBitmap`, and `Path2D` smoke.
  Minimal `ShadowRoot` / `DocumentFragment` host objects cover attach-shadow
  automation shape before composed-tree layout lands, with template `content` and
  slot assignment methods shaped for web-component probes. DOM construction and
  serialization helpers now cover `createElementNS()` and `XMLSerializer` smoke.
  The CSS
  Values 4 dimension family (`length`,
  `color`, `angle`, `time`, `resolution`) — the value primitives the
  cascade/layout/paint resolves against — is now complete for v1.0; `<length>`
  includes logical viewport units plus the small/large/dynamic viewport
  families (`sv*`/`lv*`/`dv*`), pure sRGB colour arithmetic + interpolation,
  premultiplied alpha, hue/unit normalisation, and dots-per-pixel conversion
  are all Rust-unit-tested. They are transitional renderer inputs, not future
  WebRender work; R7 deletes any without an explicit Dart formatter consumer.
  The responsive-image
  selection family (`media_query`, `source_size`, `responsive_select`)
  completes the WHATWG § 4.8.4.6–§ 4.8.4.8 pipeline end-to-end: CSS Media
  Queries 4 condition evaluation against a `Viewport` (including `screen` /
  `print` output contexts and `any-hover` / `any-pointer` aggregate input
  devices), the `<img sizes>` source-size-list parser, and the § 4.8.4.8
  density-based source selection (incl. the `<picture>`/`<source media>`
  art-direction walk) and now backs the Page-projected `<img>.currentSrc` and
  `matchMedia()` eval seams for plain `srcset`/`sizes` images and
  MediaQueryList smoke checks. The
  value-resolution primitives `calc` (CSS Values 4 § 10 `calc()`/`min()`/
  `max()`/`clamp()` with full § 10.7 dimension type-checking) and `easing`
  (CSS Easing 1 `cubic-bezier`/`steps`/`linear` timing functions) cover the
  cascade's `calc()` reduction and the transition/animation driver surface.
  The structured-clone + MessagePort family (`structured_clone`,
  `message_port`) models the HTML § 2.7.5 serialisation algorithm +
  § 9.5.2 entangled port pair `postMessage()` / `new MessageChannel()` /
  worker messaging reduce to, with the transfer-list validation
  (duplicate/ unreachable/detached rejection) and the `SharedArrayBuffer`
  cross-origin-isolation gate, now exposed through `structuredClone()` eval
  smoke checks for primitives, arrays/objects, Date, Map, Set, and Error shape;
  runtime `MessageChannel` and `BroadcastChannel` smoke now dispatch through the
  same generated WebIDL/EventTarget host layer. Browser-platform probes now also
  cover secure `crypto.getRandomValues()` / `randomUUID()`, async Clipboard text
  and `ClipboardItem` shape, first-callback `IntersectionObserver` /
  `ResizeObserver` geometry, and fail-closed `WebSocket` close diagnostics.
  The Range/Selection family (`range`)
  models the DOM § 5.2 boundary-point pair + § 5.4 direction-aware
  selection (`add_range`/`collapse_to`/`extend_to`, the forward/backward
  direction) the editing commands and user-selection reflection reduce to,
  now projected through `document.createRange()` eval checks, including Range
  rectangles, point queries, same-container clone/extract/delete/insert/
  surround operations, and a live single-range `getSelection()` with
  Page-owned element-boundary restore and `selectionchange`. Focus transitions
  now use the pinned `focusout` → `focusin` → `blur` → `focus` order with
  `relatedTarget`, and interactive form validation emits document-order
  `invalid` events before `SubmitEvent`.
  The session-history family (`history`) models the HTML § 7.1 entry-stack
  + the `history.pushState`/`replaceState`/`back`/`forward`/`go` surface +
  the `scrollRestoration` mode the `History` host hook + the navigation
  layer reduce to, now projected through read-only `history.length` / `state`
  / `scrollRestoration` eval smoke checks.   The MutationObserver family
  (`mutation_observer`)
  models the DOM § 4.3 mutation-queue + the § 4.3.1 match predicate
  (childList/attributes/characterData + the subtree/attributeFilter
  options) + the microtask-delivery batch the `MutationObserver` host
  hook reduces to, now projected through `MutationObserver` lifecycle eval
  smoke checks.   The traversal family (`traversal`) models the DOM § 6
  `TreeWalker` + `NodeIterator` filtered preorder traversal (`whatToShow`
  bitmask + the `FILTER_ACCEPT`/`REJECT`/`SKIP` distinction — REJECT skips a
  subtree for TreeWalker, REJECT == SKIP for NodeIterator) + the
  node-removal reference adjustment, over a `Tree` trait the host hook
  implements on the real DOM, now projected through Page-backed TreeWalker /
  NodeIterator eval smoke checks. The WHATWG URL parser (`whatwg_url`) models
  the URL Standard § 4 parse + serialize + relative-resolution + the
  § 4.5 origin tuple the `new URL()` host hook + the fetch / navigation /
  storage layers consult.
- **Phase 7 prep** — CSP enforcement at the script execution boundary
  (`vixen-engine::script`); `vixen-net::referrer_policy` (Fetch § 3.4/§ 4.3.7
  `Referrer-Policy` parsing + `Referer` resolution); `vixen-net::strict_transport_security`
  (RFC 6795 HSTS parsing + § 8.2 host match); `vixen-net::cors` (Fetch
  § 3.2.1 `Access-Control-*` response-header parsing + § 4.1.5 CORS check
  with credentials-mode tightening + § 4.1.6 CORS-filtered response with
  the `Set-Cookie`/`Set-Cookie2` forbidden headers);
  `vixen-net::mixed_content` (W3C Mixed Content L1 § 3 verdict —
  `NotMixed`/`Block`/`Upgrade` — the fetch layer consults at every
  subresource out of a secure context); and `vixen-net::sandboxing`
  (WHATWG HTML § 4.8.5 `<iframe sandbox>` flag parser + the
  `implies_unique_origin` / `is_dangerous_scripts_plus_same_origin`
  predicates the script/navigation/storage layers consult when loading
  framed content); `vixen-net::sec_fetch` (Fetch § 3.1 `Sec-Fetch-*`
  request-metadata parsing + the § 3.2.4 site classifier); and
  `vixen-net::permissions_policy` (Permissions Policy 1 § 3.3
  `Permissions-Policy` header + `<iframe allow>` parser + the § 4
  per-feature allowlist evaluation) — ready for the network layer to
  consult at every fetch. The cross-origin-isolation gate:
  `vixen-net::coop` (HTML § 7.8 `Cross-Origin-Opener-Policy` parser +
  the opener-isolation predicate) + `vixen-net::coep` (Fetch § 3.2
  `Cross-Origin-Embedder-Policy` parser + the combined
  `is_cross_origin_isolated` gate the `performance.now()` coarsening and
  `SharedArrayBuffer` exposure consult).   The SRI + nosniff response-header
  family: `vixen-net::integrity` (W3C SRI `<script integrity>`/`<link
  integrity>` metadata parse + the constant-time hash verify, SHA-2 family
  only, any-match-passes) + `vixen-net::nosniff` (Fetch § 2
  `X-Content-Type-Options: nosniff` enforcement — the script/style MIME
  block) — ready for the fetch layer to consult at every subresource fetch.
  The CORP family: `vixen-net::corp` (Fetch § 4.5.3
  `Cross-Origin-Resource-Policy` parse + the combined COEP + CORP gate —
  `require-corp` cross-origin no-CORP block, `credentialless`
  cross-origin no-credentials allow, CORS the alternative opt-in) — ready
  for the fetch layer to consult before applying a no-cors subresource
  into a COEP-hardened document. The Trusted Types family:
  `vixen-net::trusted_types` (W3C Trusted Types `trusted-types` +
  `require-trusted-types-for` CSP directive parse + the
  `createPolicy(name)` gate + the injection-sink decision — a Trusted\*
  value ⇒ Allow, a string at a TT-requiring sink ⇒ `default`-policy or
  Block) — ready for the DOM injection-sink host hooks to consult before
  accepting a string.
- **Phase 8 (partial)** — the CDP WebSocket server (`vixen-headless::cdp`)
  covers the growing Playwright-facing surface: browser/target attach, page
  navigation/load/history/lifecycle events, resource tree/content snapshots,
  runtime evaluation/object properties and `Runtime.awaitPromise`,
  console/dialog/binding notifications, DOM query/resolve/geometry plus
  attribute and `outerHTML` edit/read methods, network events and Playwright
  network toggles including CDP cache-disable and extra headers for runtime `fetch()`,
  browser-shaped performance/security probes, screenshots,
  viewport/media emulation, basic mouse/keyboard input, browser-context
  permission overrides, bounded Chromium JSON tracing through `IO` streams,
  same-connection stop races for pending navigate/reload requests, and stable
  machine-readable protocol errors.
- **Flutter renderer/shell target** — ADR-022 commits one Flutter web renderer,
  chrome, and chrome-less automation host to Linux, macOS, Windows, Android, and
  the Apple Silicon iOS Simulator over BrowserCore. The exported C/Dart ABI and
  Linux shell/input/Semantics baseline exist; revision/mutation/commit/query
  protocols, Flutter formatting/Canvas paint, automation-host cutover, aggressive
  native-renderer deletion, and broader platform evidence remain open.

Future delivery order lives in [`docs/ROADMAP.md`](docs/ROADMAP.md); `PLAN.md`
retains the historical phase runbook.

---

## Setup

Workspace setup is split deliberately:

- [mise](https://mise.jdx.dev) pins tool versions and exports the workspace
  environment (`CARGO_HOME`, `PATH`, Rust toolchain selection, `hk`).
- [`just`](justfile) owns project actions. Prefer a recipe over spelling out raw
  `cargo ...` commands in docs, CI, or local scripts.
- [hk](https://hk.jdx.dev/) owns git lifecycle enforcement: quick pre-commit,
  long pre-push.

```sh
mise trust
mise bootstrap --yes     # pinned tools + optional Cargo tools + `just setup`
eval "$(mise activate bash)"
just hooks-install       # installs hk hooks through mise
just check               # alias: check-all-host
just test                # alias: test-host
just smoke               # fmt-check + clippy + check + tests
```

Headless runs use isolated temporary profiles by default. Pass
`--profile-dir <DIR>` to persist BrowserCore state in `<DIR>/profile.redb`,
including for `--cdp`:

```sh
cargo run -p vixen-headless -- --url https://example.com --profile-dir .tmp/vixen-profile --eval 'document.title'
```

Common recipes:

| Recipe | Use |
|--------|-----|
| `just setup` | Nightly for fuzzing, optional Cargo tools, then `check-all-host` |
| `just hooks-install` | Install/update hk git hooks via `hk install --mise` |
| `just check` / `just check-all-host` | Type-check the host-runnable workspace |
| `just test` / `just test-host` | Run host-runnable tests |
| `just smoke` / `just gate-smoke` | Reviewer baseline used by pre-push |
| `just gate-push` | Long pre-push gate invoked by hk |
| `just webidl` / `just gate-webidl` | Generated WebIDL/runtime host seam coverage |
| `just audit` | `cargo audit` + `cargo deny check` |
| `just baseline-headless` / `just baseline-headless-json` | Measure the hermetic local headless scenario suite |
| `just baseline-profile-growth` | Measure temporary profile growth and storage persistence across reopen |
| `just size-headless` | Report structured headless artifact size |
| `just flutter-size-prefetch` | Network-capable staging for pinned Linux Flutter size inputs; not evidence |
| `just size-flutter-linux` / `just size-flutter-linux-json` | Release/AOT hello-Flutter versus Flutter+Vixen raw-bundle comparison |
| `just baseline-beta` | Run the local headless, profile-growth, and headless-size measurement batch |
| `just flutter-builder-update` | Pull the pinned GNOME 50 local release-builder image |
| `just linux-release-prefetch` | Stage locked release inputs and the pinned rusty_v8 archive |
| `just linux-release-smoke` | Build, archive, extract, and Impeller-smoke the official Linux release |

These commands complete the local latency, Linux process-memory, profile-growth,
headless-path, and artifact-size measurement foundation. They are measurement-
only: real external-site coverage, the GUI/FlatPark host matrix, frame time, JS
heap, and transfer throughput remain future baselines. See
[`docs/BASELINES.md`](docs/BASELINES.md).

The Flutter release and size recipes use a controlled checked-in hello
application, the GNOME 50 builder image, the mise-pinned Rust/Flutter
toolchains, locked dependencies, and a separately staged pinned rusty_v8
archive. The first clean measurement-only x86_64 reference is recorded in
[`docs/BASELINES.md`](docs/BASELINES.md).

`mise bootstrap` and recipes run from a mise-active shell use
`CARGO_HOME=<workspace>/.cargo`, so the Cargo registry cache and installed dev
tooling stay inside the workspace (see
[`docs/guidance/cargo-home.md`](docs/guidance/cargo-home.md)).

**The GNOME 50 SDK is not installed on the host.** Local Linux release builds
run in the pinned builder image:

```sh
just flutter-builder-update
just linux-release-prefetch
just linux-release-smoke
```

The result is a deterministic `vixen-linux-x86_64.tar.gz` GitHub Release asset.
FlatPark pins and repackages that unchanged upstream archive, signs the Flatpak,
and hosts the update repository. Vixen does not maintain a parallel OSTree
repository. Flutter's Linux embedder uses GTK, so this removes packaged
application-owned GTK widgets without promising a GTK-free runtime. The archive
remains reproducible engineering evidence; FlatPark
submission and publishing are not current priorities and resume only after the
basic-browser gate in `docs/ROADMAP.md` passes.

See [`docs/guidance/flatpark-release.md`](docs/guidance/flatpark-release.md)
for the full workflow. Headless/CI hosts that only build `vixen-api` /
`vixen-net` / `vixen-store` need neither the GNOME SDK nor the container.
`mise install` now provisions the pinned Flutter beta as a project dependency,
but `just check` does not execute it.

See [`.mise.toml`](.mise.toml) and the
[mise bootstrap guide](https://mise.jdx.dev/bootstrap.html). The library
MSRV is 1.88 (let-chains); the developer toolchain is pinned in
[`.mise.toml`](.mise.toml).

---

## Repository map

| Path                                        | Purpose                                                       |
|---------------------------------------------|---------------------------------------------------------------|
| [`docs/SPEC.md`](docs/SPEC.md)              | **What Vixen must do.** Capabilities, CLI, behaviour contracts. |
| [`docs/PROJECT_DIRECTION.md`](docs/PROJECT_DIRECTION.md) | **What Vixen is optimizing for.** North star, users, priorities, alpha definition. |
| [`docs/ROADMAP.md`](docs/ROADMAP.md)        | **What comes next.** Alpha convergence through the full replacement horizon. |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | **How Vixen is structured.** Crates, data flow, trust boundaries, trait APIs. |
| [`docs/FLUTTER_SHELL.md`](docs/FLUTTER_SHELL.md) | **How the GUI migrates.** Five-platform bridge, rendering, accessibility, packaging, size, and gates. |
| [`docs/DECISIONS.md`](docs/DECISIONS.md)    | **Why these choices.** ADR-style records for the major decisions. |
| [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md) | **How to move fast safely.** Alpha/dev workflow, gate tiers, maintainability budget. |
| [`docs/RUNTIME_WEB_PLATFORM.md`](docs/RUNTIME_WEB_PLATFORM.md) | **How WebIDL/DOM/Web APIs are exposed.** JS bootstrap vs Rust op/resource strategy. |
| [`docs/AUTONOMOUS_WORK.md`](docs/AUTONOMOUS_WORK.md) | **How agents/maintainers can proceed.** Commit/push policy, hk gates, report format. |
| [`docs/PLAN.md`](docs/PLAN.md)              | **Historical record.** Original Linux/Relm4 phased runbook. |
| [`docs/REFERENCES.md`](docs/REFERENCES.md)  | **Where to look for truth.** Pinned reference trees + how to consult each. |
| [`docs/ACCEPTANCE.md`](docs/ACCEPTANCE.md)  | **When it's done.** Release gates per capability. |
| [`docs/BASELINES.md`](docs/BASELINES.md)    | **How it is measured.** Local latency, memory, profile-growth, and artifact reports. |
| [`docs/guidance/`](docs/guidance)           | **How to do specific tasks.** Tooling, release archives, and FlatPark packaging. |
| `LICENSE`                                   | Apache 2.0 (lands at Phase 0). |

---

## Reading order

If executing the build:

1. `docs/PROJECT_DIRECTION.md` — the north star
2. `docs/ROADMAP.md` — the next delivery order
3. `docs/ARCHITECTURE.md` — the shape
4. `docs/FLUTTER_SHELL.md` — Flutter GUI contract and platform gates
5. `docs/RUNTIME_WEB_PLATFORM.md` — runtime host strategy
6. `docs/DEVELOPMENT.md` and `docs/AUTONOMOUS_WORK.md` — workflow and gates
7. `docs/DECISIONS.md` — confirm the choices
8. `docs/SPEC.md`, `docs/PLAN.md`, `docs/REFERENCES.md`, `docs/ACCEPTANCE.md`
   — contracts, historical runbook, references, release checks

If evaluating the project: read `docs/SPEC.md` and
`docs/DECISIONS.md`, then sample `docs/PLAN.md`.

When a doc and a decision record disagree, the **decision record wins**.
Update both when resolving.

---

## Working assumptions

- Primary GUI targets: **Linux, macOS, Windows, Android, and Apple Silicon iOS
  Simulator** through the pinned Flutter 3.47.0-0.1.pre beta. Each remains
  evidence-gated; the Linux RGBA renderer/shell and deterministic release path
  are implemented, while ADR-022's Flutter web renderer and non-Linux runners
  remain open. Validation tracks each target's latest stable major OS release at
  the release cutoff; older releases are best-effort unless explicitly tested.
- Linux publishes an official x86_64 release archive that FlatPark repackages
  unchanged as a signed convenience Flatpak after the basic-browser gate.
  Registry publishing is deferred meanwhile. Flutter is the sole rendered
  frontend target; its Linux embedder may still depend on GTK at runtime.
- The current Rust release profile starts with `strip = true`, `lto = "thin"`,
  `codegen-units = 1`, and `panic = "abort"`; Flutter release/AOT and native
  packaging are measured per platform before any stronger optimization claim.
- App IDs: `dev.adonm.vixen` (production), `dev.adonm.vixen.Devel` (devel).

## License

Apache 2.0 — see [`LICENSE`](LICENSE).
