# Roadmap

This is the delivery sequence from the current component-rich prototype to the
full project goal: a credible Firefox replacement with one focused Flutter shell
targeting Linux, macOS, Windows, Android, and the Apple Silicon iOS Simulator, plus first-class headless/CDP
automation. It is deliberately more ambitious than a demo-browser plan, but it
does not turn framework support into an unsupported Vixen compatibility claim.

**Linux is the highest-priority GUI and release target throughout this
roadmap.** Browser correctness remains shared-core work, while GUI integration,
host services, accessibility evidence, packaging, and performance converge on
Linux first. Non-Linux targets remain committed and follow the contract proven
by the Linux gates; they do not delay Linux parity.

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

As of 2026-07-14 the repository has these building blocks:

- A seven-crate workspace, hk/`just` development gates, stable diagnostics, fuzz
  targets, and a WPT/fixture harness. The committed manifest currently measures
  **270 fixtures / 2,027 checks at 100%**; `COMPAT.md` owns the detailed counts.
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
- Flutter is the sole rendered GUI. The packaged Linux composition root provides
  chrome, real BrowserCore FFI, visible WebRender output through a bounded RGBA texture,
  input/accessibility projection, and a deterministic release/AOT archive with
  clean extraction and Impeller Cage/headless-Wayland launch smoke. The Linux
  GUI now rejects X11/XWayland. One controlled native Wayland vertical covers
  physical chrome navigation, back/forward/reload/active stop, restored root/
  nested scrolling, and IBus input; FlatPark review, broader IME/device evidence,
  host services, complete accessibility, and Flutter parity remain open.

These are substantial components now routed through one initial browser owner,
not yet a broadly compatible browser. API shape or inert reflection is still not
counted as implemented behavior when the underlying subsystem does not exist.

## The critical gap

The production path is the browser-scoped `BrowserHandle` seam. The Flutter FFI
controller, headless CLI, CDP targets, and WPT adapter create contexts in one
`vixen-engine::browser::BrowserCore`;
they no longer own parallel `Page`, `JsRuntime`, network, cookie, history, or
profile-session state. CDP has one core context/runtime generation per target,
with bounded generation-scoped remote handles.

The largest remaining delivery risk has moved from frontend ownership
duplication to the live document lifecycle. HTML parsing, configured/author
script items, and terminal lifecycle stages now yield cooperatively on the core
owner. Accepted navigate/reload/stop/close commands snapshot the exact active
runtime generation and interrupt its V8 execution, promise pumping, or runtime
fetch/CORS worker wait before the bounded deadline; the isolate unwinds and
remains reusable. A cancelled fetch/CORS worker drops its in-flight reqwest
future, closes the peer transport, joins, and cannot commit cookies or cache state.
Runtime construction, other local native host ops, and non-script discovered-
resource fetches beyond the first external stylesheet are not yet interruptible,
and compatibility projections still coexist with the live page/runtime. Parser-
discovered external classic scripts and `<link rel="stylesheet">` use generation-
cancellable worker I/O rather than blocking the owner. Adding broad API shape
before converging those paths would preserve plausible but disconnected state.

Other material gaps remain:

- main-document source loading is asynchronous; HTML parsing, configured/author
  script items, and DOMContentLoaded/load/settle stages are generation-checked
  owner-thread quanta. Exact-generation V8 jobs and runtime fetch/CORS worker
  waits are deadline-bounded and navigation-interruptible; runtime construction
  and other local native host calls are not yet interruptible. External classic-
  script and first external-stylesheet file/HTTP reads are cancellable worker
  tasks; image/font and other discovered-resource fetches remain synchronous or
  absent;
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

**Current product-priority gate:** do not spend delivery time on FlatPark
submission, review, or publishing until the Linux Flutter path is a basic usable
browser. The gate requires visible controlled-site navigation, engine-owned
scrolling, pointer/keyboard plus text/IME input, back/forward/reload/stop,
find/zoom, and bounded navigation/runtime/surface failure recovery. Keep the
deterministic release archive healthy as build evidence, but package-registry
availability does not outrank browser behavior.

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

Progress as of 2026-07-14: A1 is routed through the dependency-free typed
`vixen-api` command/event seam and one `vixen-engine::browser::BrowserCore`.
BrowserCore owns one engine thread, profile Store/network/cookies, bounded
context/runtime registries, history, evaluation, inspection, and paint inputs.
WPT, headless CLI, CDP, and the Flutter FFI controller are thin adapters over
that owner; multi-context tests prove independent globals/sessionStorage/history
with intended profile sharing. The 2,027-check fixture manifest, controller
tests, and external Playwright smoke remain green.

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
failed/timed-out evaluations discard their deferred DOM mutation sink before the
reusable realm accepts later work, and the committed navigation still settles.
Successfully queued navigate/reload/stop/close commands snapshot the exact
current context/runtime generation before enqueue, terminate active V8 work,
suppress interrupted-job mutations/effects, and make the owner drain queued
commands before advancing another navigation quantum. Focused infinite-script
tests prove stop returns before the watchdog, emits one cancellation without a
spurious runtime exception, suppresses the next script, and leaves the isolate
reusable.
Runtime `fetch()` and CORS preflight still expose a synchronous op internally,
but network work now returns through a cancellation-polled worker channel. On
interruption a worker-local signal drops the in-flight reqwest future and the
owner joins the worker; only a still-active exact runtime may commit returned
cookie, preflight-cache, or HTTP-cache effects. Gated fetch and preflight tests
prove stop returns while the server is blocked, the peer observes disconnect
before responding, one cancellation terminalizes, and the isolate remains reusable.
Configured and parser-discovered
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
turn a committed document into a failed navigation. Runtime construction, other
local native host cancellation, and image/font resource loading remain the next
A2 boundary. Parser-discovered non-alternate external stylesheets now run before
author scripts through the same bounded text-resource worker as external classic
scripts. File/HTTP loads carry exact request and document/runtime generations,
recheck `style-src` and active mixed content at redirects, require successful and
`nosniff`-compatible responses, then commit cookies, the bounded profile cache,
the document-ordered Page cascade, refreshed runtime hosts, layout, and paint.
The checked-in file fixture proves visible pixels; gated HTTP and supersede tests
prove ordered settlement and no late cookie/cache/style commit. Dynamic links,
alternate sheets, broad link-media queries, `@import`, SRI, and full CSSOM sheet
objects remain open.

The obsolete fail-closed `Page` string-expression path and headless test-only
classifiers are deleted; all evaluation adapters use `BrowserCore`/`JsRuntime`.
Runtime/document snapshots still need convergence with the live page state.

Cross-cutting delivery work has also advanced without widening browser claims.
The safe Flutter controller now has a versioned handwritten C ABI with opaque
process tokens, bounded JSON messages and retained output/frame allocations,
stable tagged responses/events/errors, polling-only event delivery, and panic
containment. The Linux shell now uses locked Yaru 10.2.0 Adwaita-blue themes,
icons, controls, and an in-scene native-window titlebar containing the tab strip;
the GTK headerbar remains a hidden startup fallback. It also adds handwritten Dart bindings, deterministic fake
tests, a production worker isolate, bounded RGBA `FlPixelBufferTexture`
transport, physical viewport mapping, and generation-checked pointer/wheel/key
dispatch through BrowserCore hit testing, including matching-generation primary-
press cancellation. A monotonic BrowserCore host-view state now carries bounded
viewport/effective scale, content focus, visibility, and Flutter lifecycle;
stale updates fail, inactive views reject input, and live documents observe
focus/visibility state and events across navigation. One explicit bounded viewport
transform now carries Flutter's effective device scale into BrowserCore: CSS
layout and runtime viewport state divide the physical target by that scale,
while paint, hit testing, wheel deltas, texture presentation, and Semantics use
the matching physical projection. A 2.0-scale core/widget test covers visible
paint, input, runtime DPR, and accessibility geometry without Dart-side node
repair. Full lifecycle/native surface recovery remains. Current-generation
frame and Semantics capture failures now retry twice with exact keys, and texture
create/publish failures dispose and recreate the controller twice before a
recovery-failed placeholder; newer frames get a fresh bounded attempt.
Uncanceled wheel events now apply a clamped Page-owned root scroll offset; the translated layout drives
paint, hit testing, selector/accessibility bounds, and fixed-position anchoring.
Unmodified Arrow, Page Up/Down, Home/End, and Space defaults now use the same
zoom-derived CSS viewport and Page offset; page `preventDefault()` cancels the
action and focused native/editing controls retain their key handling. Flutter
single-touch drags now cross platform touch slop, cancel the pending synthetic
press, and reuse that cancelable physical-delta root path. Page-owned nested
scrollports now share input/script offsets, clipped geometry, and element scroll
events. Session-history entries capture the root plus at most 1,024 stable-
identity element offsets; `auto` restores them on reload/back/forward while
`manual` suppresses restoration, and the live runtime is resynchronized. DOM
touch/pointer events, inertia/multi-touch, and smooth scrolling remain. Live page
scripts now use the same clamped offset through numeric/options
`scroll()`/`scrollTo()`/`scrollBy()`,
synchronized window offsets, and root/body `scrollTop`/`scrollLeft`; host-view
and page-zoom changes refresh the live CSS viewport and overflow clamp. Actual
top-level changes from script, uncanceled input defaults, find traversal,
viewport clamps, and zoom clamps now emit a non-cancelable bubbling document
`scroll` event after the current script evaluation with synchronized offsets;
canceled and clamped no-ops stay silent. A bounded,
mutation-generation-tagged
BrowserCore projection now maps roles/names/states/bounds and tap/focus into Flutter
Semantics. Nearest emitted semantic-parent relationships now produce a validated,
document-order nested Flutter hierarchy. Focus actions are exact source/wire-
generation checked and execute through the live runtime. Bounded set-value uses
the same checks and live native text-control event path. The first bounded
non-tree relationship maps retained `aria-controls` targets to stable Flutter
semantic identifiers. Enabled native range inputs expose numeric state and
exact-generation increase/decrease actions through the live runtime. Additional
bounded `aria-describedby` and `aria-details` relationships plus resolved
descriptions now cross the same projection; descriptions map to Flutter hints.
Authored ARIA sliders/spinbuttons with finite numeric state now expose bounded
range values and exact-generation arrow-key adjustments through the live
runtime. Explicit and implicit live regions now map into Flutter, and active
runtime-effect events force a new frame/full-semantics pair despite unchanged
document keys. Focused writable native text controls and direct contenteditable
editing hosts now project live UTF-16 selection offsets through Flutter
Semantics. Semantic deltas, general document-range selection, long-tail authored-range/relationship
mappings, and native AT remain. Bounded `aria-owns` now re-parents retained
later nodes, and heading levels plus mixed checkbox state use dedicated Flutter
semantics properties. Same-document refreshes atomically replace the paired
frame/projection and content-sensitive node keys retain unchanged platform
semantics identities; the bounded ABI remains full-snapshot rather than making
Dart own a delta graph. The real Linux release bundle now has process-filtered,
bounded AT-SPI evidence that the BrowserCore-derived `DOM Basic` heading reaches
the native tree; broader screen-reader/platform matrices remain.
A focused writable native text control or direct contenteditable editing host
now attaches Flutter's platform `TextInputClient`; bounded full text plus UTF-16
selection and composing ranges cross exact context/document/runtime ids and
update the live DOM with composition-shaped plus `beforeinput`/`input` events.
The semantic projection now distinguishes multiline hosts and carries normalized
standard `inputmode`, supported input-type, and `enterkeyhint` intent. Flutter
maps those values to the corresponding platform keyboard/action configuration,
then routes platform actions through exact-generation Enter down/up dispatch.
The release-process Wayland smoke now enters a controlled URL through chrome,
uses native back/forward/reload with restored root/nested offsets, cancels a
FIFO-gated active file navigation through the visible stop control, requires the
prior page to recover, and retains IBus Anthy composition evidence. Broader native
navigation, IME/device, restoration-event, and real-site matrices remain.
Ctrl+F now crosses the exact active context/document ABI boundary. Page owns a
10,000-match-bounded rendered-text result and one-based active match; Enter/F3
plus Previous/Next traverse with wrapping and move the shared root offset to
reveal the match before Flutter refreshes the paired frame/Semantics projection.
Active orange and other yellow range highlights enter the same display list
before its text runs; horizontal precision currently shares the deterministic
text metric and improves with font shaping. Soft-wrapped phrases remain one
logical match with a highlight fragment on each intersected text run.
Per-context 25–500% page zoom now remains BrowserCore-owned: it derives a CSS
viewport, scales the single display list into the physical frame, converts
physical input back to CSS coordinates, and projects Semantics bounds through
the same transform. Profile persistence and native surface-loss evidence remain.
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
Exact-generation navigate/reload/stop/close commands now interrupt active V8
jobs and abort runtime fetch/CORS transport before the watchdog without emitting
a page exception or accepting late persistence. Runtime construction, other
local native host calls, and image/font resource loading remain. The first
parser-discovered external stylesheet shares the cancellable external text-
resource path, applies to the live cascade before author scripts, and rejects
late persistence/application after stop or supersede.

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
- Flutter, headless, CDP, and WPT are adapters over it;
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
- Linux FlatPark install/update, certs, fonts, portals, downloads, GPU, settings,
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

The ownership, cancellation, scrolling, and local measurement foundations are
landed. To improve implementation velocity, keep at most **two active slices**:
one Linux Flutter usability slice and one shared-core correctness slice. Finish,
document, and commit a slice before widening its lane. An environment-blocked GUI
slice may yield to the next shared-core slice, and vice versa; it does not open a
third lane.

### Active acceleration queue

Work top-to-bottom within each lane. Each item names the smallest useful proof;
it is not permission to implement the whole subsystem in one batch.

**Shared-core lane**

1. **Shape one text/fallback vertical.** Replace deterministic metrics for one
   common script/fallback case through layout, paint, hit testing, find, and
   Semantics together; add a focused fixture before broad font-platform work.

**Linux Flutter lane**

1. **Widen native interaction evidence.** Add the next highest-value IME/device
   case and restoration-event/gesture fidelity, then broaden AT/screen-reader
   actions. Keep each language, device, or relationship mapping independently
   reviewable.

### Ordered horizon after the active queue

1. Complete loader breadth, DOM/runtime convergence, font/image/layout coverage,
   downloads, host services, accessibility, WPT/CDP reductions, and reliability
   from measured corridor failures.
2. Reproduce Linux release size/performance baselines after the basic-browser
   smoke is green; add warning thresholds only after reviewed measurements.
3. Resume FlatPark submission/review/publishing only after the Linux usability
   gate, host services, and release evidence pass.
4. Expand the same bridge/chrome contract to macOS and Windows with native
   texture, accessibility, host-service, packaging/signing, ABI, size, and
   performance proof.
5. Bring up Android with pinned V8 source/toolchain, GLES, lifecycle, input/
   accessibility, and split-ABI proof.
6. Widen WebAssembly with the same API, resource limits, malformed-module, and
   conformance proof on every declared target.
7. Bring up `aarch64-apple-ios-sim` with the same Flutter bridge, V8 JavaScript/
   WebAssembly, rendering, simulated lifecycle, input, accessibility, and host-
   service behavior. Physical iOS/TestFlight/App Store work requires a new ADR.

## Velocity policy

- **Cap work in progress.** One active slice per lane; do not start adjacent API
  breadth while its state owner, cancellation path, or visible proof is open.
- **Reduce before abstracting.** Start from one controlled failure/fixture and
  reuse existing owners. Generalize only when a second landed consumer exposes
  real duplication.
- **Keep one failure domain per commit.** Split at a new trust boundary, state
  owner, platform dependency, or independently revertible behavior. A coherent
  follow-up tock is allowed; unrelated cleanup is not bundled.
- **Use the test ladder once.** Run focused checks during editing, the relevant
  slice gate before commit, and `just gate-push` once before pushing the coherent
  batch. Do not repeatedly run release/container gates for Rust-only inner loops.
- **Prefer executable evidence over status prose.** A fixture, race test, native
  smoke, or measured report moves an item. API shape, screenshots without a
  reduction, and framework capability do not.
- **Keep handoffs cheap.** Every commit updates compatibility/limitations when
  behavior changes and leaves the next smallest widening step in this queue.
  Delete completed queue text instead of accumulating another landed inventory.

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
