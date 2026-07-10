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
asynchronous source loading, and bounded ordered events. Shell, headless, CDP,
and WPT are adapters over that owner. This completes the A1 ownership migration,
not the broader alpha compatibility exit gate.

## Crates and responsibilities

| Crate | Implemented responsibility | Target boundary |
|-------|----------------------------|-----------------|
| `vixen-api` | Browser-scoped typed lifecycle ids, command/event/error/handle contracts, transitional Engine/delegate/inspector traits, diagnostics, profile configuration, graphics-context trait, DTOs | GUI/protocol-neutral command, event, id, snapshot, and factory contracts; no implementation dependencies |
| `vixen-net` | HTTP client primitives and URL/cookie/CSP/CORS/referrer/mixed-content/permissions/security policy | Pure network and policy leaf; no DOM, runtime, GTK, or profile orchestration |
| `vixen-store` | Bounded redb profile tables and clear-data operations | Persistence leaf using opaque partition/id keys; no network or UI policy |
| `vixen-engine` | Initial production BrowserCore/thread/profile/context lifecycle, HTML, DOM/Page, Stylo integration, V8 host runtime, forms/history, Vixen layout, display list, WebRender integration | Sole owner of browser/profile/context/document/navigation/resource lifecycle |
| `vixen-shell` | Relm4/libadwaita chrome, GLArea surface, one app-level engine worker, BrowserCore context/session routing | Thin GUI adapter and host-service provider; no independent loader/history/profile model |
| `vixen-headless` | BrowserCore-backed CLI, CDP target/session adapter, interaction adapter, EGL surfaceless surface | Thin CLI/CDP adapter and composition root over the browser core |
| `vixen-wpt` | Fixture/profile manifest, runner, reports, checks, visual evidence | Engine-consumer test adapter; no engine internals or alternate semantics |

The thin root `vixen` binary becomes the GUI composition root; today it only
delegates to `vixen-shell`. `data/` and `build-aux/` contain application metadata
and Flatpak packaging; `fixtures/` contains the hermetic compatibility suite and
external-profile descriptors.

## Dependency direction

### Stable target

```text
GUI root ───────┬──► vixen-shell ──────────────► vixen-api
                └──► vixen-engine ─────────────► vixen-api
                         ├──────────────────────► vixen-net
                         └──────────────────────► vixen-store

vixen-headless ───────► vixen-api + vixen-engine
  (CLI/CDP composition root; dev-dep on vixen-wpt)

vixen-wpt ────────────► vixen-api
```

Rules:

- `vixen-api`, `vixen-net`, and `vixen-store` are leaves with no dependencies on
  other Vixen implementation crates.
- `vixen-wpt` may depend only on `vixen-api` among Vixen crates.
- `vixen-engine` is the only crate that combines network, persistence, DOM,
  runtime, layout, and paint behavior.
- Composition roots may construct `vixen-engine`, but adapters use its browser
  core rather than directly combining `Page`, `Network`, `Store`, or `JsRuntime`.
- GTK/Relm4 types stay in `vixen-shell`; EGL/CLI/CDP types stay in
  `vixen-headless`; neither leaks into engine state.

### Current migration status

- `vixen-shell` depends only on `vixen-api` and `vixen-engine` among Vixen
  crates. One app-level worker owns one `ShellBrowser`; tabs retain typed ids and
  immutable presentation snapshots only.
- `vixen-headless` depends only on `vixen-api` and `vixen-engine` in production.
  CLI and CDP targets route through BrowserCore and do not own `Page`,
  `JsRuntime`, cookies, network, or session history.
- `vixen-api::Engine` remains a transitional tab-shaped trait with only a test
  implementation. Production paths use the browser-scoped `BrowserHandle` seam.

`just gate-architecture` now enforces these frontend boundaries in addition to
the stable leaf-crate rules. Remaining migration debt is inside the document
implementation: compatibility projections and synchronous parser/script work
must converge on the live document lifecycle.

The committed/external WPT adapter is no longer an exception: it creates
BrowserCore contexts and uses generation-checked snapshot, selector, style,
diagnostic, evaluation, display-list, reference-render, and paint-snapshot
queries. The 269-fixture manifest therefore exercises the production owner while
`vixen-wpt` itself remains engine-independent.

The headless CLI `--eval`, screenshot, selector, textual DOM/layout/paint
projections, interaction summaries, and URL-only paths also create one
ephemeral-profile BrowserCore context and wait for the matching typed navigation
terminal event. Evaluation, inspection, hit testing, focus/form projections, and
paint snapshots are generation checked; EGL/PNG and JSON formatting remain
adapter-owned presentation work.

CDP maps every target to a BrowserCore context, keeps only bounded protocol
presentation/session/remote-handle state, and retains events by target while
waiting. The GTK shell uses the same context/history/paint and profile-session
commands through one app-level worker. Host-runnable multi-context tests cover
both adapters; the Flatpak build remains the supported GTK SDK proof.

## Authoritative ownership model (target)

```text
BrowserCore (one per open profile)
├── ProfileServices
│   ├── Store / schema / bounded writes / clear-data coordinator
│   ├── Network client, cookie jar, cache, HSTS, proxy/cert configuration
│   ├── Permission decisions and prompt broker
│   ├── Download manager
│   └── Linux host services (paths, fonts, portals, GPU diagnostics)
├── BrowsingContextRegistry
│   └── BrowsingContext (one per top-level tab; frames form a child tree)
│       ├── SessionHistory
│       ├── NavigationController + active NavigationId/cancellation
│       ├── active DocumentState
│       │   ├── DOM + style data + invalidation
│       │   ├── JsRuntime realms/resources/event loop
│       │   ├── layout tree + scroll/hit-test/selection state
│       │   └── display list + renderer-facing state
│       └── viewport, input, dialog, and context-scoped storage state
└── EventHub / diagnostics / inspector routing
```

### Ownership invariants

1. A profile is opened once by `BrowserCore`. Cookies, cache, localStorage,
   permissions, HSTS, download history, and durable settings are profile-owned.
2. Session history, sessionStorage, viewport/input, active navigation, runtime
   realms, and document state are browsing-context owned.
3. DOM, style, layout, paint, and runtime-visible page state identify the same
   committed `DocumentId`. A navigation cannot partially replace one layer.
4. Every asynchronous result carries the ids/generation it was created for.
   Results for a closed context, cancelled navigation, or replaced document are
   discarded before mutation or success notification.
5. Frontends own presentation and transport only. The shell may own widgets and
   CDP may own sockets/session routing, but neither owns browser truth.

Stable ids should distinguish at least profile, context/tab, frame, navigation,
document, request, runtime context, remote object, and download. Use typed ids in
Rust even when protocol adapters serialize strings or integers.

## Threading and execution

`deno_core::JsRuntime` is `!Send + !Sync`, and the current DOM is `Rc`-backed.
Moving individual pages among arbitrary worker threads would add synchronization
without solving lifecycle ownership.

The execution model is one browser-core owner thread per open profile/process,
plus bounded external workers for sendable I/O and host work:

- all DOM, V8, history, navigation-commit, style/layout invalidation, and
  context-registry mutation runs there;
- network and blocking host operations may run externally, but return typed
  messages carrying context/navigation/request generations;
- the GTK main thread owns widgets and GLArea callback integration;
- CDP sockets and CLI orchestration may use Tokio tasks, but dispatch browser
  commands to the core and consume ordered events;
- renderer interaction observes document/display-list generations and cannot
  commit browser state from a stale frame.

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
history, replacing the document/runtime, or emitting success. Parsing, runtime
construction, and page-script/resource execution are still synchronous owner-
thread work and need later cooperative cancellation/checkpoints.

ADR-010's Relm4 component model remains useful, but its one-engine-worker-per-tab
ownership is superseded by ADR-017. The shell should have one browser adapter (or
factory-injected browser handle), not one independent engine state machine per
tab.

## Command and event seam

The implemented dependency-free `BrowserHandle`, `BrowserCommand`, and
`BrowserEvent` contracts establish typed routing for context/navigation/document/
request/runtime/download generations. The current `Engine` trait remains a
transitional shell-facing API. It is too
tab-shaped to own a shared profile and too callback-shaped to represent multiple
concurrent navigations safely. Evolve it or replace it with a browser-scoped seam
whose concepts are:

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

Do not add a generic engine-selection abstraction. Vixen still has one engine
and one JS runtime. The seam isolates product frontends and thread ownership, not
alternate implementations.

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
  → style/layout/display-list updates
  → DOMContentLoaded → load → settled diagnostics
```

Before commit, failure normally preserves the current document. After commit,
failure belongs to the new document/error-page lifecycle. Redirects keep one
navigation lineage but distinct request ids. Same-document history changes keep
the document id and update URL/history/scroll state through the same controller.

Implemented `stop()` invalidates the active generation and aborts source
transport/body reads. Forced late completions are rejected before cookie,
profile, history, document, runtime, or event mutation. The target extends the
same cancellation through parser/resource/runtime jobs and pending lifecycle
work; those owner-thread phases are not yet cooperatively interruptible.

## Document, runtime, and Web APIs

`Page` is the implemented facade over parsed DOM, computed/style data, focused
layout, display-list, diagnostics, form/history state, and runtime snapshots. It
becomes the document-state implementation behind `BrowserCore`; it is not itself
a profile/browser lifecycle coordinator.

JS uses `deno_core` directly:

- generated WebIDL describes interface/prototype shape;
- pure immutable/value APIs may be JS bootstrap code;
- stateful page/network/storage/security APIs cross narrow Rust ops/resources;
- validation and permission checks occur at the JS → Rust boundary and again at
  lower trust boundaries where necessary;
- resources carry document/context generations so navigation teardown revokes
  stale handles;
- parser scripts, modules, tasks, and microtasks join the document lifecycle.

`Page::evaluate_dom_expression` and snapshot host objects are compatibility
bridges. Do not widen them. Move supported behavior to the live DOM/runtime and
delete each replaced projection so there is one answer for GUI, script, CDP, and
WPT.

API surface alone is not support. Inert media/canvas/web-component objects may
help automation probes, but `COMPAT.md` must classify them as shape-only until
their observable subsystem behavior exists.

See [`RUNTIME_WEB_PLATFORM.md`](RUNTIME_WEB_PLATFORM.md) for host-module rules.

## Style, layout, paint, and inspection

Implemented rendering path:

```text
html5ever DOM
  → Stylo-compatible document/selector and computed-style integration
  → Vixen layout tree and focused formatting algorithms
  → layout fragments
  → one Vixen display list
  → one WebRender paint path
  → GTK GLArea or headless EGL surfaceless GlContext
```

The path is intentionally singular, but its current formatting and text metrics
are narrow. The target keeps the same ownership shape while adding full Stylo
computed values, font discovery/shaping/fallback, images/replaced elements,
common formatting contexts, scroll/hit-test state, compositing, animation, and
incremental invalidation.

Rules:

- DOM/style mutation marks explicit dirty state; layout and paint consume it by
  document generation.
- No post-pass geometry fixup may hide incorrect authoritative layout data.
- GUI, headless screenshot, visual fixtures, hit testing, geometry APIs, and CDP
  inspect the same fragments/display list.
- Inspection may request a bounded style/layout update or return a stable error.
  It must tolerate stale state and cannot maintain a second DOM/layout tree.
- `GlContext` abstracts host surface binding only. There is no second paint
  backend or CPU renderer.

Vixen-owned layout follows ADR-013: data-oriented arenas, stable ids, explicit
invalidation, cached intrinsic values, and formatting-context passes, with
Ladybird as an architecture reference and WPT/ref tests as behavioral evidence.

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
4. stream transport with request id, limits, progress, and cancellation;
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

## Linux host services

Modern-Linux compatibility is an engine input, not shell trivia. Small host
services provide:

- certificate roots and custom CA configuration;
- proxy/environment policy;
- fontconfig discovery, fallback, and web-font cache paths;
- XDG data/cache/config/download directories scoped by app id;
- Flatpak portals for file access, downloads, permissions, and external opens;
- GL/EGL/driver capability diagnostics; and
- safe file/download destination validation.

The current GTK-free `vixen-shell::profile` path/session helpers are useful but
transitional. Path discovery may remain platform code; profile state ownership
moves into `BrowserCore`. All host failures produce structured diagnostics usable
by GUI error pages, headless output, CDP, and smoke reports.

## Trust boundaries and limits

Web content and protocol clients are untrusted. Validate as close as possible to
entry, then preserve typed validated data internally.

| Boundary | Owner | Required behavior |
|----------|-------|-------------------|
| CLI/CDP/GUI command → core | adapter + `vixen-api` DTO validation | Validate ids/options/sizes; reject unknown/stale targets with stable errors |
| navigation/resource request | browser loader + `vixen-net` | URL/private-network/header/body/policy checks on initial request and redirects |
| HTTP response → page/profile | browser loader | CORS/security/integrity/content checks before exposure, execution, decode, cache, or persistence |
| JS → Rust op/resource | runtime host module | WebIDL conversion, size/permission/origin checks, document-generation validation |
| DOM mutation → render state | document lifecycle | Node/document validity, bounded growth, explicit invalidation, no stale commit |
| profile write/read | profile service + `vixen-store` | Partitioned normalized records, bounds, transactional failure diagnostics |
| file/portal/download | Linux host service + download manager | Approved roots/handles, safe names, no ambient arbitrary write/open |
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
- shell, headless, CDP, WPT, and real-site reports translate the same engine
  events;
- no adapter may require page text, JS expressions, credentials, form values, or
  full headers in a default trace.

## Verification and reduction architecture

Evidence layers share production paths:

1. leaf-unit tests for pure policy/data/formatting algorithms;
2. engine integration tests for ownership, lifecycle, generations, and profile
   partitioning;
3. committed local fixtures for focused regressions;
4. pinned imported WPT profiles with source×category reports;
5. GUI/headless visual comparisons and external Playwright/CDP smokes;
6. controlled real-site/Linux-host corridor reports; and
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

`lto = "fat"` is a measurement experiment, not the default. `just size-fp`
measures the real Flatpak GUI artifact and release headless binary. Hard budgets
must be based on published reproducible baselines for the active
`deno_core`/V8/GTK/WebRender dependency graph.
