# Roadmap

This roadmap moves Vixen from the current WebRender/RGBA prototype to the full
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

As of 2026-07-14 the repository has:

- one seven-crate Rust workspace with hk/`just` gates, stable diagnostics, fuzz
  targets, a fixture/WPT harness, and a committed **270 fixture / 2,027 check**
  100% baseline;
- dependency-free renderer protocol v1 DTOs and reference validation in
  `vixen-api` for exact revisions, bounded source snapshots/mutations/resync,
  atomic commit/presented state, geometry/text/scroll queries, displayed-commit
  input, semantic actions, replay rejection, and explicit handle retirement;
- one `BrowserCore` owner for profile services, contexts, navigation generations,
  DOM/Page state, V8 runtimes, history, input intent, inspection, and ordered
  events used by Flutter, native headless, CDP, and WPT;
- `html5ever`, Stylo selector/cascade integration, `deno_core`/V8, shared
  network/security policy, and bounded redb profile tables;
- generation-cancellable main-document, external-script, stylesheet, and bounded
  PNG loading plus deadline-bounded V8/runtime-fetch cancellation;
- a useful CDP/Playwright slice and a Linux Flutter shell with native Wayland
  chrome, input/IME, Semantics, scrolling/find/zoom, recovery, and deterministic
  release/Cage evidence; and
- a partially implemented Rust layout tree/display list/WebRender renderer,
  surfaceless EGL in native headless and Flutter frame capture, retained RGBA
  frame ABI/pools, and a Linux pixel-buffer texture presenter.

The final bullet is now **migration debt**, not a foundation to widen. It remains
the production comparison path until the Flutter vertical proves parity, but it
receives only security, data-loss, or release-blocking correctness fixes.
Deterministic text metrics, unfinished Rust formatting helpers, paint primitives,
EGL surface code, texture transport, and WebRender-specific tests may be removed
as soon as their replacement evidence exists. Partially implemented code does not
justify preserving the wrong ownership boundary.

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

**Proof:** ABI/header/layout checks, Dart/Rust golden round trips, malformed and
stale wire tests, cancellation/timeout tests, queue bounds, worker-blocked broker
service, shutdown, and full resync. Production still displays the old frame.

**Implemented evidence:** the bounded `RenderBroker` is independent of the
serialized BrowserCore controller lock; C `renderer_poll`/`renderer_respond`
entrypoints and handwritten Dart records use strict correlated envelopes and the
existing tokenized output release contract. Timeout, late response, cancellation,
shutdown, malformed wire, double release, worker-blocked progress, native header,
and Rust/Dart golden tests are checked in. The native worker shares only its
opaque process token with the UI-side broker endpoint, and the scripted fake has
the same queue/response shape. Normal browsing still uses the old frame.

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
presentation, stale/equal-snapshot rejection, deterministic resync, and reset.
Software and Impeller-requested captures have separate exact raw-RGBA hashes.
The formatter is not connected to normal shell presentation.

### R4. One interactive commit vertical

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

### R6. Synchronous layout and recovery gate

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

### R7. Production cutover and aggressive deletion

Cut over only after R3–R6 are green, then remove in one reviewed migration series:

- `webrender`, `gleam`, `GlContext`, renderer integration, and image upload code;
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

### R8. Linux stabilization and rebaseline

- Reproduce the compatibility manifest and imported profiles through appropriate
  native or Flutter-hosted paths; update `COMPAT.md` only from output.
- Re-run Linux interaction, IME, AT-SPI, release archive, startup, memory, frame,
  screenshot latency, and profile-growth evidence.
- Rebaseline hello-Flutter versus Flutter+Vixen and attribute removed
  WebRender/EGL/frame code, new Dart formatter, and chrome-less-host costs.
- Fix renderer-transition regressions before broadening APIs or resuming FlatPark
  publication work.

**Exit:** the controlled Linux corridor uses no transitional renderer component,
all renderer failure modes are bounded, and the next compatibility failure can be
reduced directly against the final architecture.

## Alpha — converge live browser state on render commits

After R8, resume shared-core correctness in this order.

### A1. Live document/runtime convergence

- Replace remaining Page/runtime compatibility snapshots with live
  Node/Element/Document, CSSOM, events, focus, selection, forms, history, and
  storage resources.
- Make every relevant mutation produce one render-source revision and invalidate
  accepted geometry explicitly.
- Execute parser classic/module scripts with document event-loop and microtask
  ordering; preserve realm teardown and same-origin frame boundaries.
- Delete plausible inert compatibility shims as real owners land.

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

1. **R4 interactive commit:** route displayed-commit input, scrolling, text
   queries, zoom, Semantics, and lifecycle suppression through that vertical.
2. **R5 automation host:** add the chrome-less Flutter host and migrate coherent
   fixture/screenshot/CDP groups to exact presented commits.
3. **R6 synchronous layout:** connect BrowserCore mutation flushes to the landed
   broker with cancellation, deadlines, loss, and full-resync recovery.

Do not start another native interaction, font, Rust layout, paint, texture, or
packaging slice before these are complete unless it fixes a security/data-loss or
release-blocking regression.

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
