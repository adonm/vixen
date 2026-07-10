# Roadmap

This is the delivery sequence from the current component-rich prototype to the
full project goal: a credible Firefox replacement for modern Linux, with a
focused desktop shell and first-class headless/CDP automation. It is deliberately
more ambitious than a demo-browser plan, but it does not turn ambition into an
unsupported compatibility claim.

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
   in GUI and automation with representative rendering, interaction, persistence,
   downloads, diagnostics, and Linux integration.
3. **v1.0 — an honest daily-driver minimum.** The published corridor is reliable
   enough for focused daily use, release/security operations are credible, and
   every supported capability has reproducible evidence.
4. **Replacement horizon — broad modern-browser capability.** Media,
   accessibility, offline applications, richer graphics/communications, extension
   support, and stronger process isolation expand the useful site set until
   “Firefox replacement” describes ordinary use rather than only a corridor.

No stage implies global Firefox or WPT parity. Compatibility claims always name
the profile, platform, command, and measured result.

## Proven baseline — use it, do not roadmap it

As of 2026-07-10 the repository has these building blocks:

- A seven-crate workspace, hk/`just` development gates, stable diagnostics, fuzz
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
- A Relm4/libadwaita shell vertical with tabs, URL loading, basic navigation,
  visible WebRender output, diagnostics, and bounded session restore.

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
duplication to the live document lifecycle. Parser/script/resource work still
runs synchronously on the core owner after source loading, and compatibility
projections still coexist with the live page/runtime. Adding broad API shape
before converging those paths would preserve plausible but disconnected state.

Other material gaps remain:

- main-document source loading is asynchronous, cancellable, and generation
  checked, but parser, script, and discovered-resource work is still synchronous
  and not cooperatively interruptible;
- DOM/runtime snapshots and compatibility projections still coexist with live
  page state;
- layout uses deterministic text metrics and narrow block/inline/flex/grid
  coverage rather than shaped text and a broad formatting pipeline;
- images, fonts, subresource loading, media, accessibility, workers, IndexedDB,
  service workers, and multi-frame/multi-page execution are absent or only
  browser-shaped probes;
- downloads have persistence/protocol shape but no complete HTTP transfer
  lifecycle; and
- Linux cert/proxy/font/portal/GPU behavior and release measurements are not yet
  proven across a supported matrix.

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
   and profile growth get reproducible baselines before hard thresholds. Do not
   preserve fictional limits from an obsolete dependency graph.
9. **Real-site failures become reductions.** A screenshot starts triage. A local
   fixture, pinned WPT case, or explicitly tracked unreduced failure prevents
   regression.

## Alpha — converge on one browser core

Alpha freezes an architecture capable of carrying the full goal. Complete these
in order; later work may proceed in parallel only when it does not create a new
state owner.

Progress as of 2026-07-10: A1 is routed through the dependency-free typed
`vixen-api` command/event seam and one `vixen-engine::browser::BrowserCore`.
BrowserCore owns one engine thread, profile Store/network/cookies, bounded
context/runtime registries, history, evaluation, inspection, and paint inputs.
WPT, headless CLI, CDP, and the GTK shell are thin adapters over that owner; two
CDP/shell contexts prove independent globals/sessionStorage/history with intended
profile sharing. The 2,015-check fixture manifest, GTK-free shell tests, and the
external Playwright smoke remain green.

The first A2 slice is also landed: dispatch acknowledges navigation before source
completion; a bounded Tokio loader performs cancellable HTTP/file reads; each
completion carries its context/navigation generation; current-generation cookie
deltas merge at the core boundary; and deterministic navigate/navigate plus
navigate/stop races prove late source results cannot commit, append history,
overwrite cookies, or emit terminal success. Parser/runtime construction and
post-commit script/resource work remain on the owner thread and are the next A2
boundary.

### A1. Engine-owned profile, browser, and browsing contexts

- Implement one production browser core in `vixen-engine` and expose it through
  an evolved `vixen-api` command/event seam.
- One profile service owns the store, cookies, cache, permissions, HSTS,
  downloads, clear-data policy, and host configuration. One browser service owns
  the tab/context registry. Each context owns its active document, runtime,
  session history, viewport/input state, and navigation controller.
- Give tab/context, document, frame, navigation, request, runtime-context, and
  download records stable typed ids. Include those ids in diagnostics/events so
  stale work and cross-target routing are testable.
- Run the non-`Send` DOM and V8 state on one engine-owned local executor. Shell
  and protocol I/O may use workers, but engine ownership must not be split across
  per-frontend state machines.
- Replace shell/headless direct orchestration with thin adapters. Direct
  `vixen-net`/`vixen-store` use outside the engine is migration debt, not an
  extension point.

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

**Current proof:** navigate/navigate and navigate/stop use gated transport plus
forced late completions; shell/headless/WPT/CDP wait for matching typed terminal
events; the external Playwright smoke covers navigation, history, reload, and
`document.write`/`setContent`. Redirect/stop, history/reload races, parser/runtime
cooperative cancellation, and asynchronous CDP event delivery remain.

### A3. Live document/runtime integration

- Replace remaining snapshots and string-expression projections with live
  page-backed Node/Element/Document, CSSOM, events, focus, selection, forms,
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

### B4. Daily-smoke desktop product

- Deliver robust tabs, address/search, reload/stop, back/forward, find, zoom,
  downloads, permission prompts, error/recovery pages, history, session restore,
  settings, clear-data/privacy controls, keyboard navigation, and safe external
  opens.
- Make every chrome transition resilient to late engine events, tab close,
  document/runtime reset, and failed profile writes.
- Keep the shell focused: UI exists to browse, recover, inspect, automate, or
  control profile/security state—not to become a feature buffet.

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
measurements, and known gaps on the supported Linux matrix.

## v1.0 — honest daily-driver minimum

Vixen may call itself v1.0 when all of the following are true:

- common document, documentation, form, download, and app-like pages in the
  published corridor are readable and usable with stable typography, images,
  layout, scrolling, interaction, navigation, and profile state;
- shell and Playwright/CDP use the same engine paths and recover predictably from
  network, document, runtime, renderer, and profile failures;
- supported network/security/privacy behavior is fail-closed, observable, and
  backed by tests/fuzzing/audit; single-process isolation limits are prominent;
- Flatpak install/update, certs, fonts, portals, downloads, GPU/EGL, settings,
  session restore, and clear-data flows pass on the declared Linux targets;
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
   description. Cross-platform ports remain separate product decisions.

## Immediate execution order

The first three convergence batches are complete. The source-loading part of the
fourth is complete. The next coherent batches are:

1. Finish A2 across redirects, parser/runtime jobs, history/reload races, and
   asynchronous CDP lifecycle delivery; preserve exactly one terminal outcome.
2. Move parser-discovered scripts and supported DOM mutations onto the live
   document/runtime, deleting replaced snapshot/string shims.
3. Land font discovery/shaping/fallback and image subresource decode as the first
   broad rendering verticals, then widen layout by failing imported ref tests.
4. Build the HTTP download lifecycle and shell/CDP events over profile-owned
   downloads.
5. Establish measured real-site, Linux-host, performance, and size baselines that
   gate beta work.

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
