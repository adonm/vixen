# Roadmap

This is the current delivery order toward the full project goal: a credible
modern-Linux Firefox replacement with a focused desktop shell, first-class
headless/CDP automation, and maximum web capability per byte. Historical phase
notes stay in `PLAN.md`; this file should describe what to build next, with only
short baselines for already-landed foundations.

## Current baseline: foundations, not milestones

Treat these as available building blocks and avoid re-listing them as roadmap
work:

- Developer loop: hk-owned git lifecycle gates, `just` recipes, text-first WPT /
  fixture reports, and actionable failure output.
- Rendering seam: one display-list path feeds WebRender for GUI and headless;
  screenshots use that same path.
- Runtime seam: `deno_core`/V8 is the JavaScript runtime for WPT, headless, CDP,
  and page scripts. Runtime Web API host modules own eval-visible APIs; pure
  value objects may stay JS-only, while page/network/storage/security APIs cross
  explicit Rust ops/resources.
- Profile seam: `vixen-store` persists bounded cookies, Web Storage, fetch cache,
  history, session restore (tabs, active tab, scroll/focus/form hints), downloads,
  permissions, and HSTS/security state.
  Clear-data selections are explicit and profile paths/download destinations are
  XDG/app-ID scoped.
- Network/fetch seam: `fetch()` and the minimal `XMLHttpRequest` surface share
  the same `vixen-net` path for URL policy, cookies, request headers/bodies,
  redirect modes, CSP `connect-src`, referrer policy, active mixed-content
  blocking, CORS modes/preflight/visibility filtering, cache modes including
  conditional `no-cache` revalidation, and stable success/failure network events
  surfaced to CDP.
- Automation seam: CDP already covers navigation, runtime eval/object basics,
  console/exceptions, dialogs, exposed bindings, init scripts, lifecycle-event
  opt-in (`init`, `commit`, `DOMContentLoaded`, `load`), network events,
  screenshot capture, DOM query/resolve basics, viewport
  media emulation, and basic mouse/keyboard input.

## Standing lessons from peer browsers and our recent slices

These constraints should shape every milestone:

- **One path, many seams.** A feature is not browser-shaped if it works only in
  headless, only in CDP, or only in a runtime shim. Prefer the path that serves
  GUI, headless, WPT, CDP, and page scripts together.
- **Host integration is compatibility.** Cert stores, sandbox/file access,
  fontconfig/fallback fonts, XDG downloads, portals, GL/EGL drivers, Flatpak, and
  distro CA layouts need smoke coverage before daily-browser claims.
- **Profile data is product state.** Downloads, cache bounds, clear-data flows,
  session restore, permissions, HSTS, favicons, and storage quotas are part of the
  browser engine/product contract, not shell polish.
- **Inspector can perturb the page.** DevTools/CDP snapshots, overlays, geometry
  queries, mutation observers, and animation inspection must tolerate stale
  layout and must not create an automation-only DOM.
- **Every real-site failure needs a reduction.** Screenshots are triage; a small
  fixture, WPT import, or explicit unreduced issue is the regression guard.
- **Security is fail-closed and diagnosed.** URL/header/cookie/CSP/storage inputs
  validate near the trust boundary; policy blocks should be distinguishable from
  TLS/network/protocol/unsupported-feature failures.

## Alpha: freeze the browser-shaped architecture

Alpha is not broad web parity. Alpha means the architecture can carry the full
browser without replacement. The goal is one small browser that can load,
interact with, inspect, and persist a controlled real-site corridor.

### 1. Authoritative browsing lifecycle and profile object

- Build one engine-owned profile object so tabs share cookies, history, cache,
  HSTS, permissions, and localStorage, while sessionStorage and in-flight
  navigation remain tab scoped.
- Route document navigation, reload/stop, same-document history traversal, form
  submission, `document.write`, script DOM mutation, redirects, CSP/referrer
  policy, permissions, session restore, and error pages through one `Page` /
  engine lifecycle.
- Emit stable lifecycle diagnostics for load start, response commit,
  DOMContentLoaded, load, same-document navigation, network failure, policy block,
  cancellation, renderer/runtime reset, and profile write failure.
- Persist/restore scroll position, focused element, form state where appropriate,
  and same-document history state without requiring layout to be current during
  mutation notification.
- Proof: network/storage/history/navigation WPT profiles, CDP navigation-wait
  tests, shell reload/back/forward/session-restore smoke, and store persistence
  tests.

### 2. Page-backed DOM, WebIDL, events, and forms

- Replace remaining synthetic host-object behavior with page-backed Node,
  Element, Document, HTMLFormElement, controls, events, focus, selection,
  mutation, custom elements-lite hooks where cheap, and browser-real form state.
- Keep generated WebIDL as the interface manifest, but require every stateful
  interface to read/write authoritative page data or an explicit host resource.
  JS-only shims are only for pure value objects.
- Complete forms as a browser vertical: constraint validation, visible control
  value/checked/selected state, labels, submit/reset, entry-list construction,
  encoding, navigation/fetch handoff, and event ordering.
- Prioritize geometry APIs (`getBoundingClientRect`, `getClientRects`,
  `getBoxQuads`, offset/client/scroll metrics, Range rectangles) because real
  apps, automation, hit-testing, and anti-bot probes depend on them.
- Proof: DOM/events/forms WPT profiles, WebIDL conformance fixtures, and CDP DOM
  query/resolve tests using the same page-backed objects.

### 3. Layout and rendering broadening for real pages

- Replace deterministic text metrics and projection shortcuts with the owned
  layout pipeline: block, inline, positioned, floats, overflow/scroll, flex, grid,
  multicol where cheap, intrinsic sizing, and enough fragmentation for common
  documents.
- Add text shaping/font fallback, replaced-element sizing, images/srcset,
  viewport/meta behavior, clipping/scrollbars, sticky/fixed positioning,
  stacking/compositing, filters, transforms, opacity, and invalidation that feed
  the single display-list/WebRender path.
- Treat intrinsic sizing, min/max-content, form-control anonymous layout,
  grid/subgrid, line clamp, scroll snap, multicol, text kerning/shaping, and
  animation/compositor invalidation as high-risk real-site features, not edge
  cases.
- Keep one styled-DOM → layout-tree → display-list flow; no post-pass geometry
  fixups that hide bad authoritative data.
- Proof: imported layout/ref profiles, visual fixtures, real-site corridor
  screenshots, paint/display-list invariants, and `COMPAT.md` pass-count updates.

### 4. Network, security, and privacy envelope reaches browser shape

- Finish fetch/XHR semantics that matter for real sites: streaming bodies where
  needed, upload/download progress, abort propagation, richer cache freshness and
  validators, preflight caching, redirect/CORS corner cases, error taxonomy, and
  DevTools-visible request/response metadata.
- Wire permissions-policy, prompts/decisions, sandboxing, COOP/COEP/CORP,
  partitioned cookies/storage, private-network access blocking, SRI, nosniff,
  Trusted Types sinks, and HSTS upgrades through one fail-closed policy layer.
- Make Linux network/platform behavior testable: cert-store discovery, proxy/env
  handling, HTTP/2 quirks, Flatpak portal access, sandboxed filesystem reads, and
  distro CA-bundle layouts.
- Add a “site blocked us” diagnostic path that separates policy blocks, TLS/cert
  failures, network protocol failures, CSP/mixed-content/CORS blocks, unsupported
  feature detection, and likely anti-bot/fingerprinting failures.
- Proof: `vixen-net` policy tests, security WPT profiles, focused fuzz targets,
  CDP network assertions, and controlled Linux compatibility smokes.

### 5. Desktop shell becomes a daily-smoke browser

- Move from “window can display a page” to the tight browser vertical: tabs,
  URL/search entry, reload/stop, back/forward, find, zoom, downloads/status,
  permission prompts, error pages, history, session restore, settings, and basic
  privacy controls.
- Keep chrome small and inspectable; add UI only when it supports daily browsing,
  debugging, recovery, or profile control.
- Make profile state visible and recoverable: cookies/history/cache/storage survive
  restart, clear-data flows are explicit, profile DB growth is bounded, and
  session restore is deterministic.
- Treat downloads as first-class browser state: XDG download directory, progress,
  cancel/pause/resume when available, persisted history, clear-data integration,
  and safe “show in folder.”
- Smoke every tab/chrome transition that can desynchronize UI state: duplicate,
  close, restore, switch by click/keyboard, hover previews, focus, reload/stop,
  crashed/error page recovery, and renderer/runtime reset.
- Proof: `just flatpak-build`, manual GNOME smoke, `just gate-smoke`, and a
  realworld fixture checklist in `COMPAT.md` or a smoke report.

### 6. Automation is a product surface, not a test harness

- Grow CDP toward a Playwright MVP: target/session lifecycle, runtime handles and
  properties, DOM querying, input, navigation waits, downloads, dialogs,
  console/network/lifecycle events, permissions, screenshots, tracing-lite, and
  stable errors.
- Keep headless CLI, CDP, WPT, and GUI using the same engine/runtime/page paths;
  no automation-only DOM, no headless-only navigation model, no fake network
  model for tests.
- Add inspector stress tests with CDP/devtools attached during animation, DOM
  mutation, style/layout invalidation, form-control shadow updates, downloads,
  navigation, error pages, and renderer recovery.
- Maintain at least one external Playwright smoke script against controlled HTTP
  fixtures and one terminal/app fixture such as the Zuko target in
  `PROJECT_DIRECTION.md`.
- Proof: `docs/CDP_PLAYWRIGHT_SMOKE.md`, CDP integration tests, WPT harness reuse,
  and repeatable Playwright smoke commands.

### 7. Compatibility loop scales honestly

- Expand imported WPT profiles by user-visible risk, not easy pass counts:
  layout/paint, DOM/events/forms, storage/history/network, CSS cascade/values,
  then automation-relevant APIs.
- Publish measured local/imported fixture pass counts in `COMPAT.md` after
  meaningful behavior changes.
- Choose implementation slices from failing WPTs and real-site diagnostics; do not
  claim broad parity until representative profiles run green.
- Maintain a real-site reduction queue. Every corridor failure should be
  classified as layout, DOM/Web API, network/security, storage/profile,
  media/downloads, shell/platform, performance, or anti-bot/fingerprinting, then
  reduced or explicitly marked unreduced.
- Proof: `vixen-wpt` profile reports with local/imported split, green local
  release-blocking fixtures, screenshots for the real-site corridor, and linked
  reductions.

## Beta: credible Linux browser claim

Beta starts when the architecture is frozen, the shell is usable for a controlled
daily-smoke corridor, and failures are measured rather than anecdotal.

1. **Measured real-site corridor** — choose a small, public, reproducible set of
   static sites, docs sites, forms-heavy pages, media/download pages, and app-like
   pages. Load them in GUI and headless, publish screenshots, diagnostics, known
   gaps, and exact commands.
2. **Compatibility dashboards** — track WPT/profile pass counts, local fixture
   status, real-site corridor status, CDP/Playwright smoke status, and known
   unsupported APIs in one compatibility report.
3. **Performance and footprint budget** — track binary size, startup time,
   navigation latency, style/layout/paint time, JS eval latency, memory after
   first page, animation frame stability, download throughput, profile DB growth,
   and screenshot time. Regressions need explicit product tradeoffs.
4. **Reliability discipline** — no panics on malformed content, deterministic
   errors for unsupported capabilities, bounded memory at network/storage/script
   boundaries, restart-safe profile writes, useful crash diagnostics, and recovery
   from renderer/runtime reset.
5. **Security hardening** — complete audit/deny gates, fuzz URL/CSP/cookie/header
   and HTML/storage boundaries, keep private-network fetch blocking and CSP/CORS
   fail-closed, and document the single-process isolation limits honestly.
6. **Packaging and platform smoke** — Flatpak/install paths, GNOME integration,
   portals, certs, fonts, downloads, settings schemas, and GPU/EGL paths are
   smoke-tested on the supported Linux target matrix.

## v1.0: daily-driver minimum

Vixen can call itself a v1 browser only when it is honest and useful, not because
every web API exists.

- The measured corridor is usable with known gaps documented.
- Core browsing works: navigation, forms, cookies/storage/cache, downloads,
  history/session restore, error pages, permissions, and privacy controls.
- Rendering is good enough for common documents/apps: stable layout/paint,
  readable typography, usable forms, scroll/overflow, images, and enough flex/grid
  for the corridor.
- Automation works as a product: Playwright/CDP can navigate, inspect, input,
  screenshot, wait, observe network/console/dialogs/downloads, and report stable
  errors through the same paths as GUI.
- Security posture is explicit: fail-closed policy boundaries, fuzz/audit gates,
  bounded profile/network/script state, documented sandbox/isolation limitations,
  and a credible update/release process.
- Every v1 capability in `ACCEPTANCE.md` has a gate, a fixture/profile, a
  compatibility entry, and a Flatpak/install smoke path.

## v1.x / v2 ambition: after the core browser is credible

- Media, audio/video controls, WebAudio basics, codec integration, and media
  permissions.
- Accessibility tree integration, keyboard navigation, caret/selection fidelity,
  and screen-reader smoke deep enough for daily browsing.
- Service workers, IndexedDB, Cache Storage, Push/Notifications, Background Sync,
  and offline app behavior.
- WebSockets/EventSource, WebTransport/WebRTC where justified, compression,
  streams, and richer fetch/upload/download bodies.
- Extension-shaped APIs only after the browser core is stable; prefer a small,
  explicit permissions model over cloning a full extension ecosystem early.
- Site isolation/OOPIF only if the single-process architecture becomes the
  limiting security issue; design it as a new architecture, not as an ad-hoc
  worker pool.
- WebGPU/mobile/cross-platform ports only after measured Linux desktop and
  headless goals are credible.

## Working rule

Every milestone should land as one coherent batch with:

- a browser-visible seam (`Page`, headless, CDP, GUI, or WPT profile),
- focused tests/fixtures,
- a compatibility/limitation update when behavior changes,
- green focused checks plus hk pre-push gates before push.

Prefer small, boring, engine-shaped slices. A slice is not complete if it only
works in one seam while a parallel model remains elsewhere. Architecture changes
are the only routine reason to stop for human direction before alpha; after
alpha, architecture changes need a new ADR.
