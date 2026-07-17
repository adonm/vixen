# Roadmap

This roadmap moves Vixen from its original WebRender/RGBA prototype to the full
project goal: a credible Firefox replacement with one Flutter-hosted web renderer
and browser shell on Linux, macOS, Windows, Android, and the Apple Silicon iOS
Simulator, plus first-class rendered CLI/CDP/WPT automation through the same
Flutter renderer.

**Linux is the first renderer, GUI, automation, integration, packaging, and
release target.** The other platforms remain committed, but they reuse the
BrowserCore/renderer contract proven on Linux rather than delaying it.

Product direction lives in [`PROJECT_DIRECTION.md`](PROJECT_DIRECTION.md), the
current architecture in [`ARCHITECTURE.md`](ARCHITECTURE.md), accepted decisions
in [`DECISIONS.md`](DECISIONS.md), measured support in
[`COMPAT.md`](COMPAT.md), and executable commands in
[`MILESTONES.md`](MILESTONES.md). [`PLAN.md`](PLAN.md) is historical only.

## Destination and release ladder

The stages are capability gates, not dates:

1. **Renderer transition — one cross-platform visual truth.** BrowserCore emits
   bounded render mutations; Flutter commits layout, scene, geometry, hit testing,
   text queries, scroll state, and semantic bounds. Rendered GUI/headless/CDP/WPT
   use it. WebRender/EGL/RGBA and superseded Rust layout/paint are deleted.
2. **Alpha — one browser architecture.** BrowserCore owns one profile/context/
   document/runtime lifecycle; the Flutter renderer owns no browser truth; live
   script mutation, inspection, input, and pixels converge on exact render
   commits.
3. **Beta — a measured useful browser.** A controlled real-site corridor works
   in the Linux GUI and chrome-less renderer with representative layout,
   interaction, persistence, downloads, diagnostics, accessibility, and host
   integration.
4. **v1.0 — an honest daily-driver minimum.** The published corridor is reliable
   enough for focused daily use, release/security operations are credible, and
   every supported capability and platform has reproducible evidence.
5. **Replacement horizon — broad modern-browser capability.** Media, offline
   applications, richer graphics/communications, extensions, accessibility, and
   stronger isolation widen ordinary use until “Firefox replacement” is an
   honest default description.

No stage implies global Firefox or WPT parity. Every compatibility claim names the
profile, platform, renderer host, command, and measured result.

## Current baseline and transition debt

As of 2026-07-16 the repository has:

- one eight-crate Rust workspace with hk/`just` gates, stable diagnostics, fuzz
  targets, a fixture/WPT harness, and a committed **270 fixture / 2,027 check**
  100% baseline;
- dependency-free renderer protocol v1 DTOs and reference validation in
  `vixen-api` for exact revisions, bounded source snapshots/mutations/resync,
  atomic commit/presented state, geometry/text/scroll queries, displayed-commit
  input, semantic actions, replay rejection, and explicit handle retirement;
- one `BrowserCore` owner for profile services, contexts, navigation generations,
  DOM/Page state, V8 runtimes, history, input intent, inspection, and ordered
  events used by Flutter, native text utilities, CDP, and WPT;
- `html5ever`, Stylo selector/cascade integration, `deno_core`/V8, shared
  network/security policy, and bounded redb profile tables;
- generation-cancellable main-document, external-script, stylesheet, and bounded
  PNG loading plus deadline-bounded V8/runtime-fetch cancellation;
- a useful CDP/Playwright slice and a Linux Flutter shell with native Wayland
  chrome, input/IME, Semantics, scrolling/find/zoom, recovery, and deterministic
  release/Cage evidence; and
- one Flutter renderer: R7 deleted the Rust layout/paint island, WebRender/gleam,
  both EGL owners, native visual headless, RGBA frame transport, Linux texture
  presentation, raw coordinate input, and their obsolete tests/gates.

## Architecture rules for every stage

1. **BrowserCore owns browser truth.** Profile → browser → context → document is
   authoritative for navigation, DOM, V8, Stylo computed styles, network/security,
   persistence, history, resource acceptance, events, and accessibility meaning.
2. **Flutter owns rendered truth.** The renderer owns CSS box/anonymous trees,
   formatting/fragmentation, Paragraph/image measurement, paint order, clips,
   transforms, mechanical scroll geometry, hit testing, semantic bounds, scenes,
   and capture. Public Flutter scene APIs sit over required Impeller; a Skia
   fallback does not satisfy a Vixen rendered-platform gate.
3. **Mutations are not a second DOM.** Dart receives bounded immutable
   `RenderMutationBatch` data with stable ids and exact compound revisions. It
   cannot mutate navigation, DOM, policy, or durable state.
4. **Commits are atomic.** One `RenderCommit` pairs scene-ready layout, geometry,
   an opaque Flutter-side hit-test handle, text-query state, scroll state, and
   semantic bounds. Visible input and native accessibility name the displayed
   commit.
5. **Basic geometry comes back to BrowserCore.** Flutter computes it;
   BrowserCore validates and queries the immutable index for synchronous
   DOM/CSSOM/CDP operations. Paragraph-specific queries remain bounded renderer
   services.
6. **Synchronous layout is explicit.** Same-task mutation followed by geometry
   uses deadlock-safe, cancellable, deadline-bounded `EnsureLayout`; stale
   approximations cannot become the permanent behavior.
7. **One renderer after cutover.** Experimental Flutter rendering is test-only
   until parity. Production cuts over once, then deletes WebRender/EGL/RGBA and
   obsolete Rust renderer ownership. No fallback renderer survives.
8. **Policy precedes renderer exposure.** URL/CSP/CORS/mixed-content/integrity,
   response type, body/decode limits, and cache policy run before Flutter receives
   image/font/resource data.
9. **Every content-controlled boundary is bounded.** Mutations, snapshots,
   strings, nodes/depth, resources, image/font bytes, fragments, queries, commits,
   queues, V8 work, protocol handles, and diagnostics have explicit limits.
10. **Generations reject late work.** Navigation, runtime, resource, renderer,
    query, scroll, input, and semantic results cannot affect a replacement
    document or commit.
11. **Flutter supplies primitives, not CSS semantics.** Flutter Flex/widgets and
    packages are not accepted as CSS implementations. Vixen formatting code is
    WPT-driven and uses `dart:ui` Paragraph/Canvas/scene primitives.
12. **Linux proves the contract first.** Framework support is not Vixen support;
    each platform and ABI earns native renderer, input, accessibility, lifecycle,
    host-service, package, size, and performance evidence under ADR-019.

## Renderer transition — execute before feature breadth

Keep **one active renderer slice**. Only an independently critical BrowserCore
security/lifecycle fix may run beside it. Do not widen native interaction,
WebRender, Rust layout, text shaping, paint effects, packaging registries, or new
Web API shape while it would create porting work for the renderer transition.

### R0. Freeze and name ownership — landed with ADR-022

- Consolidate current decisions and remove superseded renderer/shell/layout ADRs.
- Mark WebRender, EGL, RGBA frame transport, native visual headless, and Rust
  layout/paint breadth as transitional.
- Make the mutation/commit/query model and aggressive deletion policy the sole
  current direction.

**Proof:** no current-direction document names WebRender or Rust layout as the
target; `git diff --check`, docs build, and architecture references are clean.

### R1. Renderer protocol types — landed

Dependency-free, versioned, bounded DTOs in `vixen-api` now provide:

- compound `RenderRevision` with context, document, source/style, viewport, and
  resource generations;
- incremental `RenderMutationBatch`, exact `base_revision`, bounded full snapshot,
  and resync request;
- stable render node/resource/fragment/commit ids;
- atomic `RenderCommit` and separate `Presented` acknowledgement;
- immutable geometry indices, opaque Flutter-side hit-test handles,
  text/caret/range query DTOs, scroll snapshot/commands, semantic bounds, and
  truncation/limit diagnostics;
- input targets carrying displayed commit, revision, node/fragment, and finite
  coordinates; and
- semantic-action targets carrying document, displayed commit, semantic node,
  and advertised action generation.

Define limits before payload details. Prefer plain arrays/records and explicit
release over a generic scene framework.

**Proof:** `just test-api` covers malformed/round-tripped ids, exact monotonic
source and viewport generations, non-finite geometry, oversized/deep snapshots,
unknown resources, atomic invalid-batch rejection, missed bases, deterministic
full-resync recovery, equal-revision idempotence, stale/late commits, separate
presentation, query correlation, bounded UTF-16 ranges, truncation policy,
scroll-command replay, forged/stale/replayed semantic actions, and explicit
opaque-handle retirement. Strict API Clippy and the all-target workspace check
pass. This is model-only evidence: no C ABI, Dart bridge, broker, or production
renderer changed.

### R2. Native/Dart bridge and broker — landed

- Carry R1 DTOs through the safe Rust controller, C ABI, handwritten Dart models,
  and fake controller.
- Add a dedicated renderer request/response channel that the Flutter UI/renderer
  isolate can service while the BrowserCore command worker or V8 evaluation is
  waiting.
- Keep ordinary mutation/commit flow asynchronous; reserve the broker for
  `EnsureLayout` and bounded renderer queries.
- Prohibit renderer-to-BrowserCore re-entry during layout and release every
  retained payload/resource explicitly.

**Proof at R2:** ABI/header/layout checks, Dart/Rust golden round trips,
malformed and stale wire tests, cancellation/timeout tests, queue bounds,
worker-blocked broker service, shutdown, and full resync. Production still
displayed the old frame at that checkpoint.

**Implemented evidence:** the bounded `RenderBroker` is independent of the
serialized BrowserCore controller lock. Ordinary snapshots, every mutation
variant, and handle releases use a bounded asynchronous update queue; commits,
presentation, and resync use a separately bounded submission queue. Only
`EnsureLayout`, hit tests, and text queries use correlated request/response.
C `renderer_poll`/`renderer_respond`/`renderer_submit`/`renderer_shutdown`
entrypoints and handwritten Dart records are strict, versioned, and retain C
output only through the existing tokenized release contract. Total in-flight
requests remain capped after polling, update source is capped at 512 KiB before
JSON encoding, incoming messages remain capped at 64 KiB, and encoded output at
1 MiB. Timeout, late response, exact identity/kind correlation, cancellation,
queue saturation, shutdown wakeup, malformed wire, double release,
worker-blocked progress, native header, and Rust/Dart golden tests are checked
in. A small Dart service drives the formatter from the same transport; the
scripted fake enforces the same queue/payload bounds. Normal browsing still used
the old frame at R2.

### R3. First Flutter-rendered document — landed test-only

Use one controlled fixture containing:

- block and inline boxes with margin/padding/background;
- mixed styled text requiring Paragraph measurement and wrapping;
- one BrowserCore-policy-accepted PNG image; and
- semantic heading/link/text descriptors.

Build the smallest Vixen Dart formatter over `dart:ui`, not a widget-per-DOM
adapter. Construct a Flutter scene and return one atomic commit with geometry,
an opaque Flutter-side hit-test handle, text ranges, scroll limits, and semantic
bounds.

**Proof:** exact-generation Impeller-backed Canvas pixels/visual hash, Paragraph
line/range checks, image pixels, geometry index, renderer hit tests, Semantics
bounds, scene capture, mutation update, stale rejection, and full resync. This
path remains test-only.

**Implemented evidence:** `just test-flutter-formatter-impeller` drives one
immutable snapshot through a small flow formatter over `dart:ui` Paragraph,
Canvas/Picture, encoded PNG decode, Scene capture, geometry, reverse-paint-order
hit testing, UTF-16 range/point queries, scroll limits, semantic bounds, mutation,
presentation, explicit idempotent handle release, stale/equal-snapshot rejection,
deterministic resync, and reset. Candidate source/scene state publishes only
after successful formatting and bounded commit submission; failed submissions or
superseded asynchronous builds retain the previous revision and dispose their
Paragraph/image/Picture resources. Mixed
text runs have run/line fragments, padded boxes retain distinct content bounds,
and wrapped semantic text retains all Paragraph rectangles.
Software and Impeller-requested captures have separate exact raw-RGBA hashes.
The formatter remained test-only through R3; the bounded production vertical
below now reuses it without claiming the rest of R4.

### R4. One interactive commit vertical — landed

Route one controlled Linux document through the new renderer for:

- displayed-commit pointer targeting and DOM click;
- wheel/key/script scroll intent, `preventDefault()`, renderer clamp, returned
  scroll commit, and DOM `scroll` effect;
- find match and caret/range geometry from Paragraph;
- page zoom/viewport change as a new revision;
- BrowserCore semantic meaning combined with renderer bounds; and
- lifecycle hide/resume with stale scene/commit suppression.

**Proof:** widget/core/ABI tests plus a Cage interaction smoke. Every assertion
names one commit id. The old texture path remains production-only comparison and
is not widened.

**Implemented evidence:** the native Linux shell now requests
one bounded BrowserCore projection for the selected document, carries it over the
dedicated renderer update queue, formats it with the R3 service, validates the
returned commit in Rust, and paints the accepted `RenderCommitPainter` view.
At R4 completion the source was deliberately a basic title plus at most 64
non-hidden semantic elements (or bounded body-text fallback), not a claim of
computed-style or general CSS rendering. The R5 source checkpoint below has now
replaced that temporary projection.

Presentation is acknowledged only from a Flutter post-frame callback. Pointer
input uses the formatter's displayed commit, opaque hit-test handle, exact
revision, fragment, viewport point, and local point; Rust validates all of them,
resolves text hits to the nearest BrowserCore semantic element, and only then
dispatches the DOM event. Snapshot replacement, submissions, releases, and
queues stay bounded; consuming a submission and publishing all resulting handle
releases is atomic. At R4 the WebRender/RGBA texture was still the explicit
fallback; R7 deleted it.

All six R4 behavior slices now cross the production seam:

- renderer-targeted down/up input synthesizes a real DOM `click` on the exact
  displayed commit in the native ABI smoke;
- find results, highlight boxes, and endpoint carets come from commit-bound
  Paragraph UTF-16 geometry rather than the transitional layout;
- page zoom and physical viewport changes produce, accept, and present newer
  revisions/commit ids while retiring old handles;
- BrowserCore semantic descriptors use Flutter-computed bounds, and advertised
  tap/focus/value/range actions are suppressed unless the same commit and
  accessibility generation are still displayed; and
- lifecycle generations clear hidden presentation, reject late hidden work, and
  require a newer commit before resume while bounding acknowledgement retries.
- BrowserCore snapshots carry the accepted root offset and extent only after its
  cancelable wheel/key/script policy runs. The formatter independently clamps
  that intent, translates pixels, geometry, Paragraph queries, hit testing, and
  semantic bounds together, and returns the offset in a newer atomic commit.
  Canceled wheel input leaves the offset unchanged; the native ABI smoke covers
  script, wheel cancellation/default, and key commits while the release-process
  Cage interaction smoke correlates DOM effects with exact presented commit ids.
  `mousedown` no longer publishes a replacement source before its matching
  `mouseup`, and input is suppressed during source/commit transition windows, so
  strict stale validation remains enabled without breaking click synthesis.

Computed styled nodes/resources, nested Flutter scroll nodes, and DOM/script
mutation batches remain broader renderer-transition work; deletion of the
fallback remains R7.

### R5. Chrome-less Flutter automation host

- Add a minimal Flutter entrypoint that opens an exact viewport without browser
  chrome, drives the same BrowserCore/renderer bridge, and captures an exact
  presented commit.
- Run it under Cage/wlroots headless Wayland on Linux.
- Move visual hashes, layout-box evidence, screenshots, and CDP screenshot/input
  workflows to it in coherent groups. Keep text-only native tests only where they
  require no pixels or geometry.
- Retire `display-list-contains` and migrate its three assertions in two fixtures
  to commit-bound layout/pixel evidence before claiming the full manifest; do
  not recreate a Flutter display-list dump compatibility API.
- Preserve independent contexts/targets and bounded startup/shutdown behavior.

**Proof:** fixture manifest through the Flutter host, external Playwright smoke,
multiple target viewports, input, before/after script capture, renderer loss, and
no compositor/chrome pixels in page screenshots.

**First implemented checkpoint:** the release bundle now runtime-selects a
page-only Dart host with an undecorated Linux runner window. One strict
`--vixen-automation` invocation requires an absolute file/HTTP(S) URL, a viewport
within the existing 4,096-pixel/64-MiB bounds, and an absolute bounded `.png`
output path. It bypasses profile tab restore/save and browser/frame fallback
capture, paints the accepted formatter view without browser widgets, then only
after a Flutter frame acknowledges and captures the exact still-presented commit
through `Scene.toImage`. Startup/capture is bounded to 60 seconds; successful
work closes the sole BrowserCore, while shutdown gets a five-second grace before
the process fails closed. `just linux-automation-smoke` launches that same
release/AOT bundle under Cage twice at 320×240 and 480×300 with fresh profiles;
it checks Impeller and exact commit diagnostics, strict PNG structure/dimensions,
RGBA scene pixels, and pinned full-scene hashes. Because capture serializes the
formatter scene rather than the Flutter or compositor surface, browser,
runner, and compositor chrome cannot enter the PNG. Dart tests cover
configuration rejection, legacy-capture suppression, exact presentation identity,
PNG encoding, and output bounds. At that checkpoint this did not yet satisfy
full R5: fixture
manifest, layout evidence, CDP/Playwright screenshot and input routing,
independent simultaneous targets, before/after mutation capture, and renderer
loss remain to migrate.

**Renderer-source checkpoint:** BrowserCore now publishes the bounded renderable
DOM tree rather than synthetic title/semantic wrappers. Element ids are the
stable BrowserCore node ids; renderer-only text ids occupy a disjoint range;
parent/sibling/depth topology, viewport-resolved Stylo properties, accepted PNG
resources, semantic descriptors, and root scroll intent travel in one validated
`FullRenderSnapshot`. Metadata/script/style subtrees are counted for stable DOM
ids but excluded from renderer payload and paint. The Dart formatter consumes
authored dimensions, per-side margin/padding, background colors, visibility,
image sizing, and page zoom while preserving exact commit input validation. The
release Cage hashes now cover actual `fixtures/dom/basic.html` DOM text rather
than the former synthetic document card. This establishes the source needed by
the remaining manifest/CDP migration; it does not by itself satisfy the proof
paragraph above.

**Shared-core CDP checkpoint:** CDP protocol ownership now lives in the reusable
`vixen-cdp` adapter. BrowserCore can create independent bounded event
subscriptions without cloning lifecycle ownership, so the long-lived release
Flutter host runs the listener against its sole BrowserCore. Rendered CDP
screenshots publish a target-specific full snapshot, wait for its exact Flutter
commit and `Presented` acknowledgement, then return bounded raw PNG bytes from
the displayed scene. `DOM.getContentQuads`/`DOM.getBoxModel` and mouse input use
Flutter commit geometry/hit testing in this mode. At that checkpoint native CDP
retained a comparison backend; R7 later deleted it. `just
flutter-cdp-playwright-smoke` proves
320×240 and 480×300 targets alive together, target isolation, Flutter-routed
input, before/after mutation pixels, target switching, no chrome pixels, and a
forced renderer reset followed by byte-identical full-resync capture. The old
`display-list-contains` manifest check is removed; its three assertions now use
computed-style together with existing layout/pixel evidence. At that checkpoint,
full fixture-manifest routing was the last R5 migration item.

**R5 complete:** the Dart formatter now implements the bounded fixture slice of
content-box/border-box block and inline flow, relative/absolute positioning,
row/column/reverse flex sizing, fixed/fractional/minmax grid tracks, gaps,
deterministic text line geometry, backgrounds, borders, and images. `just
flutter-fixture-manifest` starts one release/AOT Flutter host under Cage and runs
all **270 fixtures / 2,027 checks** in manifest order. Every fixture uses a fresh
target in the host's sole BrowserCore, so script/style mutations and rendered
assertions share one document/runtime lifecycle. The 1,868 native-safe
document/runtime checks use typed BrowserCore inspection; 19 `flutter-js-eval`,
104 `layout-box`, 25 `visual-hash`, and 11 `ref-equivalent` checks use exact
presented Flutter commits. Reference
checks compare direct RGBA scene pixels, visual baselines now name Flutter
scenes, and the native runner is text/runtime-only. `just gate-r5` composes this
manifest with the one-shot and external Playwright gates. R6 synchronous layout
and R7 cutover/deletion are now also complete; R8 stabilization is next.

### R6. Synchronous layout and recovery gate — landed

Implement:

- DOM mutation → Stylo flush → mutation batch → `EnsureLayout` → matching commit
  → synchronous geometry answer;
- repeated/batched geometry reads without repeated layout;
- cancellation by navigate/stop/close/shutdown;
- renderer timeout, crash/loss, malformed commit, resource eviction, missed
  revision, and bounded full-resync recovery; and
- no BrowserCore mutex held during wait, no Dart re-entry, no late commit, and no
  poisoned next request.

**Proof:** same-task `style`/DOM mutation plus `getBoundingClientRect()`, Range and
caret queries, forced races/timeouts, isolate reuse, and GUI/CDP agreement on the
same commit.

**Implemented evidence:** BrowserCore page realms now share the one authoritative
`Page` with a synchronous geometry host. A geometry read drains the task's
bounded DOM mutation sink, refreshes the Page cascade, diffs the previous exact
renderer source into a `RenderMutationBatch` (or publishes a full snapshot for
first load/resync), and waits on the dedicated broker without holding the C
controller or renderer-state mutex. The response is accepted only after its
matching asynchronous commit submission validates against the same replica.
Repeated element reads reuse that commit; Range boxes and collapsed caret
rectangles use commit-bound batched Paragraph text queries.

Navigation, stop, close, shutdown, and the V8 deadline carry explicit renderer
cancellation while the normal GUI keeps a separate bounded UI-isolate broker
pump alive even when its browser command worker is blocked. Late replies are
unknown/inert. One bounded retry sends a full snapshot after renderer resync,
timeout, malformed commit, or missed state; a non-finite malformed submission is
consumed and retired without poisoning the next request. Focused tests prove
same-task style mutation plus two reused element reads, Range and caret geometry,
exact source batches, renderer-reset full resync, navigation/stop races, late
reply rejection, malformed-commit recovery, and same-isolate reuse. `just
test-r6` runs the focused Rust/Dart gate; `just gate-r6` composes it with all R5
rendered fixture/CDP/Cage evidence.

### R7. Production cutover and aggressive deletion

R7 cut over after R3–R6 were green and removed in one reviewed migration series:

- `webrender`, `gleam`, `GlContext`, native renderer integration, and WebRender image upload;
- native-headless and FFI frame EGL implementations;
- RGBA frame ABI/tokens/pools, Dart frame worker, Linux pixel-buffer texture
  plugin/presenter, and texture recovery tests;
- Rust display-list/paint modules and formatting/layout code not explicitly
  reused by the Dart formatter;
- obsolete visual/layout tests, gates, docs, dependencies, fixtures, and CLI
  flags rather than preserving compatibility shims; and
- duplicated scale, hit-test, scroll, text-metric, and semantic-bound projections.

Use source search and dependency gates to prove absence. Do not retain dead APIs
for hypothetical embedders.

**Proof:** one Flutter renderer in dependency/source scans; no WebRender/EGL/frame
transport; GUI and chrome-less host share mutation/commit code; all supported
layout/pixel/input/semantics/CDP evidence uses it.

**Landed:** production GUI and automation always paint Flutter commits. The
WebRender/gleam dependency graph, `GlContext`, both EGL implementations, native
visual headless, screenshot/incremental CLI flags, RGBA C/Dart transport, Linux
texture path, Rust layout/display-list/paint and paint-helper modules,
`PaintSnapshot`, Page hit testing/geometry/semantic bounds, raw coordinate-input
ABI, native rendered WPT/CDP checks, and obsolete Phase 4/5 gates are deleted.
`flutter-js-eval` makes renderer-dependent manifest checks explicit. `just
test-r7` proves source/dependency absence and both native/Flutter surfaces;
`just gate-r7` composes all R5/R6 rendered evidence.

### R8. Linux stabilization and rebaseline

- Reproduce the compatibility manifest and imported profiles through appropriate
  native or Flutter-hosted paths; update `COMPAT.md` only from output.
- Re-run Linux interaction, IME, AT-SPI, release archive, startup, memory, frame,
  screenshot latency, and profile-growth evidence.
- Rebaseline hello-Flutter versus Flutter+Vixen and attribute removed
  WebRender/EGL/frame code, new Dart formatter, and chrome-less-host costs.
- Fix renderer-transition regressions before broadening APIs or resuming FlatPark
  publication work.

**Compatibility reproduction checkpoint:** on clean revision `e224bf6`, `just
compat-report` reproduced all 270 fixtures and all 1,868 native-safe BrowserCore
checks at 100%. The post-R7/Yaru release/AOT Flutter host subsequently reproduced
the full 270 fixtures / 2,027 checks at 100%, including 19
`flutter-js-eval` checks plus 104 exact layout boxes, 25 visual hashes, and 11
exact-pixel references. Renderer evidence is kept separate from, not inferred
from, the native run.

The matching external Playwright/CDP rerun is also green: two target viewports,
Flutter-routed geometry/input, before/after mutation captures, target switching,
and forced renderer reset/full-resync all retained exact scene identity.

**Renderer/frame/GPU measurement checkpoint:**
`just baseline-flutter-linux` now measures the release/AOT CDP host from process
spawn through exact capture, then joins eight direct mutations and one mouse
release to exact presented Flutter commits and engine frame timings. Clean
five-run/one-warmup version-2 references contain 45 interaction frames each.
Mesa software records 15.402 ms median mutation → commit-frame, 26.364 ms mouse
release → commit-frame, and 2,587 µs exact-frame total span; the corresponding
AMD Ryzen 7 7700X integrated-GPU/radeonsi/Mesa 26.0.4 run records 14.527 ms,
25.269 ms, and 2,590 µs. Renderer-specific exact PNGs repeated in every sample
and all processes exited cleanly. Cage reported no refresh rate, and Flutter
raster finish is not compositor scanout. These are checked-in measurement-only
single-host observations, not budgets, animation stability, physical-input
latency, isolated Flutter/GPU attribution, or a supported GPU matrix.

**First size/release checkpoint:** clean, equally stripped Flutter 3.47 hello
and post-R7/Yaru Vixen release bundles now have a checked-in component report.
The 85,377,960-byte Vixen bundle is 131,560 bytes smaller than the historical
pre-R7 bundle despite adding Yaru assets/plugins; its aggregate native library
is 2,076,976 bytes smaller. The hello control also shrank, so the current
63,979,292-byte Vixen-minus-hello delta is larger and is not misreported as a
product regression. The same Vixen bundle produces a deterministic
31,913,890-byte archive; clean extraction and a bounded Cage launch reported
Impeller and presented an exact Flutter commit. These are unreproduced
measurements and one controlled launch, not budgets, sustained release evidence,
or FlatPark install evidence.

**Profile-growth checkpoint:** a clean five-repeated/five-unique-visit run kept
the opaque profile's logical size constant, added 8,192 allocated bytes across
repeated visits and zero across unique visits, then added 139,264 bytes for a
65,536-byte localStorage payload that a fresh process reopened successfully.
This is a checked-in single-host measurement, not a growth budget or broad
history/cache workload.

**Native interaction/accessibility checkpoint:** R8's final gate passed on
2026-07-17. An unchanged Fedora `ibus-mozc`/`mozc`
2.29.5111.102-16.fc43 pair ran from a workspace-local extraction under a private
IBus daemon; a user-namespace bind supplied its compiled `/usr/libexec` path
without changing host packages. The release/AOT Cage run observed real GTK
preedit start/update/end and commits in both the native input and
contenteditable controls. A narrowly scoped Linux-runner guard terminates
Flutter 3.47's recursive `Component.get_extents` walk at its non-component
`FlViewAccessible` root; descendant bounds remain Flutter-authored. The same run
then observed the editor as text/editable/visible/showing with positive bounds
`(8, 187, 40, 20)`, invoked Flutter's unchanged native `Focus` action, reached
DOM `focus=editor`, and advanced the same document from commit 18 to 20. The
complete interaction corridor continued through IME, wheel ownership and
cancellation, script/root scroll, navigation stop/recovery, keyboard input, and
clean app exit (`commits=3>31>34>40>45`). `just linux-at-spi-smoke` separately
passed the process-filtered name gate. This closes R8; it is one controlled
Linux/IBus/Mozc/AT-SPI proof, not an IME, assistive-technology, compositor, or
device matrix.

**Exit:** the controlled Linux corridor uses no transitional renderer component,
all renderer failure modes are bounded, and the next compatibility failure can be
reduced directly against the final architecture.

## Alpha — converge live browser state on render commits

R8 is complete. Continue shared-core convergence in this order without
reintroducing native renderer ownership or weakening the landed host gates.

### A1. Live document/runtime convergence

- Replace remaining Page/runtime compatibility snapshots with live
  Node/Element/Document, CSSOM, events, focus, selection, forms, history, and
  storage resources.
- Make every relevant mutation produce one render-source revision and invalidate
  accepted geometry explicitly.
- Execute parser classic/module scripts with document event-loop and microtask
  ordering; preserve realm teardown and same-origin frame boundaries.
- Delete plausible inert compatibility shims as real owners land.

**First A1 checkpoint:** `HTMLElement.dataset` is now one stable live
`DOMStringMap` per element instead of a frozen property projection. External
attribute changes reflect into the retained object; property assignment/deletion
uses the shared Rust name conversion and the normal DOM mutation path. Focused
runtime proof requires exactly one render-source generation per write and Stylo
attribute-selector recascade. The release/AOT Playwright smoke then performs one
dataset write, observes 140×32 geometry synchronously in that task, reads the
same attribute/node/geometry through CDP, and pins different before/after exact
Flutter PNGs. This is one live host-family vertical, not completion of A1.

**Second A1 checkpoint:** `Element.classList` now retains one live
`DOMTokenList` identity across external and list-driven `class` mutations rather
than discarding the wrapper after every attribute write. Focused runtime proof
retains the object through `setAttribute`, reflects current tokens, advances
exactly one renderer-source generation per write, and recascades `.wide` and
`.tall` selectors to 140×30. The release/AOT Playwright corridor retains the
same object through Flutter-routed input, observes `clicked` and 140px geometry
in the page task and CDP, and pins the resulting exact Flutter PNG to
`5633ca7a032c8c6a1582f5389b6b4a594b91d99e89784683fbf3679f18639f95` before
byte-identical target switching and renderer recovery. This converges one more
attribute-backed host object; other token lists, inline style, collections, and
attribute nodes remain separate work.

**Third A1 checkpoint:** `HTMLAnchorElement.relList` now retains one live
`DOMTokenList` across external and list-driven `rel` mutations. Focused runtime
proof retains identity through `setAttribute` and `add`, reflects ordered tokens,
advances exactly one renderer-source generation per write, and recascades
`[rel~="wide"]`/`[rel~="tall"]` selectors to 140×30. A hidden real anchor keeps
the prior release/AOT baseline, dataset, and classList hashes unchanged; its rel
mutation becomes visible at 120×32, agrees with CDP attributes/geometry, and
pins exact Flutter pixels to
`7ae6e6d8f650d733922b1af018dfdcac310bdcbb4f14537cdb20500c44da3c04` before
byte-identical target switching and renderer recovery. Sandbox tokens, inline
style, collections, and attribute nodes remain separate work.

**Fourth A1 checkpoint:** `HTMLIFrameElement.sandbox` now retains one live
`DOMTokenList` across external and list-driven `sandbox` mutations, completing
the three attribute-backed token-list identities currently hosted by the
runtime. Focused proof retains identity through `setAttribute` and `add`,
reflects valid ordered sandbox tokens, advances exactly one renderer-source
generation per write, and recascades token selectors to 140×30. A hidden real
iframe preserves all earlier exact hashes; `allow-same-origin allow-forms`
reveals a 120×32 box in the release/AOT corridor, agrees with CDP, and pins
Flutter pixels to
`57b9814c22902e40fc38180d79a1a78068f1b15154f4149bef8fbea5b6cf05cb`
before byte-identical target switching and renderer recovery. Inline style,
collections, and attribute nodes remain separate work.

**Fifth A1 checkpoint:** `HTMLElement.style` now retains one live inline
`CSSStyleDeclaration` across external `style` replacement and declaration API
writes instead of replacing its wrapper after each mutation. Focused proof
retains identity through `setAttribute` and `setProperty`, reflects current
declarations in both directions, advances exactly one renderer-source generation
per write, and recascades to 140×30. A hidden target preserves all prior exact
hashes; the release/AOT corridor reveals it at 120×32, matches its serialized
style and geometry through CDP, and pins exact Flutter pixels to
`b4fe0e2cdba9f98193e8dfc7aadb7fa892e508e269a4a94beb9c2970d8ce5096`
before byte-identical target switching and renderer recovery. Collections and
attribute nodes remain separate work.

**Sixth A1 checkpoint:** `Element.attributes` now retains one live
`NamedNodeMap`, with dynamic length/index/name lookup and stable attached `Attr`
identity across external writes. Attached `Attr.value` reads current state and
writes through the authoritative DOM mutation path. Focused proof retains both
identities through `setAttribute` and `Attr.value`, advances exactly one
renderer-source generation per write, and recascades to 140×30. A hidden target
preserves all prior exact hashes; the release/AOT corridor reveals it at 120×32,
agrees with CDP attribute/geometry state, and pins Flutter pixels to
`17cb0de692001fcb97dcab23c870b800e7e7c3b09010e312a0bbc64e496ec1ea`
before byte-identical target switching and renderer recovery. Detached Attr
lifecycle plus `setNamedItem`/`removeNamedItem`, and live structural collections,
remain separate work.

**Seventh A1 checkpoint:** live structural collection attributes now retain
resolver-backed identity while reflecting Page mutations: Node/Element
`childNodes`/`children`, document forms/images/links/scripts, form controls,
select/datalist options, labels, and table collections. Element/document
`getElementsByTagName` and `getElementsByClassName` return cached live
`HTMLCollection`s; `querySelectorAll` remains a static `NodeList` as required.
Focused proof performs two structural writes, observes exactly one
renderer-source generation each, preserves collection identity/index/name
lookup, and proves a pre-mutation query list stays static. The release/AOT click
corridor retains empty collections before Flutter-routed input, observes the
rendered `#dynamic.badge` afterward through the same objects, matches the
authoritative CDP node, and keeps the pinned classList scene hash byte-identical.
Detached Attr operations and live CSSOM/script scheduling remain separate work.

**Eighth A1 checkpoint:** `document.styleSheets` now retains one live
`StyleSheetList`, each author `<style>` resolves to the same stable
`CSSStyleSheet`, and retained `CSSRuleList`, `CSSStyleRule`, and rule
`CSSStyleDeclaration` objects resolve refreshed BrowserCore CSS after an
external style-element mutation. The CSSOM resource refreshes even when a
same-task synchronous geometry query consumed the pending mutation before the
ordinary runtime drain. Focused proof retains every identity, advances exactly
one renderer-source generation, and observes Stylo's 140×30 result. The
release/AOT corridor retains the objects across all seven earlier stages,
changes one dedicated author rule, observes 120×32 synchronously and through
the retained CSSOM plus CDP, and pins exact Flutter pixels to
`b09bce0ee8acf5ac3b40a2190241a6592880a3e47615c030469b2a887d118f1d`
before target switching and byte-identical renderer recovery. CSS rule mutation
APIs, detached Attr operations, and parser-module/task scheduling remain
separate work.

**Proof:** script-driven mutation visibly changes the Flutter scene; synchronous
and asynchronous geometry observe the right commit; CDP and page script inspect
the same nodes.

### A2. Unified loader and profile policy

- Finish one resource loader for documents, scripts, styles, images, fonts,
  fetch/XHR, frames, and downloads with shared request ids, redirect/policy,
  cookies/cache, priorities, cancellation, and diagnostics.
- Complete streaming/abort/progress behavior and policy-before-renderer exposure.
- Integrate profile state, partition keys, cert/proxy/path/portal host services,
  and a real bounded download lifecycle.

**Proof:** multi-context profile tests, waterfalls, CORS/CSP/SRI/mixed-content/
cache profiles, cancellation races, safe download tests, and Linux host smokes.

### A3. Renderer and frame model breadth

- Establish child-frame render mutations, same-origin access, cross-origin
  boundaries, sandboxing, nested viewport/scroll commits, and lifecycle teardown.
- Widen Flutter formatting only from reduced corridor/WPT failures; do not build
  isolated CSS helpers without a rendered commit consumer.
- Make animation/timers request bounded commits without starving BrowserCore or
  creating unbounded scene work.

### Alpha exit gate

Alpha requires:

- one BrowserCore profile/context/document/runtime lifecycle;
- one Flutter mutation/commit renderer for GUI and rendered automation;
- two contexts that independently load, script, render, inspect, and share only
  intended profile state;
- active navigation/runtime/render work cancellable without stale commits;
- same-task DOM/style mutation driving correct synchronous geometry and visible
  pixels;
- input, scroll, find/selection, CDP, and accessibility naming exact commits; and
- reproducible architecture, compatibility, limitations, and measurements.

## Beta — build a useful measured browser

### B1. Rendering and content fidelity

Drive the Dart formatter from reductions and pinned profiles:

- common block/inline formatting, floats, positioned/fixed/sticky, overflow,
  flex, grid, tables, intrinsic sizing, replaced elements, fragmentation/print;
- responsive raster images, SVG basics, accepted web fonts, gradients, borders,
  shadows, transforms, opacity/compositing, filters, animation;
- typography, bidi/writing modes, fallback, line breaking, caret/selection; and
- browser-correct form-control rendering and interaction.

Prioritize typography, intrinsic sizing, tables, controls, and scrolling because
they dominate real-page failures.

### B2. Runtime and application basics

Widen live DOM, HTML, CSSOM, events, forms, navigation, URL/encoding/streams,
timers, observers, messaging, WebSocket/EventSource, modules, workers, frames,
sandboxing, and resource timing from corridor failures. Unsupported APIs remain
explicit; inert shape does not count.

### B3. Network, security, privacy, and downloads

Complete transfer streaming, upload/download progress, authentication/proxy,
HTTP/2 interoperability, cache freshness, safe filenames/resume/history,
Permissions Policy, COOP/COEP/CORP, HSTS, Trusted Types, partitioned state,
private-network access, prompts, and failure classification.

### B4. Daily-smoke Flutter product

Deliver robust tabs, address/search, reload/stop, history, find, zoom, downloads,
permissions, error/recovery pages, session restore, settings/privacy controls,
keyboard navigation, safe external opens, and host integration. Chrome remains a
controller over BrowserCore; renderer state remains ephemeral and commit-bound.

### B5. Automation and inspection products

Support independent targets/contexts, reliable waits, DOM/runtime handles,
commit-aware input, downloads, dialogs, network/console/lifecycle events,
Flutter-scene screenshots, permissions, and bounded traces. Drive additions from
external Playwright workflows rather than method-name counts.

### B6. Compatibility, performance, and reliability loop

- Expand pinned WPT profiles across parser, DOM/events/forms, CSS/layout/paint,
  network/security, storage/history, runtime APIs, and accessibility.
- Publish a controlled corridor spanning static content, docs, forms, downloads,
  app-like pages, and automation-heavy pages.
- Track startup, navigation, cascade/layout/paint/commit time, frame stability,
  memory, capture latency, throughput, install size, and profile growth.
- Bound malformed/content-controlled work and make renderer/runtime/profile
  recovery diagnosable.

### Beta exit gate

The corridor loads in Linux GUI and chrome-less automation, supports meaningful
interaction/persistence, survives restart/cancellation/renderer loss, and has
published screenshots, reductions, profile counts, automation results,
measurements, and known gaps. Other platforms remain committed targets until they
pass their own gates.

## v1.0 — honest daily-driver minimum

Vixen may call itself v1.0 when:

- common document, documentation, form, download, and app-like corridor pages are
  readable and usable with stable typography, images, layout, scrolling,
  interaction, navigation, and profile state;
- GUI and Playwright/CDP share BrowserCore and the Flutter renderer and recover
  predictably from network, document, runtime, renderer, and profile failures;
- supported security/privacy behavior is fail-closed and tested; single-process
  isolation limits are prominent;
- Linux install/update, certs, fonts, portals, downloads, GPU, settings, session
  restore, accessibility, and clear-data flows pass, and each additional platform
  claimed as supported by that release passes its native gate;
- compatibility, performance, memory, binary/install size, and unsupported
  capabilities are published from reproducible commands; and
- every claim maps to an acceptance gate, fixture/profile/smoke, and owner.

v1.0 is a useful supported subset, not the end of the replacement goal.

## Platform expansion

After Linux R8 and beta-quality renderer stability:

1. **macOS and Windows:** same render mutation/commit broker, native Flutter
   runner, fonts, input/IME, accessibility, host services, signing/packaging,
   capture, size, and performance evidence.
2. **Android:** pinned V8 source/toolchain, lifecycle/process recreation,
   touch/IME, accessibility, host services, split-ABI packaging, capture, and
   resource budgets through the same renderer contract. A prewarmed builder is
   allowed only as a reviewed digest-pinned cache that exactly matches the
   Flutter/engine/JDK/API/NDK/Gradle pins and still supports reproducible Rust/V8
   source builds.
3. **Apple Silicon iOS Simulator:** same Flutter renderer, BrowserCore, V8
   JavaScript/WebAssembly, simulated lifecycle/input/accessibility/host services,
   and reproducible Xcode runner. Physical iOS requires a new decision.
4. **WebAssembly:** widen API and resource/conformance proof on every declared
   target without adding an alternate runtime.

## Replacement horizon

After v1, prioritize by measured site/user impact:

1. **Accessible browser:** complete semantics, screen-reader interaction, keyboard,
   caret/selection, forced colors, reduced motion, and native controls.
2. **Media:** Flutter-compatible platform media integration, codecs, controls,
   tracks, fullscreen/PiP, autoplay/permissions, Media Source, and WebAudio.
3. **Offline applications:** IndexedDB, Cache Storage, service workers, workers,
   file/blob streaming, notifications, installability, and offline lifecycle.
4. **Communications:** production WebSocket/EventSource, WebRTC/device permissions,
   richer streaming/compression, and justified WebTransport.
5. **Graphics/documents:** Canvas 2D, SVG breadth, WebGL/WebGPU, print/PDF, color
   management, advanced typography/writing modes, and CSS long tail.
6. **User ecosystem:** scoped extensions, content blocking, password/autofill,
   import/export, developer tools, and policy controls.
7. **Defense in depth:** renderer/content sandboxing, site isolation/OOPIF,
   brokered host access, crash containment, update/signing hardening.
8. **Broader compatibility:** continuously widen WPT and the real-site corridor
   until exceptions are uncommon across supported targets.

## Immediate execution queue

Work top-to-bottom and finish/document/commit each slice:

1. **Continue A1 live document convergence:** use the landed live `dataset`
   vertical as the pattern; take the next snapshot/shim family only with one
   mutation/revision, synchronous geometry, CDP, and exact Flutter-pixel proof.
2. **Preserve the R8 host corridor:** keep real Mozc preedit/commit and native
   AT-SPI role/state/bounds/action → DOM → newer-commit evidence green while
   widening shared-core behavior; do not replace it with injected text or
   BrowserCore geometry.

Do not reintroduce native layout/paint/frame ownership while stabilizing. A
security, data-loss, or release-blocking regression may preempt the queue.

## Velocity and deletion policy

- **One renderer slice at a time.** A critical BrowserCore fix may run beside it;
  adjacent feature breadth may not.
- **Delete before adapting.** If transitional code has no independent
  BrowserCore value and replacement evidence exists, remove it instead of adding
  compatibility layers.
- **No speculative renderer framework.** Start from the R3 fixture and generalize
  only when a second reduced case proves the need.
- **One trust boundary per commit.** Split protocol, ABI/broker, formatter,
  automation host, synchronous flush, and deletion at independently reviewable
  points.
- **Use the test ladder once.** Focused checks while editing, relevant gate before
  commit, `just gate-push` once for a coherent push batch.
- **Executable evidence beats prose.** A commit advances only with DTO adversarial
  tests, fixture pixels/geometry, a race, a native smoke, or measured output.
- **Update or delete gates with ownership.** Tests that prove removed WebRender/
  EGL/texture behavior disappear at cutover; tests of browser semantics move to
  the Flutter renderer rather than pinning old implementation details.
- **Keep handoffs cheap.** Update limitations and leave the next smallest queue
  item; completed queue prose is replaced, not accumulated.

## Working rule

Every milestone lands with:

- one named authoritative owner and no parallel browser/renderer truth;
- exact revisions/commit ids across every renderer boundary;
- focused unit/adversarial tests plus one browser-visible fixture or smoke;
- stable bounded diagnostics at trust and lifecycle boundaries;
- compatibility/limitation updates when behavior changes; and
- the cheapest focused checks followed by the relevant hk/`just` gate.

Prefer small, boring verticals. A large surface of plausible APIs is less valuable
than one exact BrowserCore mutation becoming one Flutter commit observed by
pixels, script, input, CDP, and accessibility together.
