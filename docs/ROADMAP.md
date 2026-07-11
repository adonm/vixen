# Roadmap

This is the delivery sequence from the current component-rich prototype to the
full project goal: a credible Firefox replacement with one focused Flutter shell
targeting Linux, macOS, Windows, Android, and the Apple Silicon iOS Simulator, plus first-class headless/CDP
automation. It is deliberately more ambitious than a demo-browser plan, but it
does not turn framework support into an unsupported Vixen compatibility claim.

Historical phase instructions live in [`PLAN.md`](PLAN.md), executable evidence
in [`MILESTONES.md`](MILESTONES.md), and measured support in
[`COMPAT.md`](COMPAT.md). Already-landed feature inventories do not belong in the
future milestones below.

## Destination and release ladder

The stages are capability gates, not dates:

1. **Alpha — one browser architecture.** GUI, headless, CDP, WPT, page scripts,
   and profile services drive one engine-owned browser lifecycle. Narrow behavior
   is acceptable; parallel state models are not.
2. **Beta — a measured useful browser.** A controlled real-site corridor works
   in the Linux Flutter GUI and automation with representative rendering,
   interaction, persistence, downloads, diagnostics, accessibility, and host
   integration; desktop expansion uses the same bridge.
3. **v1.0 — an honest daily-driver minimum.** The published corridor is reliable
   enough for focused daily use, release/security operations are credible, and
   every supported platform and capability has reproducible evidence.
4. **Replacement horizon — broad modern-browser capability.** Media,
   accessibility, offline applications, richer graphics/communications, extension
   support, and stronger process isolation expand the useful site set until
   “Firefox replacement” describes ordinary use rather than only a corridor.

No stage implies global Firefox or WPT parity. Compatibility claims always name
the profile, platform, command, and measured result.

## Proven baseline — use it, do not roadmap it

As of 2026-07-10 the repository has these building blocks:

- An eight-crate workspace, hk/`just` development gates, stable diagnostics, fuzz
  targets, and a WPT/fixture harness. The committed manifest currently measures
  **269 fixtures / 2,015 checks at 100%**; `COMPAT.md` owns the detailed counts.
- `html5ever` parsing, Stylo-backed selector/cascade integration, a Vixen-owned
  layout tree and focused formatting helpers, one display list, and one WebRender
  path used by GUI/headless screenshot surfaces.
- A persistent `deno_core`/V8 runtime seam, generated WebIDL scaffolding, focused
  page-backed DOM/CSSOM/geometry/events/forms/selection behavior, and explicit
  Rust ops/resources for selected stateful Web APIs.
- Shared network policy primitives for URL validation, redirects, cookies, CSP,
  mixed content, referrer policy, CORS/preflight, cache revalidation, SRI, and
  stable network events; fetch and the minimal XHR surface reuse these parts.
- Bounded redb tables for cookies, Web Storage, fetch cache, history, sessions,
  downloads, permissions, and HSTS/security records, plus explicit clear-data
  selections and app-ID/XDG path helpers.
- A useful CDP slice covering navigation, runtime evaluation/handles, DOM basics,
  input, lifecycle/network/console/dialog events, screenshots, permissions,
  tracing-lite, and stable protocol errors, with an external Playwright smoke.
- A Relm4/libadwaita compatibility-shell vertical with tabs, URL loading, basic
  navigation, visible WebRender output, diagnostics, and bounded session
  restore. The Linux Flutter alpha slice now has chrome, real BrowserCore FFI,
  deterministic fake tests, and visible WebRender output through a bounded RGBA
  pixel-buffer texture; input, accessibility, host services, packaging, and
  compatibility-shell parity remain open.

These are substantial components now routed through one initial browser owner,
not yet a broadly compatible browser. API shape or inert reflection is still not
counted as implemented behavior when the underlying subsystem does not exist.

## The critical gap

The production path is now the browser-scoped `BrowserHandle` seam rather than
the older tab-shaped `vixen_api::Engine` trait. Shell, headless CLI, CDP targets,
and the WPT adapter create contexts in one `vixen-engine::browser::BrowserCore`;
they no longer own parallel `Page`, `JsRuntime`, network, cookie, history, or
profile-session state. CDP has one core context/runtime generation per target,
with bounded generation-scoped remote handles.

The largest remaining delivery risk has moved from frontend ownership
duplication to the live document lifecycle. HTML parsing, configured/author
script items, and terminal lifecycle stages now yield cooperatively on the core
owner. Individual V8 execution and promise pumping have a bounded deadline with
isolate recovery, but navigation cancellation cannot yet interrupt them before
that deadline, native host ops and non-script discovered-resource fetches remain
synchronous, and compatibility projections still coexist with the live page/
runtime. Parser-discovered external classic scripts now use generation-cancellable
worker I/O rather than blocking the owner. Adding broad API shape before
converging those paths would preserve plausible but disconnected state.

Other material gaps remain:

- main-document source loading is asynchronous; HTML parsing, configured/author
  script items, and DOMContentLoaded/load/settle stages are generation-checked
  owner-thread quanta. Runtime construction and native host calls are not yet
  interruptible; individual V8 jobs are deadline-bounded but not navigation-
  cancellation-aware. External classic-script file/HTTP reads are cancellable
  worker tasks; other discovered-resource fetches remain synchronous or absent;
- DOM/runtime snapshots and compatibility projections still coexist with live
  page state;
- layout uses deterministic text metrics and narrow block/inline/flex/grid
  coverage rather than shaped text and a broad formatting pipeline;
- images, fonts, subresource loading, media, accessibility, workers, IndexedDB,
  service workers, and multi-frame/multi-page execution are absent or only
  browser-shaped probes;
- downloads have persistence/protocol shape but no complete HTTP transfer
  lifecycle;
- Linux cert/proxy/font/portal/GPU behavior and release measurements are not yet
  proven across a supported matrix; and
- the first Linux Flutter bridge, shell, and RGBA external-texture transport are
  implemented, but input, accessibility projection, host services, packages,
  release evidence, and all non-Linux runners remain open; native
  BrowserCore/V8/WebRender viability remains unproven outside Linux.

## Design rules for every stage

1. **One authoritative state graph.** Profile → browser → browsing context →
   document owns state. Frontends send commands and observe events; they do not
   create alternate history, network, runtime, or permission models.
2. **One behavior, many adapters.** GUI, headless, CDP, WPT, and page scripts
   must reach the same navigation, DOM, layout, network, and profile operations.
3. **State owner before API shape.** Stateful WebIDL/CDP/shell surfaces land only
   after an engine owner exists. Pure immutable value objects may remain JS-only.
4. **Generational asynchronous work.** Every navigation and document has a stable
   id. Cancellation invalidates the generation, transport aborts where possible,
   and stale completions cannot commit state or emit success events.
5. **Policy before exposure or side effects.** Validate untrusted inputs and apply
   URL/CSP/CORS/mixed-content/integrity/storage policy before script exposure,
   cache insertion, persistence, download creation, or UI handoff.
6. **Bound everything user or content controlled.** Queues, caches, object
   handles, traces, profile tables, decoded resources, DOM growth, script work,
   and diagnostics need explicit limits and useful failure modes.
7. **Inspection cannot invent a second page.** CDP snapshots and geometry may
   request an explicit style/layout update or return a stable stale-state error;
   they may not maintain automation-only DOM or layout state.
8. **Measure, then budget.** Binary size, memory, startup, navigation, frame time,
   and profile growth get reproducible baselines before hard thresholds. The
   local headless/process-memory/profile/artifact foundation is now checked in;
   representative host and live workloads still precede budgets. Do not
   preserve fictional limits from an obsolete dependency graph.
9. **Real-site failures become reductions.** A screenshot starts triage. A local
   fixture, pinned WPT case, or explicitly tracked unreduced failure prevents
   regression.
10. **Flutter is presentation, not a second browser.** BrowserCore owns browser
     truth and accessibility source data; Dart owns chrome and host-service UI.
     One WebRender output crosses a bounded texture transport. Each platform and
     ABI is supported only after its gate passes on the latest stable major OS
     release pinned at the release cutoff. Older OS versions are best-effort.

## Alpha — converge on one browser core

Alpha freezes an architecture capable of carrying the full goal. Complete these
in order; later work may proceed in parallel only when it does not create a new
state owner.

Progress as of 2026-07-11: A1 is routed through the dependency-free typed
`vixen-api` command/event seam and one `vixen-engine::browser::BrowserCore`.
BrowserCore owns one engine thread, profile Store/network/cookies, bounded
context/runtime registries, history, evaluation, inspection, and paint inputs.
WPT, headless CLI, CDP, and the GTK shell are thin adapters over that owner; two
CDP/shell contexts prove independent globals/sessionStorage/history with intended
profile sharing. The 2,015-check fixture manifest, GTK-free shell tests, and the
external Playwright smoke remain green.

The first A2 slices are also landed: dispatch acknowledges navigation before
source completion; a bounded Tokio loader performs cancellable HTTP/file reads;
each completion carries its context/navigation generation; current-generation
cookie deltas merge at the core boundary; successful navigations emit the ordered
phase sequence; live network progress surfaces redirects with fresh typed request
ids while the final response is still pending, without completion-time replay;
HTML parsing advances in bounded UTF-8-safe quanta with commands checked between
quanta; and deterministic navigate/navigate, navigate/stop, redirect/stop,
reload/active-load, history/active-load, stop/parse, reload/parse, and
history/parse races prove stale work cannot commit, append history, overwrite
cookies, or emit terminal success. One core terminalization
boundary now atomically takes the matching generation and emits exactly one
settled, failed, or cancelled phase across success, stop, supersede, close, and
shutdown paths. Runtime construction and post-commit script/resource work remain
on the owner thread. Individual V8 execution, promise pumping, microtask
checkpoints, and runtime-effect drains now share a five-second production
deadline; V8 termination is cancelled after the stack unwinds so the isolate is
reusable, author timeouts surface as `script.timeout`, later scripts continue,
and the committed navigation still settles. Configured and parser-discovered
scripts now advance one item per generation-checked quantum, followed by separate
DOMContentLoaded, load, and settle quanta. External classic scripts resolve and
pass CSP and active-mixed-content policy before every initial/redirect-hop request,
load on the bounded core worker runtime, and check final status/`nosniff` before
cookie merge or execution. They carry exact context/navigation/document/runtime/
request generations. Accepted cookie deltas merge into the core, current runtime,
and current profile-store partitions without replacing concurrent unrelated
cookies; other contexts and profile reopen observe them. Stop, supersede, close,
and shutdown abort pending script loads and emit bounded request/failure effects;
stale completions cannot execute, persist cookies, or resume lifecycle work. Main-
document and script `file:` reads also enforce the configured body limit before
and during allocation. Stop or supersede preserves completed script mutations while
suppressing unstarted items and later success lifecycle events. Author exceptions
surface as runtime effects, allow later independent scripts to run, and do not
turn a committed document into a failed navigation. Navigation-triggered runtime
interruption, synchronous native host calls, and non-script resource loading
remain the next A2 boundary.

The obsolete fail-closed `Page` string-expression path and headless test-only
classifiers are deleted; all evaluation adapters use `BrowserCore`/`JsRuntime`.
Runtime/document snapshots still need convergence with the live page state.

Cross-cutting delivery work has also advanced without widening browser claims.
The safe Flutter controller now has a versioned handwritten C ABI with opaque
process tokens, bounded JSON messages and retained output/frame allocations,
stable tagged responses/events/errors, polling-only event delivery, and panic
containment. The Linux shell adds handwritten Dart bindings, deterministic fake
tests, a production worker isolate, bounded RGBA `FlPixelBufferTexture`
transport, physical viewport mapping, and generation-checked pointer/wheel/key
dispatch through BrowserCore hit testing. A bounded, mutation-generation-tagged
BrowserCore projection now maps roles/names/states/bounds and tap into Flutter
Semantics; hierarchy, richer actions, incremental updates, and native AT remain.
External WPT profiles now reject mutable or
mismatched revisions, dirty/non-root checkouts, and fixtures outside declared
sparse paths. Headless `--incremental` now captures real before/after frames from
one BrowserCore context around live evaluation. Hosted CI executes architecture,
native ABI, Node baseline, and external Playwright/CDP evidence, with separate
dependency-policy and scheduled bounded-fuzz jobs. Fetch Metadata, CORP, and
cookie `Domain` validation now share one static Mozilla Public Suffix List policy
including private suffixes; non-registrable site comparisons fail closed.

### A1. Engine-owned profile, browser, and browsing contexts — landed

- One production browser core in `vixen-engine` is exposed through the typed
  `vixen-api` command/event seam.
- One profile service owns the store, cookies, cache, permissions, HSTS,
  downloads, clear-data policy, and host configuration. One browser service owns
  the tab/context registry. Each context owns its active document, runtime,
  session history, viewport/input state, and navigation controller.
- Tab/context, document, frame, navigation, request, runtime-context, and download
  records have stable typed ids. Those ids appear in diagnostics/events so
  stale work and cross-target routing are testable.
- Non-`Send` DOM and V8 state runs on one engine-owned thread. Shell
  and protocol I/O may use workers, but engine ownership must not be split across
  per-frontend state machines.
- Shell/headless are thin adapters. The architecture gate forbids their former
  direct `vixen-net`/`vixen-store` composition.

**Proof:** a production `Engine`/browser-core implementation, two tabs sharing
profile state but not session state, GUI/headless/CDP navigation through the same
commands, dependency-boundary checks, and tests that no frontend owns an
independent navigation history.

### A2. Asynchronous navigation and document commit

- Model navigation as explicit phases: intent → policy → request → response →
  commit → parse → scripts/subresources → DOMContentLoaded → load → settled or
  failed/cancelled.
- Make reload, stop, redirects, history traversal, form submission,
  `location`/history APIs, `document.write`, error pages, and session restore enter
  the same controller.
- Propagate cancellation through transport, body reads, parser/resource tasks,
  and runtime jobs. A superseded navigation must not mutate the current document,
  cache forbidden data, append history, or emit a later `load`.
- Separate provisional and committed documents so errors before and after commit
  have deterministic behavior.

**Proof:** race tests for navigate/navigate, navigate/stop, redirect/stop,
history/reload, late network completion, and runtime reset; matching shell and
CDP lifecycle traces; no stale commit after cancellation.

**Current proof:** navigate/navigate, navigate/stop, redirect/stop, reload during
an active load, and history traversal during an active load use gated transport
plus forced late completions; a gated redirect is observed before its final
response is released, stop reports the latest redirect request id, and stale
redirect progress is generation-rejected without duplicate replay; terminal
outcomes pass through one generation-checked core boundary; HTML parsing yields
between bounded quanta, and stop, reload, and history traversal during parse
reject stale parser work without commit; shell/headless/WPT/CDP wait for matching
typed terminal events; the external Playwright smoke covers navigation, history,
reload, and ordered opt-in lifecycle notifications plus `document.write`/
`setContent`. One CDP socket event pump now owns continuations for navigate/reload,
target creation, cross-document history traversal, and runtime/input-triggered
navigation. BrowserCore returns the exact ordered navigation ids created by
evaluation/input actions, so the adapter consumes superseded generations and
allows same-connection stop races without a second event consumer. Abandoned
wire operations retain bounded claimed outcomes so late completion cannot poison
a later request. Configured initial-URL readiness remains a deliberate pre-connect
wait; the default `about:blank` target no longer performs a redundant navigation.
Configured/author scripts and lifecycle completion also yield between items;
author exceptions produce `Runtime.exceptionThrown` while navigation continues
to settlement. Infinite V8 execution is deadline-terminated with reusable-isolate
proof, unresolved promises fail closed, and author timeouts allow later scripts
and navigation settlement. Gated external-script tests prove in-order execution,
same-owner stop/supersede responsiveness, pre-request redirect-CSP and active-
mixed-content checks, profile-wide cookie sharing/persistence, bounded file reads,
and rejection of late source/cookie/document/runtime completions.
Navigation-aware V8 interruption, synchronous native host calls, and non-script
resource loading remain.

### A3. Live document/runtime integration

- Replace remaining runtime/document snapshots with live page-backed
  Node/Element/Document, CSSOM, events, focus, selection, forms,
  history, storage, and geometry resources.
- Execute parser-discovered classic/module scripts in document order with an
  event loop and microtask checkpoints tied to the document lifecycle.
- Make DOM mutation invalidate style/layout/paint and inspector state through one
  explicit mechanism. Preserve browser event ordering and realm teardown.
- Establish the frame/realm model needed for same-origin child frames and
  cross-origin boundaries, even if initial frame support is narrow.
- Delete compatibility shims as their supported behavior moves to the live path;
  unsupported APIs must remain explicit rather than returning plausible fiction.

**Proof:** DOM/events/forms/history/storage profiles running through V8 against
the live document, script-driven rendered mutations, realm/navigation teardown
tests, and CDP queries that observe exactly the same nodes.

### A4. Real document loader and profile policy

- Build one resource loader for document, script, style, image, font, fetch/XHR,
  and download requests, with shared request ids, redirect/policy processing,
  cookies/cache, priorities, cancellation, and diagnostics.
- Integrate profile-shared cookies, cache, permissions, HSTS, localStorage, and
  history. Keep sessionStorage and in-flight work context scoped; define
  partition keys before adding third-party persistence.
- Finish streaming/abort/progress semantics where observable and ensure response
  policy runs before exposure, execution, decode, persistence, or cache insert.
- Treat cert roots, proxies, XDG paths, portals, fonts, and GL/EGL capabilities as
  explicit host services with structured diagnostics.

**Proof:** multi-tab profile tests, resource waterfall assertions, CORS/CSP/SRI/
mixed-content/cache WPT profiles, cancellation tests, and controlled Linux host
integration smokes.

### A5. Owned style, layout, and paint lifecycle

- Replace the compact cascade projection with full Stylo computed values behind
  the authoritative document/style state.
- Replace deterministic text metrics with real font discovery, shaping,
  fallback, line breaking, glyph runs, and intrinsic measurement.
- Carry DOM/style invalidation through the Vixen layout tree to one display list
  and WebRender path; no frontend geometry or post-pass coordinate correction.
- Establish scroll, hit-test, selection/caret, image, clipping, stacking,
  transform, and animation state as engine-owned data.

**Proof:** shaped-text and fallback-font fixtures, script mutation → repaint,
GUI/headless pixel comparisons, layout/ref profiles, and inspector queries during
invalidation.

### Alpha exit gate

Alpha is reached only when:

- a production browser core owns lifecycle/profile state;
- shell, headless, CDP, and WPT are adapters over it;
- two independent contexts can load, script, render, inspect, and share only the
  profile state they should share;
- active navigation can be cancelled without stale commits;
- the live DOM/runtime drives visible layout/paint; and
- the architecture, compatibility counts, known gaps, and measurements are
  reproducible from checked-in commands.

## Beta — turn the architecture into a useful browser

Once ownership is singular, broaden capability by user-visible risk rather than
by easiest API count.

### B1. Rendering and content fidelity

- Complete common block/inline formatting, floats, positioned/fixed/sticky
  layout, overflow/scroll, flex, grid, tables, intrinsic sizing, replaced
  elements, and enough fragmentation/print behavior for the measured corridor.
- Support responsive raster images, SVG document/image basics, web fonts,
  gradients/borders/shadows, transforms, opacity/compositing, filters, animation,
  caret/selection, and native-looking form controls through the same pipeline.
- Prioritize typography, intrinsic sizing, tables, form controls, and scrolling:
  they dominate real-page breakage even when small synthetic layout tests pass.

### B2. Browser runtime and application basics

- Widen live DOM, HTML, CSSOM, events, forms, navigation, URL, encoding, streams,
  timers, observers, messaging, WebSocket, and EventSource behavior from failing
  profiles and corridor reductions.
- Add multi-frame execution, same-origin access rules, sandboxing, module loading,
  workers, and resource timing sufficient for representative app-like pages.
- Keep inert probes out of the supported matrix until observable behavior exists.

### B3. Network, security, privacy, and downloads

- Complete HTTP transfer streaming, upload/download progress, redirects,
  authentication/proxy behavior, HTTP/2 interoperability, cache freshness, and a
  real download manager with safe filenames, resume where supported, and profile
  history.
- Integrate Permissions Policy, sandboxing, COOP/COEP/CORP, HSTS, nosniff,
  Trusted Types sinks, partitioned state, private-network access, and prompt/user
  decisions into the shared loader/profile model.
- Diagnose policy block, DNS/connectivity, TLS/cert, HTTP/protocol, unsupported
  feature, and likely anti-bot/fingerprinting failure separately.

### B4. Daily-smoke Flutter product

- Deliver robust tabs, address/search, reload/stop, back/forward, find, zoom,
  downloads, permission prompts, error/recovery pages, history, session restore,
  settings, clear-data/privacy controls, keyboard navigation, and safe external
  opens.
- Make every chrome transition resilient to late engine events, tab close,
  document/runtime reset, and failed profile writes.
- Keep the shell focused: UI exists to browse, recover, inspect, automate, or
  control profile/security state—not to become a feature buffet.
- Prove the same BrowserCore command/event/texture/semantics contract first on
  Linux, then macOS and Windows; keep platform adaptations in chrome, native
  runner, accessibility, texture, and host-service layers.

### B5. Automation and inspection as products

- Support independent targets/runtimes, browser contexts where semantics exist,
  reliable navigation waits, DOM/runtime handles, input, downloads, dialogs,
  network/console/lifecycle events, screenshots, permissions, and bounded traces.
- Drive additions from external Playwright workflows and documented protocol
  contracts, not from method-name coverage alone.
- Stress inspection during animation, mutation, style/layout invalidation,
  navigation, downloads, errors, and runtime recovery.

### B6. Compatibility, performance, and reliability loop

- Expand pinned WPT profiles across parsing, DOM/events/forms, CSS/layout/paint,
  network/security, storage/history, runtime APIs, and accessibility-relevant
  behavior. Report local/imported and source×category results.
- Publish a reproducible real-site corridor spanning static content, docs,
  forms, downloads, app-like pages, and automation-heavy pages.
- Track startup, first navigation, style/layout/paint, frame stability, memory,
  screenshot latency, transfer throughput, binary/install size, and profile
  growth. Add budgets only after representative baselines exist.
- Eliminate panics/data loss on malformed content; bound content-controlled work;
  make recovery and diagnostics useful.

### Beta exit gate

The corridor loads in GUI and headless, supports meaningful interaction and
persistence, survives restart/error/cancellation cases, and has published
screenshots, reductions, WPT/profile counts, automation results, performance
measurements, and known gaps on the supported Linux Flutter matrix. A platform
not yet through its own gate remains a committed target, not a supported beta
claim.

## v1.0 — honest daily-driver minimum

Vixen may call itself v1.0 when all of the following are true:

- common document, documentation, form, download, and app-like pages in the
  published corridor are readable and usable with stable typography, images,
  layout, scrolling, interaction, navigation, and profile state;
- shell and Playwright/CDP use the same engine paths and recover predictably from
  network, document, runtime, renderer, and profile failures;
- supported network/security/privacy behavior is fail-closed, observable, and
  backed by tests/fuzzing/audit; single-process isolation limits are prominent;
- Linux Flatpak install/update, certs, fonts, portals, downloads, GPU, settings,
  session restore, and clear-data flows pass through the Flutter shell, and each
  additional declared release platform passes its native gate;
- compatibility, performance, memory, binary/install size, and unsupported
  capabilities are published from reproducible commands; and
- every v1 claim maps to an acceptance gate, fixture/profile or smoke, and an
  owner in the shared architecture.

v1.0 is a useful supported subset, not the end of the Firefox-replacement goal.

## Replacement horizon — continue until ordinary browsing is credible

After v1, prioritize these programs by measured site impact:

1. **Accessible browser:** accessibility tree, AT-SPI integration, screen-reader
   smoke, keyboard/caret/selection fidelity, forced colors, reduced motion, zoom,
   and accessible native controls.
2. **Media platform:** GStreamer-backed audio/video, common codecs, controls,
   captions/tracks, fullscreen/PiP, autoplay/permission policy, Media Source where
   justified, and WebAudio basics.
3. **Offline/application platform:** IndexedDB, Cache Storage, service workers,
   workers/shared workers, file/blob streaming, notifications, installable apps,
   and offline lifecycle semantics.
4. **Communications:** production WebSocket/EventSource, WebRTC and device
   permissions, richer streaming/compression, and WebTransport only where demand
   and security design justify it.
5. **Graphics and documents:** Canvas 2D behavior, SVG breadth, WebGL, WebGPU,
   print/PDF, color management, advanced typography/writing modes, and the long
   tail of CSS layout/paint.
6. **User ecosystem:** a deliberately scoped extension model, content blocking,
   password/autofill integration, import/export, developer tools, and policy
   controls without cloning every Firefox chrome feature.
7. **Defense in depth:** renderer/content process sandboxing, site isolation or
   OOPIF, brokered host access, crash containment, and update/signing hardening.
   Treat this as an explicit architecture generation, not an ad-hoc worker pool.
8. **Broader compatibility:** continuously widen WPT and real-site profiles until
   exceptions are uncommon enough that replacement is an honest default-use
   description across the five committed GUI targets.

## Immediate execution order

The core ownership and local headless measurement foundations are landed. The
next work has two interleaved tracks: Flutter shell migration and browser
correctness. Neither may starve the other.

1. Extend the landed physical viewport and pointer/wheel/keyboard path with IME,
   gesture, focus, scale, visibility, and lifecycle generations. In parallel, finish
   navigation-aware runtime/native-host cancellation and preserve one BrowserCore
   terminal outcome.
2. Move parser-discovered resources and supported DOM mutations onto the live
   document/runtime rather than creating compatibility state.
3. Harden and measure the implemented WebRender-to-RGBA texture transport while
   continuing font shaping/fallback,
   image decode, and WPT-driven layout/rendering work on the shared core.
4. Extend the landed bounded flat accessibility projection with hierarchy,
   relationships, richer actions, incremental updates, live regions/text
   selection, and native AT evidence. Add platform host-service UI; both remain
   cross-cutting through every later platform.
5. Produce hello-Flutter and Flutter+Vixen Linux size/performance baselines and a
   pinned, offline source-built Flatpak through `flatpak-flutter` 0.15.0. Adopt
   warning thresholds only after reviewed evidence; do not invent hard budgets.
6. Reach Linux parity, then remove Relm4/libadwaita/custom GLArea ownership. GTK
   may remain as a Flutter Linux embedder runtime dependency.
7. Expand the same bridge/chrome contract to macOS and Windows, with native
   texture, accessibility, host-service, packaging/signing, ABI, size, and
   performance proof.
8. Bring up Android with pinned V8 source/toolchain, GLES, lifecycle, input/
   accessibility, and split-ABI proof.
9. Widen the existing V8 WebAssembly path with the same API, resource-limit,
   malformed-module, and conformance proof on every declared target.
10. Bring up `aarch64-apple-ios-sim` with the same Flutter bridge, V8 JavaScript/
    WebAssembly, rendering, simulated lifecycle, input, accessibility, and host-
    service behavior. Physical iOS/TestFlight/App Store work requires a new ADR.

Shared browser work continues throughout: live document/runtime convergence,
resource loading, fonts/images/layout, downloads, network/security, storage,
WPT/CDP, real-site reductions, and reliability remain release-critical.

## Working rule

Every milestone lands with:

- one named authoritative owner and no new parallel state model;
- a browser-visible path through the shared core;
- focused unit tests plus an integration fixture/profile/smoke;
- stable, bounded diagnostics at trust and lifecycle boundaries;
- compatibility/limitation updates when observable behavior changes; and
- the cheapest focused checks followed by the relevant hk/`just` gate.

Prefer small, boring vertical slices. A large surface of plausible objects is
less valuable than one real navigation, document, render, or persistence path
that every frontend shares.
