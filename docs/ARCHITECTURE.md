# Vixen architecture

This document describes Vixen's implemented subsystem boundaries, the target
browser ownership model, data flows, and migration constraints. Product scope is
defined in [`PROJECT_DIRECTION.md`](PROJECT_DIRECTION.md); delivery order is in
[`ROADMAP.md`](ROADMAP.md); accepted tradeoffs are in
[`DECISIONS.md`](DECISIONS.md).

## Status language

Architecture documents can accidentally make planned integration sound landed.
This document uses three explicit states:

- **Implemented:** present in production code and exercised by a checked-in path.
- **Transitional:** present, but ownership or duplication must change before
  alpha.
- **Target:** the required end state; not a claim that it exists today.

Vixen now has a production `BrowserCore` behind the browser-scoped `vixen-api`
command/event seam. It owns one profile Store/network/cookie service, one
DOM/V8 owner thread, typed context/document/runtime/navigation generations,
asynchronous source loading, and bounded ordered events. Flutter, headless, CDP,
and WPT are adapters over that owner. This completes the current BrowserCore
ownership migration, not ADR-022's renderer transition or the broader alpha
compatibility exit gate.

Flutter is the target rendered frontend on Linux, macOS, Windows, Android, and
the Apple Silicon iOS Simulator.
Linux is the highest-priority GUI and release target: architecture integration,
host services, packaging, accessibility, and performance evidence converge there
first, then the same boundary expands to the other committed platforms.
The checked-in Linux Flutter runner and release archive require native Wayland;
X11/XWayland is rejected. Rendered automation uses the same executable in
chrome-less mode under bounded Cage/headless Wayland. Native `vixen-headless`
is intentionally text/runtime-only.

## Crates and responsibilities

| Crate | Current responsibility |
|-------|------------------------|
| `vixen-api` | Browser lifecycle and bounded renderer revision/mutation/commit/query/input/semantic contracts; no implementation dependencies |
| `vixen-net` | HTTP and URL/cookie/CSP/CORS/referrer/mixed-content/security policy |
| `vixen-store` | Bounded redb profile persistence and clear-data operations |
| `vixen-engine` | Sole BrowserCore owner for contexts, navigation, DOM/Page source, cascade, V8, history, input intent, resources, and accessibility meaning; no layout or paint backend |
| `vixen-cdp` | Bounded target/session/runtime adapter over a non-owning BrowserCore subscription and injected rendered backend |
| `vixen-ffi` | Safe one-owner controller, C ABI v1, renderer broker, in-host CDP composition, copied bounded JSON, and panic containment; no frame ABI |
| `vixen-headless` | Native text/runtime/profile CLI and non-rendered CDP test composition; rendered methods fail closed |
| `vixen-wpt` | Source-check manifest/runner/report schema; rendered checks remain in the schema but execute only in Flutter |

The packaged Linux composition root is the Flutter runner plus `vixen-ffi` into
one BrowserCore. There is no Rust GUI, native screenshot renderer, texture
fallback, or second browser core.

## Dependency direction

```text
Flutter renderer + chrome ─► vixen-ffi broker/controller ─┬─► vixen-api
                                                          ├─► vixen-cdp
                                                          └─► vixen-engine
                                                                 ├─► vixen-net
                                                                 └─► vixen-store

rendered automation/WPT ─► chrome-less Flutter host ─► same bridge/core
native text utilities ──► BrowserCore, with no invented geometry
vixen-wpt ───────────────► vixen-api
```

Rules:

- `vixen-api`, `vixen-net`, and `vixen-store` are implementation leaves.
- `vixen-wpt` depends only on `vixen-api` among Vixen crates.
- `vixen-engine` owns browser truth and renderer source generations, but no
  formatting, text measurement, hit testing, geometry, semantic bounds, or
  paint authority.
- Flutter owns formatter state, Paragraph/Canvas/scene output, exact geometry,
  hit testing, scroll mechanics, semantic bounds, and rendered capture. Public
  Flutter APIs run over explicitly enabled Impeller.
- A rendered CLI/CDP/WPT session is hosted by Flutter and has one BrowserCore.
  Native-only sessions may inspect source/runtime state but renderer-dependent
  operations fail closed.
- Pointer input crosses the C ABI only with an exact displayed-commit query and
  optional Flutter hit target. The old raw coordinate-input command is deleted.
- Renderer-dependent fixture checks use `flutter-js-eval`, `layout-box`,
  `visual-hash`, or `ref-equivalent`; the native WPT runner excludes them and the
  Flutter fixture host executes them.

R1–R7 are implemented. `just test-r7` enforces absence of WebRender/gleam,
`GlContext`, EGL, native frame/screenshot ownership, Rust layout/display-list/
paint modules, frame transport/texture presentation, and raw coordinate input.
`just gate-r7` composes this with the R5/R6 rendered fixture/CDP/Cage evidence.

## Authoritative ownership model (target)

```text
BrowserCore (one per open profile)
├── ProfileServices
│   ├── Store / schema / bounded writes / clear-data coordinator
│   ├── Network client, cookie jar, cache, HSTS, proxy/cert configuration
│   ├── Permission decisions and prompt broker
│   ├── Download manager
│   └── platform host services (paths, fonts, portals/native pickers, GPU diagnostics)
├── BrowsingContextRegistry
│   └── BrowsingContext (one per top-level tab; frames form a child tree)
│       ├── SessionHistory + bounded root/nested scroll restoration state
│       ├── NavigationController + active NavigationId/cancellation
│       ├── active DocumentState
│       │   ├── DOM + style data + invalidation
│       │   ├── JsRuntime realms/resources/event loop
│       │   ├── render-source revision + accepted atomic render commit
│       │   └── scroll/selection/accessibility semantic state
│       └── viewport, input, dialog, and context-scoped storage state
└── EventHub / diagnostics / inspector routing
```

### Ownership invariants

1. A profile is opened once by `BrowserCore`. Cookies, cache, localStorage,
   permissions, HSTS, download history, and durable settings are profile-owned.
2. Session history, sessionStorage, viewport/input, active navigation, runtime
   realms, and document state are browsing-context owned.
3. DOM, style, renderer source revision, atomic commit, presented scene, and
   runtime-visible page state identify the same committed `DocumentId`. A
   navigation cannot partially replace one layer.
4. Every asynchronous result carries the ids/generation it was created for.
   Results for a closed context, cancelled navigation, or replaced document are
   discarded before mutation or success notification.
5. Flutter owns formatting, paint, scene capture, chrome/widgets, Semantics
   presentation, and host-service UI over bounded revision/commit state. CDP may
   own sockets/session routing. Neither owns navigation, DOM, policy, or durable
   browser truth.
6. Flutter Canvas/Paragraph is the sole target web-content renderer. BrowserCore
   accepts only exact-revision atomic commits and remains the source of
   accessibility meaning; Dart does not infer semantics from pixels or retain a
   mutable DOM.

Stable ids distinguish at least profile, context/tab, frame, navigation,
document, request, runtime context, render revision/commit/node/fragment/resource,
remote object, and download. Use typed ids even when adapters serialize them.

## Threading and execution

`deno_core::JsRuntime` is `!Send + !Sync`, and the current DOM is `Rc`-backed.
Moving individual pages among arbitrary worker threads would add synchronization
without solving lifecycle ownership.

The execution model is one browser-core owner thread per open profile/process,
plus bounded external workers for sendable I/O and host work:

- all DOM, V8, history, navigation-commit, style invalidation, render-generation,
  and context-registry mutation runs there;
- network and blocking host operations may run externally, but return typed
  messages carrying context/navigation/request generations;
- the target Flutter renderer isolates/platform thread own formatting, Paragraph,
  Canvas/scene paint and capture, chrome, Semantics, and host-service
  presentation;
- CDP sockets and CLI orchestration may use Tokio tasks, but dispatch browser
  commands to the core and consume ordered events;
- renderer interaction observes document/render generations; returned geometry
  cannot commit browser state or target input from a stale snapshot.

The implemented core confines every `Page`, V8 isolate, history mutation,
document commit, and context-registry mutation to its named owner thread.
rusty_v8 enters isolates for their lifetime, so context/runtime generations are
retained in a bounded 512-slot arena and destroyed in reverse construction order;
commands temporarily enter older isolates through one localized V8 boundary.

Main-document source reads run on a bounded two-worker Tokio runtime. Each task
owns only sendable network/input data and an isolated cookie snapshot; completion
returns a typed context/navigation message and a cookie delta. Stop, supersede,
context close, and shutdown abort the task and invalidate its generation. The
owner checks the generation before applying cookies, parsing, writing profile
history, replacing the document/runtime, or emitting success. HTML parsing runs
as bounded owner-thread quanta, checks commands between quanta, and drops stale
parser state after stop or supersede. Runtime construction and page-script/
resource execution remain owner-thread work. Configured and parser-discovered
scripts advance one item per generation-checked quantum, followed by separate
DOMContentLoaded, load, and settle quanta. Individual V8 execution, promise
pumping, microtask checkpoints, and runtime-effect drains share a five-second
production watchdog. Timeout terminates V8, unwinds the job, cancels the
termination state, and joins the exact watchdog before another job can start, so
a late timeout cannot poison or terminate the next evaluation. The command-side
control registry snapshots the exact context runtime before enqueueing an
accepted navigate/reload/stop/close intent; it can terminate only that active
generation, not a replacement runtime created by the command. Interrupted work
discards deferred DOM mutations and runtime outputs, and the owner checks queued
commands before advancing another navigation quantum. Runtime `fetch()` and CORS
preflight network calls return through cancellation-polled worker channels. On
cancel, a worker-local signal wins against and drops the reqwest future, aborting
the active transport before the owner joins the worker. Cookie/preflight/HTTP-
cache writes remain outside the worker under the exact still-active runtime guard.
Runtime construction, other local native host calls, and discovered resources
beyond the first external stylesheet still need interruptible paths.

Parser-discovered external classic scripts and non-alternate external stylesheets
are the first post-commit resources on that worker model. The owner resolves the
URL and current script/style policy, then sends only network/profile-cookie data
to the existing bounded Tokio runtime. Manual redirect handling validates URL
policy, destination CSP, and active mixed-content policy before every hop and
does not buffer redirect bodies. Completion
carries context, navigation, document, runtime, and resource request ids plus an
isolated cookie delta. The owner rechecks every id and final HTTP status/`nosniff`
before exposing source, updating the bounded profile cache, applying style to the
Page cascade/runtime hosts, or resuming script work. Accepted cookie deltas apply to
the core, active runtime, and each current profile-store origin partition; delta-
against-current persistence preserves unrelated writes from other contexts and
makes accepted cookies visible after profile reopen. Stop, supersede, close, and
shutdown cancel the task and emit one bounded request/failure sequence; late
completions are inert. File documents and file scripts share one async reader that
checks the configured body limit both before allocation and while reading;
external file stylesheets use that reader as well.

The Rust GTK shell has been removed. Every frontend has one browser adapter (or factory-injected browser
handle), not an independent engine state machine per tab.

## Command and event seam

The implemented dependency-free `BrowserHandle`, `BrowserCommand`, and
`BrowserEvent` contracts establish typed routing for context/navigation/document/
request/runtime/download generations. They replace the removed tab-shaped
callback API with a browser-scoped seam whose concepts are:

- **Commands:** create/close/activate context; navigate/reload/stop/traverse;
  evaluate; dispatch input; query/snapshot; set viewport/emulation; answer a
  permission/dialog; start/cancel a download; clear profile data.
- **Events:** context/document created/destroyed; navigation requested/started/
  redirected/committed/cancelled/failed; DOMContentLoaded/load; URL/title/history/
  progress changed; request/response/failure; console/exception; dialog/
  permission/download; invalidation/frame-ready; diagnostic/profile-write error.
- **Queries/snapshots:** explicitly versioned, bounded views. Mutable behavior
  remains commands, not shared references into engine internals.

Every command and event names the relevant context and generation. Ordering is
defined on the engine thread; adapters may translate but not reorder lifecycle
within a context. Stable diagnostics and protocol errors are product contracts.
Evaluation and input results include the exact ordered cross-document navigation
ids they created. CDP stores those ids in per-request continuations and uses one
production event pump to settle page, target-creation, history, runtime, and input
requests while the socket remains readable. Earlier ids from one command are
consumed as superseded; disconnected or timed-out requests retain claimed
tombstones until their late terminal outcome can no longer be misattributed.
Configured initial-URL startup remains a pre-connect readiness barrier rather than
a concurrent event consumer.

Do not add a generic engine-selection abstraction. Vixen still has one engine
and one JS runtime. The seam isolates product frontends and thread ownership, not
alternate implementations.

### Flutter bridge status and target

The implemented `vixen-ffi::FlutterBrowserController` is deliberately non-clone.
It owns one `EngineBrowserHandle`, returns navigation acceptance without waiting
for settlement, and exposes one nonblocking or timeout-bounded ordered event
receiver. Its isolated handwritten C ABI module wraps that exact controller in a
process registry: no Rust reference crosses the boundary, all pointer inputs are
bounded and copied before parsing, no callbacks exist, output allocations are
released only by opaque token, and each delivered event receives a monotonically
increasing per-handle sequence. The crate builds `rlib`, `cdylib`, and `staticlib`
forms. `just gate-native-abi` is native ABI/header/wire evidence only.

The Dart FFI binding and worker isolate are transport adapters over the same
browser-scoped seam:

- opaque browser handles plus typed context/frame ids, explicit version
  negotiation and destruction, and no Rust references retained by Dart;
- checked owned buffers with one allocator/free contract;
- commands copied to BrowserCore and bounded ordered events copied to Dart with
  typed ids/generations;
- no synchronous Dart callback while Rust locks or V8 scopes are active;
- bounded mutation/commit/query channels plus explicit payload release; and
- stable structured errors rather than panic/exception-driven lifecycle flow.

A generated bridge is optional, not architectural. Adopt one only if its output,
ownership, platform build behavior, and artifact cost remain inspectable. The
Linux shell uses constructor-injected scripted tests without inventing production
browser state; production always uses the native worker and fails closed.

The target renderer protocol never calls Dart directly while Rust locks are held.
Ordinary rendering is asynchronous: BrowserCore publishes a base/target revision
batch, Flutter lays it out and paints it, then returns one atomic commit. A
dedicated request/response broker remains serviceable while the command worker or
V8 evaluation waits for `EnsureLayout`; the renderer cannot re-enter BrowserCore.
Cancellation, deadlines, and exact revision/commit checks prevent deadlock and
late mutation.

## Navigation and document commit

Target main-document flow:

```text
frontend/page intent
  → BrowserCommand::Navigate(context, intent)
  → assign NavigationId; cancel/supersede prior provisional work
  → normalize URL + navigation/sandbox/permission policy
  → profile loader: HSTS/cookies/cache/referrer/request metadata
  → network request and redirect loop (policy on every hop)
  → response security checks and content classification
  → provisional DocumentState
  → atomic commit: URL/origin/history/document/runtime generation
  → parse + parser scripts + discovered subresources
  → Stylo update → renderer mutation → atomic Flutter commit
  → DOMContentLoaded → load → settled diagnostics
```

Before commit, failure normally preserves the current document. After commit,
failure belongs to the new document/error-page lifecycle. Redirects keep one
navigation lineage but distinct request ids. The bounded network worker reports
each redirect to `BrowserCore` as it occurs; the core generation-checks it,
advances the active request id, and emits it before final response completion.
Request-start and final-response progress map to the existing navigation phases
rather than creating duplicate lifecycle events. Same-document history changes
keep the document id and update URL/history/scroll state through the same
controller.

Implemented `stop()` invalidates the active generation and aborts source
transport/body reads. Forced late completions are rejected before cookie,
profile, history, document, runtime, or event mutation. HTML parsing is also
generation scoped and cooperatively interruptible between bounded source quanta;
stop, reload, and history-traversal parser races prove stale work cannot commit.
Configured/author scripts and pending lifecycle stages are generation-scoped
quanta as well: stop/supersede suppresses unstarted items and later lifecycle
success. The target still extends cancellation inside individual runtime and
resource jobs.

## Document, runtime, and Web APIs

`Page` is the BrowserCore facade over parsed DOM, computed styles, accepted
resources, diagnostics, form/history state, runtime snapshots, and renderer
source projection. It owns no formatting, layout, hit testing, or paint state and
is not a profile/browser lifecycle coordinator.

JS uses `deno_core` directly:

- generated WebIDL describes interface/prototype shape;
- pure immutable/value APIs may be JS bootstrap code;
- stateful page/network/storage/security APIs cross narrow Rust ops/resources;
- validation and permission checks occur at the JS → Rust boundary and again at
  lower trust boundaries where necessary;
- resources carry document/context generations so navigation teardown revokes
  stale handles;
- parser scripts, modules, tasks, and microtasks join the document lifecycle.

The obsolete `Page` string-expression and headless classifier shims are deleted;
all evaluation adapters use `BrowserCore`/`JsRuntime`.

API surface alone is not support. Inert media/canvas/web-component objects may
help automation probes, but `COMPAT.md` must classify them as shape-only until
their observable subsystem behavior exists.

See [`RUNTIME_WEB_PLATFORM.md`](RUNTIME_WEB_PLATFORM.md) for host-module rules.

## Style, layout, paint, and inspection

The implemented production path is:

```text
BrowserCore DOM + Stylo computed styles + accepted resources/semantics
  → RenderMutationBatch(base_revision, target_revision)
  → Flutter Vixen formatter
       CSS box/anonymous trees + formatting/fragmentation
       dart:ui Paragraph/image measurement
       Canvas paint order/clips/transforms/compositing
       mechanical scroll geometry + hit testing
  → Flutter scene/layers
  → RenderCommit(commit_id, target_revision)
       immutable basic geometry index
       opaque Flutter-side hit-test handle
       bounded text/caret query handle
       scroll snapshot
       semantic bounds
  → Presented(commit_id)
```

### Source revisions and mutation recovery

`RenderRevision` includes context, document, source/style, viewport, and resource
generations. Every incremental batch names its exact base and target. Flutter
applies it only to that base; a gap requests a bounded full snapshot. Mutations
carry immutable styled render inputs, stable node/resource/semantic ids,
pseudo/generated content, scroll intent, and removals. They are not a second DOM.
Dart retains only bounded active generations/resources and releases superseded
payloads explicitly.

BrowserCore fetches and validates image/font bytes under URL/CSP/CORS/integrity/
cache policy before renderer exposure. Node count/depth, text, mutations,
resources, decoded bytes, fragments, commits, and queues are bounded.

The full-snapshot source projects renderable DOM elements with stable BrowserCore ids,
renderer-only text ids, resolved style properties, accepted PNG bytes, semantic
descriptors, and root scroll intent. Non-rendered metadata/script/style subtrees
still participate in DOM id allocation but are omitted from renderer payload.
R6 diffs this source deterministically into exact same-document mutation
batches and falls back to the full snapshot on first load, missed state, or
resync. R7 deleted the former Rust layout/display-list projection.

### Formatter and commit authority

Vixen implements web formatting semantics in Dart over Flutter primitives.
Ordinary widgets or Flutter Flex are not CSS. `dart:ui` Paragraph is authoritative
for shaping, fallback, bidi, line breaking, intrinsic text measurement, caret and
range geometry, and text hit testing. Canvas/scene APIs are authoritative for
paint order, clips, transforms, compositing, images, and capture.

`RenderCommit` atomically identifies a ready scene plus geometry, an opaque
Flutter-side hit-test handle, text query state, scroll state, semantic bounds,
and truncation. `Presented` is separate because input/accessibility must name
what is visible, not merely the newest layout. BrowserCore rejects stale or
mismatched commits before inspection, input, scroll events, or accessibility
publication.

Flutter returns immutable basic border/padding/content/fragment/clip/scroll/paint
geometry to BrowserCore. BrowserCore validates and queries it cheaply for common
synchronous DOM/CSSOM/CDP calls without reimplementing layout. Paragraph-specific
offset/caret/range/affinity operations use a bounded batched renderer query
service. Renderer-authoritative means Flutter computes every value; it does not
require an FFI round trip for every rectangle read.

### Dedicated renderer transport

R2 keeps renderer traffic outside serialized browser commands. BrowserCore-side
code publishes bounded asynchronous snapshot/mutation/handle-release updates;
Flutter submits bounded commit/presented/resync records. `EnsureLayout`, hit
tests, and Paragraph text queries alone occupy correlated in-flight request
slots. One mutex/condition queue atomically owns closure, deadlines, queue order,
and all pending slots, so polling does not free capacity before a response. C
output remains retained only by release token. The bridge can be shut down from
the Flutter/UI side to cancel requests and wake polls even if the command worker
is blocked. The small Dart service consumes records into the R3 formatter without
calling BrowserCore. Production now uses the service for one bounded R4
projection; source publication and submission draining remain control operations,
while renderer DTO payloads stay on the dedicated queues.

### Synchronous layout broker

For same-task mutation followed by geometry, BrowserCore flushes DOM/Stylo,
publishes the required mutation batch, posts `EnsureLayout(required_revision)` to
a dedicated Flutter renderer broker, and waits without holding browser mutexes.
The Flutter UI/renderer isolate must remain serviceable while the originating
command/V8 evaluation waits, cannot re-enter BrowserCore, and returns through a
separate response channel. Navigation, stop, close, shutdown, and deadline cancel
the wait. Late commits are inert.

R6 implements this with one Page shared by BrowserCore and its page realm. The
geometry op drains pending DOM mutations into that Page, refreshes cascade/source
state, publishes the exact batch, and waits through renderer state isolated from
the C controller lock. The normal Flutter shell runs a bounded broker pump on a
separate UI-isolate service tail, so a blocked command cannot block its own
renderer response. Basic geometry is read from the accepted commit; Range/caret
queries use its Paragraph handle. One full-resync retry handles timeout, reset,
missed state, and malformed commits without poisoning the next request.

### Input, scroll, semantics, and automation

Flutter hit-tests the displayed commit and returns commit/revision plus stable
node/fragment ids and finite coordinates. BrowserCore validates the target and
owns DOM dispatch, cancellation, and default-action policy. Flutter owns live
scroll offsets/extents/clips; BrowserCore sends scroll commands after
`preventDefault()` and owns script intent, DOM scroll effects, history restoration,
and persistence.

BrowserCore authors accessibility role/name/value/state/relationships/focus/
actions. Flutter supplies accepted semantic bounds/text geometry and publishes
Semantics only for the displayed commit. Actions return with exact commit and
advertised action generation.

The same renderer runs without chrome for screenshots, layout/visual WPT, and
rendered CDP. Linux hosts it in Cage/headless Wayland and captures an exact
presented scene without compositor chrome.

### Cutover invariant

R7 is complete. No fallback renderer or compatibility API may reintroduce
WebRender/EGL/frame transport, Rust layout/paint authority, native screenshots,
or raw coordinate input. Renderer-dependent evidence must use the same Flutter
formatter/commit path as the GUI. A pure shared algorithm may be added only
through an explicit stable formatter contract when measured reuse is simpler
than a direct Dart implementation.

## Resource loading, network, and policy

`vixen-net` owns pure transport/policy primitives. `BrowserCore`'s profile loader
combines them with document and profile context. One loader must serve main
documents, scripts, styles, images, fonts, fetch/XHR, frames, and downloads.

For every request:

1. derive source origin/partition, destination, credentials, referrer, CSP,
   sandbox, and permission context from authoritative state;
2. validate URL/method/headers/body and private-network policy;
3. apply HSTS, cookies, cache, redirect, mixed-content, CORS, and request metadata
   policy in a defined order;
4. stream transport with request id, destination-specific limits, progress, and cancellation;
5. apply response CORS/CORP/COEP/nosniff/integrity/content policy;
6. only then expose, execute, decode, persist, cache, or create a download.

Policy failure, transport/TLS failure, protocol failure, decode failure,
unsupported behavior, and cancellation have distinct stable diagnostics. CDP
and shell translate the same underlying event; they do not infer failures from
frontend-specific state.

## Profile and storage

One `Store` is opened per profile. The implemented schema includes bounded
records for:

```text
profile.redb
  cookies
  fetch-cache
  history
  session
  web-storage
  downloads
  permissions
  hsts
downloads/
reports/
```

The filename and XDG/app-ID paths are selected by the composition/host service;
partition keys are produced by the engine/network layer and remain opaque to
`vixen-store`.

Before adding a durable table, define:

- engine owner and partition key;
- record and total-table limits plus eviction behavior;
- transaction/failure/recovery semantics;
- clear-data category and session-restore interaction;
- private/ephemeral profile behavior; and
- observability without leaking sensitive content.

Downloads, favicons/icons, settings, credentials/autofill, and future IndexedDB/
Cache Storage require purpose-built bounded schemas, not generic JSON dumping.

## Platform host services

Platform compatibility is an engine input, not shell trivia. Small native host
services provide:

- certificate roots and custom CA configuration;
- proxy/environment policy;
- bounded system/bundled/web-font descriptors and accepted bytes for Flutter's
  Paragraph font collection; BrowserCore retains web-font fetch/policy/cache;
- platform data/cache/config/download directories scoped by app id;
- Flatpak portals or native pickers/services for file access, downloads,
  permissions, and external opens;
- Flutter engine/Impeller backend and driver capability diagnostics as
  applicable; and
- safe file/download destination validation.

Path discovery may remain platform code, but profile state ownership stays in
`BrowserCore`. Flutter owns prompt/dialog presentation, not policy or durable
decisions. All host failures produce structured diagnostics usable by GUI error
pages, chrome-less renderer output, CDP, and smoke reports.

## Trust boundaries and limits

Web content and protocol clients are untrusted. Validate as close as possible to
entry, then preserve typed validated data internally.

| Boundary | Owner | Required behavior |
|----------|-------|-------------------|
| CLI/CDP/GUI command → core | adapter + `vixen-api` DTO validation | Validate ids/options/sizes; reject unknown/stale targets with stable errors |
| navigation/resource request | browser loader + `vixen-net` | URL/private-network/header/body/policy checks on initial request and redirects |
| HTTP response → page/profile | browser loader | CORS/security/integrity/content checks before exposure, execution, decode, cache, or persistence |
| JS → Rust op/resource | runtime host module | WebIDL conversion, size/permission/origin checks, document-generation validation |
| DOM/style generation → Flutter mutations | BrowserCore + FFI | Exact base/target revision, bounded immutable data, known resources, deterministic resync |
| Flutter commit/query → browser inspection/input | renderer bridge + BrowserCore | Exact revision/commit, bounded finite geometry/ranges, known node/resource ids, reject stale or truncated-required answers |
| profile write/read | profile service + `vixen-store` | Partitioned normalized records, bounds, transactional failure diagnostics |
| file/portal/download | platform host service + download manager | Approved roots/handles, safe names, no ambient arbitrary write/open |
| inspector/snapshot | engine inspector | Bounded output; explicit update or stable stale-state error; no alternate model |

Content-controlled queues and data need explicit caps: redirects, headers/body,
DOM nodes/depth, parser/script work, runtime handles, events/microtasks, decoded
images/fonts/media, cache/profile records, downloads, traces, console/diagnostic
buffers, snapshots, and protocol output. On limit breach, fail deterministically
without exposing partially accepted unsafe state.

## Diagnostics and observability

Observability is a product contract, not debug residue:

- lifecycle events name context/navigation/document/request ids;
- stable error codes separate policy, transport, protocol, unsupported,
  cancellation, stale-state, resource-limit, renderer/runtime reset, and profile
  failure;
- traces and logs are bounded and privacy-minimal by default;
- Flutter GUI, chrome-less rendered automation, CDP, WPT, and real-site reports
  translate the same engine and renderer-generation events;
- no adapter may require page text, JS expressions, credentials, form values, or
  full headers in a default trace.

## Verification and reduction architecture

Evidence layers share production paths:

1. leaf-unit tests for pure policy/data/formatting algorithms;
2. engine integration tests for ownership, lifecycle, generations, and profile
   partitioning;
3. committed local fixtures for focused regressions;
4. pinned imported WPT profiles with source×category reports;
5. Flutter GUI/chrome-less-host visual comparisons and external Playwright/CDP
   smokes;
6. controlled real-site/platform-host corridor reports; and
7. fuzz, audit, performance, memory, size, restart, and recovery gates.

Classify a real-site failure as navigation/network/security, DOM/runtime,
style/layout/paint, storage/profile/download, media/accessibility, shell/platform,
automation/inspection, or reliability/performance. Reduce it to the lowest layer
that reproduces the production path. If it cannot yet be reduced, retain exact
commands, platform, artifacts, and classification rather than a vague issue.

## Build profile

The release profile remains:

```toml
[profile.release]
strip = true
lto = "thin"
codegen-units = 1
panic = "abort"
```

`lto = "fat"` is a measurement experiment, not the default. After cutover,
structured size commands measure the GUI, chrome-less Flutter host, and any
text-only launcher separately. Hard budgets
must be based on published reproducible baselines for the active
`deno_core`/V8/Flutter dependency graph. Pre-R7 measurements include deleted renderer costs and must be labeled historical.

Each Flutter GUI release uses release/AOT/strip/LTO controls and a per-platform/
ABI hello-Flutter versus Flutter+Vixen report with component attribution. Debug
engines/symbols, duplicate ABIs, headless tools, build tools, and caches do not
belong in GUI bundles. Warning and hard thresholds follow the evidence policy in
`BASELINES.md`; no numerical Flutter budget exists yet.
