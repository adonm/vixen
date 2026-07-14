# Decision records

Architecture decisions for Vixen, recorded ADR-style. Each entry carries
context, the decision, the alternatives considered, and the consequences.

This file contains the current accepted decisions only. Superseded decision
bodies are removed rather than retained as competing guidance; Git history is
the historical record. When direction changes, consolidate the surviving
constraints into the replacement ADR and update `PROJECT_DIRECTION.md`,
`ARCHITECTURE.md`, and `ROADMAP.md` in the same batch.

---

## ADR-001: Delegate spec-heavy primitives, own browser integration

**Status:** accepted

**Context.** A modern browser cannot credibly reimplement every parser, cascade,
JavaScript, text, and graphics primitive. Whole-engine embedding, however, would
bring another product's navigation, network, persistence, and frontend ownership
and fight Vixen's focused architecture.

**Decision.** Reuse focused upstream components behind Vixen-owned lifecycle and
policy boundaries:

| Subsystem | Selected foundation |
|-----------|---------------------|
| HTML parsing | `html5ever` |
| CSS cascade and selector matching | Stylo / `selectors` |
| JavaScript | `deno_core` / V8 |
| Native cross-platform scene, text, images, and accessibility | Flutter engine and `dart:ui` |

Vixen owns BrowserCore, navigation, network/security policy, persistence, Web
APIs, CSS formatting semantics, the renderer mutation/commit protocol, and
compatibility evidence. Flutter supplies the cross-platform rendering substrate;
it is not treated as a CSS engine or a source of browser policy.

**Alternatives considered.**

- *Build every primitive from scratch.* Rejected as too slow and permanently
  trailing compatibility.
- *Embed Servo, WebKit, or another whole browser engine.* Rejected because it
  creates a second browser lifecycle and contradicts ADR-002.
- *Use generic Flutter widgets as CSS layout.* Rejected because widget layout is
  not web formatting; Vixen still implements and WPT-tests CSS semantics.

**Consequences.** Upstream crates/frameworks set important capability and binary
costs, but Vixen owns their integration and support claims. Component API shape
never counts as browser behavior without a production BrowserCore-to-renderer
vertical and executable evidence.

---

## ADR-002: Single-engine project, no fallback engine

**Status:** accepted

**Context.** A browser project can support multiple engines behind an
abstraction (e.g. WebKit + custom, switchable at compile time or runtime).
This doubles the maintenance surface for no end-user win: every shell
change must be validated against both engines, dependency isolation
requires constant auditing, and only one engine can be the production
path anyway.

**Decision.** Vixen has exactly one engine: the component-backed BrowserCore
described in ADR-001. There is no WebKit fallback, no compile-time engine
selection, and no runtime engine switching.

**Alternatives considered.**

- *WebKitGTK as production + custom engine as preview.* Rejected: at
  that point the project is a WebKitGTK wrapper, not a browser engine
  project. If WebKitGTK is the goal, use GNOME Web directly.
- *Compile-time engine feature flag (one binary, either engine).* Rejected:
  adds dep-leak gates, doubles test matrix, no end-user benefit.

**Consequences.**

- One engine to test, one engine to ship, one engine to document.
- Replacing BrowserCore or adopting a whole engine requires a superseding ADR;
  it is not an adapter or fallback feature.
- Compatibility claims come from Vixen's measured production path, not from the
  lineage of individual parser, cascade, runtime, or renderer components.

---

## ADR-004: Drop the multi-process JS sandbox

**Status:** accepted

**Context.** A previous design used a process-per-origin JS sandbox
(spawned binaries communicating over IPC) for isolation. The embedded JS runtime
already provides in-process context isolation, and out-of-process isolation
(proper OOPIF) is a separate, much larger effort.

**Decision.** Single-process engine. JS isolation is via runtime contexts (one
per origin once host bindings are widened). No `JsSandbox`, no `JsSandboxPool`,
no `process_pool`, no `ipc` module.

**Alternatives considered.**

- *Keep the multi-process sandbox.* Rejected: the complexity (IPC
  framing, pool management, origin-keyed spawn) is not justified by the
  security payoff for a single-user browser. Site isolation, if ever
  needed, is a future Servo-style OOPIF effort.

**Consequences.**

- ~1.5 kLOC less code.
- A single malicious page can still OOM or hang the engine process. This
  matches every other browser's pre-OOPIF behaviour.
- If genuine site isolation becomes a v1.x goal, design it as OOPIF
  against the upstream Servo pattern, not as a forked-engine-per-origin
  approach.

---

## ADR-008: WebGPU and media are post-v1.0

**Status:** accepted

**Context.** WebGPU and media playback are real features but require substantial
integration work with Flutter's scene/platform-texture lifecycle, device policy,
codecs, permissions, and security. Neither is on the critical path for the first
useful browser corridor.

**Decision.** WebGPU and media are outside the v1.0 gate. Promote them after v1
by measured corridor impact. WebGPU uses one bounded native `wgpu` device/policy
integration presented through Flutter's scene; media uses platform codec/
GStreamer services and Flutter-compatible textures under BrowserCore autoplay,
permission, lifecycle, and resource policy. Neither adds a second page renderer.

**Alternatives considered.**

- *Build WebGPU/media scaffolding now, fill in backends later.* Rejected:
  scaffolding without backends is dead code that rots and misleads users.

**Consequences.** v1.0 does not claim WebGPU or media playback. API reflection
stays explicitly inert/unsupported until the corresponding subsystem, policy,
renderer integration, and compatibility evidence exist.

---

## ADR-011: Stylo via the crates.io-published `stylo` crate

**Status:** accepted

**Context.** ADR-001 commits to Stylo (`style`) for the CSS cascade.
When Phase 0–2 landed, `style` was only available as a Servo git
dependency — a clone of `https://github.com/servo/servo` plus a
`[patch.crates-io]` table. That made the build non-reproducible from
crates.io alone and left Phase 3 marked "blocked" in `docs/PLAN.md`.

Since then, the Stylo team split the engine out of the Servo monorepo
into `https://github.com/servo/stylo` and now publish it on crates.io
as [`stylo`](https://crates.io/crates/stylo) (lib name `style`). All
subsystems Vixen needs — cascade, selector matching, rule tree,
computed values — are in that crate.

**Decision.** Depend on `stylo = "0.18"` (with the `servo` feature for
the non-Gecko config) directly. Do not pull a Servo git checkout, do
not patch crates.io, do not vendor the source. Implement
`selectors::Element` (and, for the cascade, `TNode`/`TElement`/
`TDocument`) over Vixen's html5ever `RcDom` in
`crates/vixen-engine/src/style_dom.rs`.

**Alternatives considered.**

- *Hand-roll selector matching on top of `selectors` alone, defer the
  cascade.* Rejected: doubles the selector-matching surface (Vixen's
  plus Stylo's), and the cascade is the actual reason we wanted Stylo
  in the first place.
- *Pin a Servo git revision of `style`.* Rejected: bigger dep surface
  (the whole `servo` repo at that revision), non-reproducible from
  crates.io, blocks Phase 3 indefinitely.
- *Switch CSS engine to `taffy` or another standalone cascade.* Rejected
  per ACCEPTANCE.md hard gates (no `taffy`); also re-introduces the
  perpetual trailing-edge compatibility ADR-001 rejects.

**Consequences.**

- Phase 3 unblocks. The selector-matching surface (`vixen-engine::
  style_dom`) is live; the WPT selector fixtures pass end-to-end.
- The crate ships with its lib name as `style` even though the package
  is `stylo`; source uses `use style::…` while `Cargo.toml` says
  `stylo = …`. Documented in `style_dom.rs` to head off confusion.
- The dependency increase is an accepted trade for a real cascade. Dependency
  and artifact costs remain measured; numerical limits become gates only from
  reproducible baselines under `BASELINES.md` and `ACCEPTANCE.md`.
- Future Stylo releases may shift trait shapes (`TElement` etc.). Pin
  `stylo = "0.18"` and bump deliberately; track upstream
  `https://github.com/servo/stylo/releases`.

---

## ADR-014: Move JS runtime to `deno_core`

**Status:** accepted

**Context.** The first Phase 2 implementation used `mozjs` because the original
plan optimized for Firefox-family components end-to-end. The later Phase 6 work
showed that Vixen's actual risk is the Rust-side host API layer: object
registration, bootstrap JS packaging, resource/permission boundaries, testing,
and long-term maintenance of many Web API families. The `deno_core` crate solves
that packaging problem directly. It brings a well-maintained Rust embedding layer
for V8, explicit extension/op registration, module loading, resource tables,
structured errors, and the runtime architecture Deno uses to expose large Web API
surfaces from Rust.

`deno_core` does mean Vixen no longer uses a Firefox-family JS engine. That is an
acceptable trade: JS language compatibility comes from V8, Web API compatibility
remains Vixen-owned and fixture/WPT-gated, and Rust host-layer velocity matters
more for alpha progress than preserving SpiderMonkey specifically.

**Decision.** Migrate Vixen's JS runtime from `mozjs`/SpiderMonkey to
`deno_core`/V8 and use `deno_core` directly inside `vixen-engine::script`. Do
not introduce a generic JS-engine abstraction or a `dyn JavaScriptRuntime` layer:
Vixen has one JS runtime target, and `deno_core` already provides the embedding
API shape we want. The migration has landed behind the existing
`JsRuntime`/`JsValue`, headless `--eval`, and CDP `Runtime.evaluate` seams.

The target JS architecture is Deno-shaped:

- Host API families live in small modules under `vixen-engine::script` or pure
  sibling modules, not as one ever-growing `script.rs` file.
- Each family has a Rust op/resource surface, a JS bootstrap surface, and
  focused tests. The Rust side owns validation and stable errors; JS glue owns
  Web-shaped object ergonomics only.
- Registration uses a Deno-style extension list: ordered, explicit, testable,
  and feature-family scoped (`encoding`, `dom`, `url`, `fetch`, `storage`, etc.).
- Long-lived host state should use explicit resource IDs/handles and permission
  checks near the op boundary, following `deno_core`/Deno resource-table and
  permissions patterns rather than ad-hoc globals.
- Bootstrap JS is packaged as static assets or generated strings owned by the
  feature module, with Rust tests proving the installed surface.

**Alternatives considered.**

- *Stay on SpiderMonkey and only mimic Deno packaging.* Rejected: it keeps the
  hard part — building and maintaining a browser-scale Rust host layer — while
  missing the maintained `deno_core` abstractions that solve that exact problem.
- *Abstract over `mozjs` and `deno_core` behind an internal JS-engine trait.*
  Rejected: it would preserve two runtime mental models, hide useful
  `deno_core` concepts like extensions/resources/ops behind a leaky common
  denominator, and create a test matrix Vixen does not intend to support.
- *Keep all host glue inside `script.rs`.* Rejected: it does not scale past the
  first few host-object slices and hides feature-family boundaries.
- *Adopt Deno wholesale, including CLI/npm/Node compatibility.* Rejected: Vixen
  needs `deno_core`, not the Deno product surface. Node/npm semantics are not
  part of the browser runtime.
- *Copy Firefox WebIDL binding generation immediately.* Deferred: Firefox's
  binding stack is authoritative for many DOM semantics, but `deno_core` is the
  better Rust embedding/runtime substrate for Vixen.

**Consequences.**

- `deno_core` is the `vixen-engine::script` dependency; `mozjs` is no longer in
  the active engine dependency graph.
- Internal host modules may depend on `deno_core` APIs directly. The stable seam
  is the Vixen product API (`JsRuntime`, `JsValue`, headless/CDP behavior), not a
  portable JS-engine adapter.
- Binary-size gates must be remeasured for V8. The old system/static mozjs split
  no longer applies.
- `docs/REFERENCES.md` pins Deno as the primary JS runtime/host packaging
  reference. Firefox remains a DOM/Web API semantic reference, but not the JS
  engine target.
- New JS host families should be reviewed for module size, bootstrap locality,
  explicit registration, and permission/resource boundaries.
- Existing Page string-smoke projections and bootstrap snapshot pilots should
  migrate into explicit `deno_core` op/resource extensions one family at a time,
  while still reusing the same pure Rust modules.

---

## ADR-016: hk owns git lifecycle gates

**Status:** accepted

**Context.** The previous gate story mixed raw cargo commands, many `just gate-*`
recipes, manual pre-push habits, and ad-hoc agent summaries. Iteration speed is a
north-star concern, but work leaving the machine still needs consistent checks.
The project already uses mise, and hk is built by the same toolchain ecosystem
for fast git hook orchestration.

**Decision.** Add checked-in `hk.pkl` and make hk the git lifecycle enforcement
layer. `just` remains the project command library; hk decides when those recipes
run. Pre-commit stays quick and mostly local: formatting, merge-conflict/private
key scans, and staged diff whitespace. Long gates run only pre-push through one
recipe, `just gate-push`.

The standard pre-push gate is:

```sh
just gate-alpha
just gate-phase6
just gate-smoke
git diff --check
git diff --cached --check
```

**Alternatives considered.**

- *Keep manual gate discipline.* Rejected: too easy for long autonomous sessions
  to drift.
- *Run all long gates pre-commit.* Rejected: hurts iteration speed and produces
  small, slow commits.
- *Replace `just` with hk commands.* Rejected: `just` recipes are still useful
  as explicit project actions and documentation anchors.

**Consequences.**

- Agents may commit and push automatically when hk gates pass.
- Hook setup is part of normal mise/bootstrap workflow.
- If pre-push becomes too slow or misses an important area, change
  `just gate-push` first; keep hk pointing at that stable recipe.

---

## ADR-017: One engine-owned browser, profile, and context lifecycle

**Status:** accepted

**Context.** Sharing component types is not the same as sharing a browser.
Profile sharing, independent tabs, navigation cancellation, stale-result
rejection, downloads, renderer commits, and runtime recovery need one owner above
an individual document, renderer, or protocol session.

**Decision.** `vixen-engine` owns one `BrowserCore` per open profile. It runs on
an engine-owned thread/local executor suitable for the non-`Send` DOM and
`deno_core::JsRuntime`, and owns:

- profile storage, cookies/cache, permissions, HSTS, downloads, clear-data policy,
  and host configuration;
- the top-level browsing-context registry and future child frames;
- context-scoped history, sessionStorage, viewport/input intent, active
  navigation, runtime realms, and committed document state; and
- document-scoped DOM, computed style, render-source revisions, accepted renderer
  commits, script resources, accessibility meaning, and inspector state.

Commands and events cross a browser-scoped `vixen-api` seam and carry typed
context/navigation/document/request/runtime/download/render ids. Asynchronous
work carries its creation generation. Cancellation or supersession invalidates
that generation; late network, script, renderer, geometry, or persistence results
are rejected before mutation, side effects, input targeting, or success events.

The Flutter GUI/chrome-less renderer, text CLI, CDP, and WPT harness are adapters
over BrowserCore. They may own bounded ephemeral renderer state/resources, scenes,
widgets, sockets, and protocol routing, but not alternate navigation, history,
page-runtime, permission, cookie/cache, or profile state. Vixen has one concrete
engine; this seam is not an engine-plugin abstraction.

**Alternatives considered.**

- *One independent engine per tab with a shared store.* Rejected: profile state,
  downloads, renderer scheduling, target routing, and clear-data operations need
  coordinated in-memory ownership.
- *Keep frontend coordinators and share helpers.* Rejected: helpers cannot define
  atomic commit, cancellation, ordering, or teardown across independent owners.
- *Move DOM and V8 into Flutter.* Rejected: browser truth would depend on
  renderer scheduling and protocol adapters would acquire divergent behavior.
- *Make every subsystem `Send + Sync` and distribute it immediately.* Rejected:
  it adds locking/reentrancy before measured isolation or throughput requires it.

**Consequences.**

- `just gate-architecture` forbids frontend direct composition of network/store
  leaves and independent browser orchestration.
- Two contexts can own independent documents/runtimes/render revisions while
  sharing only intended profile state.
- Navigation, stop, history, downloads, error pages, and renderer reset use one
  lifecycle and diagnostic model.
- The owner thread is a reliability boundary. Long script and synchronous
  `EnsureLayout` work need cancellation, deadlines, and deadlock-safe scheduling.

**Implementation status (2026-07-14).** BrowserCore owns production contexts,
profiles, navigation generations, DOM/V8 state, and ordered events for Flutter,
headless, CDP, and WPT. Main-document, external-script/stylesheet, and bounded PNG
loads are generation-cancellable; V8 jobs and runtime fetch waits are deadline-
bounded and interruptible. ADR-022's render mutation/commit ownership is the next
architecture migration.

---

## ADR-019: Validate Flutter targets on the latest stable major OS

**Status:** accepted

**Context.** Carrying a broad legacy OS matrix before Vixen has one supported
release multiplies native runner, graphics, accessibility, signing, and CI work
without compatibility evidence. Flutter's own support range is not evidence that
BrowserCore, V8, Vixen's Flutter renderer, or packaging works throughout that range.

**Decision.** At each release cutoff, Vixen validates one contemporary baseline:
the latest generally available major release for each target OS. Linux uses the
latest stable Fedora Workstation major as its native reference and the current
pinned Flatpak/GNOME runtime for distribution; macOS uses the latest stable macOS
major; Windows uses the latest stable client release and feature update; Android
uses the latest stable major/API; and iOS Simulator uses the latest stable
simulator major in the latest stable Xcode on current macOS. Release evidence
pins exact versions and architectures. Preview releases do not satisfy gates.

Older releases are best-effort unless a release explicitly adds them as tested
tiers. This policy may move forward at any release after native build, rendering,
input, accessibility, lifecycle, packaging, and performance gates pass on the
new baseline.

**Consequences.** Vixen can adopt current platform APIs and security behavior
without promising an untested legacy matrix. Users receive an exact release
manifest rather than an ambiguous “Flutter supports it” claim. Expanding
backward compatibility remains possible, but requires measured demand and its
own ongoing gate capacity.

## ADR-020: Linux Flutter GUI is native-Wayland-only

**Status:** accepted

**Context.** Supporting both native Wayland and X11/XWayland duplicates native
window, compositor, input/IME, accessibility, lifecycle, GPU, and release-smoke
matrices while Linux browser usability is still converging. Fedora Workstation
and the pinned GNOME distribution runtime already provide the contemporary
Wayland target. ADR-022's Linux rendered automation also benefits from one
controlled native Wayland environment under Cage.

**Decision.** The packaged Linux Flutter GUI requires GTK to select a native
Wayland display. Startup on X11 or XWayland exits nonzero with an explicit
diagnostic. Local isolated GUI testing, release archive launch evidence, and
native AT-SPI evidence use Cage with wlroots' headless Wayland backend. FlatPark
permissions will expose Wayland and will not request X11 or fallback-X11.
Rendered CLI, CDP, WPT, and screenshot automation use the chrome-less Flutter
host under Cage after ADR-022 cutover. Text-only native utilities need no display.

**Consequences.** Vixen has one Linux GUI display-server matrix and can focus
native work on Wayland input, IME, accessibility, portals, scaling, and surface
recovery. X11-only sessions cannot launch the supported GUI, and XWayland is not
a compatibility fallback. Reintroducing X11 requires a new ADR plus dedicated
window/input/IME/accessibility/GPU/release gates; framework capability alone is
not sufficient.

## ADR-022: Flutter owns web layout, paint, and rendered automation

**Status:** accepted

**Context.** Vixen targets one focused browser across Linux, macOS, Windows,
Android, and the Apple Silicon iOS Simulator, with Linux first. The implemented
WebRender plus offscreen-EGL path gives GUI and native headless one paint backend,
but Vixen still owns GPU context creation, frame readback/transport, font
shaping/fallback, renderer recovery, and platform texture integration before it
improves web compatibility. Flutter then presents those pixels through another
graphics stack.

Flutter already supplies the supported cross-platform scene, Canvas, Paragraph,
font, image, accessibility, lifecycle, and capture substrate on all five targets.
Using it only for chrome leaves that leverage unused. Flutter is not a CSS engine,
so Vixen must still implement and WPT-test web formatting, fragmentation, scroll,
and inspection semantics.

**Decision.** Flutter is Vixen's sole rendered frontend: it owns web formatting,
text/image measurement, paint, hit testing, semantic geometry, scene presentation,
and rendered automation as well as browser chrome. BrowserCore remains the sole
owner of profile, contexts, navigation, committed DOM, V8, Stylo cascade/computed
styles, resource/security policy, storage, history, downloads, web-event
semantics, and durable accessibility meaning.

Vixen targets Flutter's public Canvas/Paragraph/scene APIs with **Impeller** as
the required engine rendering backend. The runner enables Impeller explicitly;
a Skia-backed launch does not satisfy renderer, release, or platform evidence.
The latest pinned Flutter beta is deliberate while required Linux Impeller
support has not reached the selected stable SDK. Vixen does not call private
Impeller APIs or add a backend-specific paint path; Flutter remains the boundary.

The product targets are Linux, macOS, Windows, Android, and the Apple Silicon iOS
Simulator. Linux is the first renderer, GUI, chrome-less automation, packaging,
and release gate. Physical iOS and App Store distribution require a later
runtime/distribution decision. Flutter is the only GUI and the only rendered
headless substrate; no fallback native renderer is retained after cutover.

### Renderer source protocol

BrowserCore publishes bounded mutation batches over an exact compound revision:

```text
RenderRevision {
  context_id
  document_id
  source_revision
  style_revision
  viewport_revision
  resource_revision
}

RenderMutationBatch {
  base_revision
  target_revision
  mutations
}
```

Mutations describe immutable styled render inputs, stable DOM/resource/semantic
ids, accepted text/image/font resources, pseudo/generated content, scroll intent,
and removals. They are not a Dart DOM and cannot be mutated into browser state.
The renderer builds CSS box and anonymous trees from them. A missed base revision
fails closed and requests a bounded full snapshot; batches are never guessed,
reordered, or applied to another document.

Dart may retain only bounded active renderer generations and resources. Node,
mutation, string, depth, image/font byte, fragment, query, and queue limits apply
at the bridge. BrowserCore owns web-font fetch, CSP/CORS/integrity/cache policy and
passes only accepted resources to Flutter's font collection.

### Flutter renderer ownership

Vixen implements CSS block, inline, flex, grid, positioned, overflow, replaced-
element, table, and fragmentation behavior in Dart against computed BrowserCore
inputs. Ordinary Flutter widgets, Flutter Flex, and third-party UI layout packages
are not treated as CSS implementations. A widget-per-DOM model is neither required
nor allowed to become durable browser state.

Flutter's `dart:ui` Paragraph is authoritative for shaping, fallback, bidi, line
breaking, intrinsic text measurement, caret/range geometry, and text hit testing.
Canvas/scene APIs own clipping, transforms, paint order, compositing, images, and
capture. BrowserCore must not retain competing approximate layout/text metrics
after cutover.

### Atomic renderer commit

A renderer commit means layout, query data, semantic bounds, and a scene are ready
for the same source revision:

```text
RenderCommit {
  commit_id
  render_revision
  viewport
  geometry_index
  hit_test_handle
  text_query_handle
  scroll_snapshot
  semantic_bounds
  truncation_state
}
```

Basic immutable border/padding/content/fragment/clip/scroll/paint-order geometry
is returned to BrowserCore so ordinary synchronous DOM/CSSOM/CDP queries do not
cross FFI repeatedly. Flutter remains authoritative because it produced that
index; BrowserCore only validates and queries it. The hit-test handle names an
immutable Flutter-retained index and is opaque to BrowserCore: Flutter resolves
it and returns a bounded target for validation. Paragraph-specific offset, caret,
range-box, affinity, and selection operations may use a bounded batched renderer
query service.

`RenderCommit` and `Presented(commit_id)` are distinct. Geometry can be accepted
before presentation, but visible input and native accessibility identify the
actually displayed commit. BrowserCore rejects commits, queries, input targets,
scroll results, and semantic bounds whose context, document, viewport, resources,
or revision no longer match.

### Synchronous layout broker

A script can mutate style and synchronously call `getBoundingClientRect()` in the
same task. BrowserCore therefore exposes bounded `EnsureLayout(required_revision)`
and geometry-query operations through a request/response broker:

```text
V8 geometry read
  → BrowserCore flushes DOM + Stylo
  → publishes required RenderMutationBatch
  → posts EnsureLayout to the Flutter renderer
  → waits for matching RenderCommit or cancellation/deadline
  → answers from committed geometry
```

The BrowserCore owner thread may wait without holding browser mutexes. The Flutter
UI/renderer isolate processes the request without re-entering BrowserCore and
returns through a separate response channel. Navigation, stop, close, and shutdown
cancel the wait; late commits are inert. The current polling event worker cannot
be the only broker if it is blocked on the originating evaluation. Cutover is
blocked until this path is deadlock-safe, bounded, and tested with same-task
mutation plus geometry reads.

### Input, scroll, and accessibility

Flutter performs hit testing against the displayed commit and sends a bounded
target containing commit/revision, stable node/fragment ids, and coordinates.
BrowserCore validates that target before DOM dispatch. It owns cancelable event
semantics and default-action policy. Flutter owns mechanical scroll geometry,
offsets, clips, and clamps; BrowserCore sends an accepted scroll command only
after `preventDefault()` and owns DOM scroll events, script intent, history
restoration, and persistence. Renderer scroll results return in a new exact
commit.

BrowserCore authors accessibility role, name, value, state, relationships, focus,
policy, and actions. Flutter contributes accepted semantic bounds/text geometry,
combines them only for the displayed commit, and publishes native Semantics.
Actions route back with exact document, commit, semantic node, and advertised
action generation.

### Rendered automation

A minimal chrome-less Flutter host creates BrowserCore, accepts CLI/CDP/WPT
requests, and captures the exact Flutter scene/commit. Linux runs it under
Cage/wlroots headless Wayland. Other platforms use their native Flutter runner
when their rendered gates begin. Text-only utilities may remain native clients
when they do not invent geometry or pixels. One logical rendered session owns
exactly one BrowserCore inside the host; a native launcher never splits fast
DOM/runtime commands into a second core. GUI bundles need not ship developer
automation entrypoints.

### Migration and deletion policy

The current Rust layout/display-list/WebRender/EGL/RGBA path is transitional and
frozen except for security, data-loss, or release-blocking correctness fixes.
Do not complete adjacent WebRender, EGL, texture, deterministic-font, or Rust
layout breadth merely because scaffolding exists. Partially implemented code is
not an asset when deleting it shortens the renderer transition and its behavior
is not independently needed by BrowserCore.

Experimental Flutter renderer work remains test-only until one controlled
vertical proves layout, pixels, input, geometry, text ranges, scroll, Semantics,
and scene capture from one commit. There are never two supported production
renderers. Cutover removes WebRender/gleam, `GlContext`, headless/frame EGL,
image upload, RGBA frame ABI/pools, the Dart frame worker, pixel-buffer texture
plugin/presenter and recovery tests, superseded Rust paint/layout modules or
DTOs, duplicate scale/hit/scroll/text/semantic projections, obsolete fixtures/
gates/docs/dependencies, and renderer-internal CLI flags. Pure CSS algorithms may
be moved/reused only when the Dart formatter consumes them through an explicit
stable data contract and that is simpler than reimplementation.

**Alternatives considered.**

- *Keep WebRender and Flutter only for chrome.* Rejected: it preserves the
  cross-platform GPU/font/surface burden and duplicates graphics stacks.
- *Capture the current Flutter window under Cage.* Rejected as a migration: the
  current window already contains WebRender/EGL pixels, so capture removes
  nothing.
- *Use Flutter only as a painter over Rust final geometry.* Useful only as the
  first proof; rejected as the destination because layout, text, hit testing,
  semantics, and pixels could diverge.
- *Map every DOM node to ordinary Flutter widgets.* Rejected because widget
  layout is not CSS and a mutable widget tree would become a second DOM.
- *Keep separate Flutter GUI and Rust headless renderers.* Rejected because visual
  and geometry fixes would retain two acceptance paths.

**Consequences.**

- Flutter is required for screenshots, visual/layout WPT, rendered CDP, and GUI.
  Linux rendered automation requires Cage/headless Wayland rather than
  surfaceless EGL.
- Flutter SDK promotion must preserve Impeller scene, capture, recovery, and
  driver evidence; “a Flutter window opens” is not renderer proof.
- CSS layout remains a major Vixen subsystem, now concentrated on Flutter's
  cross-platform text/scene substrate.
- The FFI boundary grows render mutation/commit/query traffic before frame
  buffers and native renderer dependencies are deleted. It is a primary
  content-controlled trust and performance boundary.
- Rendered headless startup may grow while total GUI/native complexity and
  platform-specific dependencies shrink. Measurements compare the chrome-less
  host, GUI, and removed WebRender/EGL costs honestly.
- Every platform still earns support through native BrowserCore/V8, renderer,
  input, accessibility, lifecycle, host-service, package, size, and performance
  evidence under ADR-019.

**Migration gates.** Execute in order:

1. Define and test bounded `RenderRevision`, mutation/full-snapshot, commit,
   presented, geometry, target, scroll, semantic-bound, and query DTOs in
   `vixen-api`, including semantic-action targets bound to document, displayed
   commit, semantic node, and advertised action generation.
2. Carry them through the C ABI and handwritten Dart models with malformed,
   stale, truncation, release, and resync tests; production still uses the old
   frame during this protocol-only step.
3. Render one controlled background/text/image document with a test-only Flutter
   formatter/Canvas/Paragraph path and atomically return geometry, hit/text
   queries, scroll state, and semantic bounds.
4. Prove same-commit pixels, input targeting, find/caret ranges, scrolling,
   Semantics, and scene capture in the Linux shell.
5. Add the chrome-less Flutter host under Cage and move visual/layout fixtures
   plus screenshot/CDP capture to it.
6. Implement and race-test bounded synchronous `EnsureLayout`, cancellation,
   resync, renderer loss, and same-task mutation-to-geometry behavior.
7. Cut production over; aggressively delete WebRender/EGL/RGBA/texture and
   superseded Rust layout/paint code, dependencies, tests, gates, and docs.
8. Reproduce Linux compatibility, interaction, accessibility, size, memory,
   startup, and release gates, then continue the full browser roadmap and expand
   the same renderer contract to the other four targets.
