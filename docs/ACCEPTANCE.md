# Vixen acceptance criteria

Release is done only when every applicable gate below passes. Capability claims
map to fixtures/profiles/smokes and the exact BrowserCore/Flutter renderer path
defined in [`SPEC.md`](SPEC.md), [`COMPAT.md`](COMPAT.md), and ADR-022.

Alpha architecture and delivery order are defined in
[`PROJECT_DIRECTION.md`](PROJECT_DIRECTION.md) and [`ROADMAP.md`](ROADMAP.md).

## Hard gates

- [ ] One concrete BrowserCore and one `deno_core`/V8 runtime; no WebKit fallback
      or runtime-engine abstraction.
- [ ] One Flutter Canvas/Paragraph web renderer over an explicitly enabled and
      evidenced Impeller backend for GUI and rendered automation; Skia fallback
      is not accepted platform proof.
- [ ] No production `webrender`, `gleam`, `GlContext`, headless/frame EGL, RGBA
      frame ABI/pools, pixel-buffer texture presenter, fallback painter, or
      second screenshot path after renderer cutover.
- [ ] BrowserCore owns navigation, DOM/runtime, Stylo computed styles,
      network/security, profile state, resource acceptance, web-event semantics,
      and accessibility meaning; Dart owns no durable DOM/browser state.
- [ ] Render revisions, mutation/full-resync payloads, atomic commits, presented
      ids, geometry/text/scroll/semantic queries, opaque Flutter-side hit-test
      handles, input targets, and semantic-action targets with advertised action
      generations are bounded, versioned, and stale-safe.
- [ ] Same-task DOM/style mutation followed by geometry uses cancellable,
      deadline-bounded, deadlock-safe `EnsureLayout` and returns the matching
      Flutter commit.
- [ ] GUI, chrome-less Flutter host, CDP layout/input/screenshots, visual/layout
      WPT, and native Semantics use exact commits from the same renderer.
- [ ] `fixtures/manifest.json` and every declared external profile are green;
      `COMPAT.md` publishes measured counts and limitations.
- [ ] `just audit`, `just check`, hk pre-push, relevant fuzz targets, and
      `git diff --check` pass from a clean checkout.
- [ ] No non-test module over 1,000 lines without an immediate named split.
- [ ] Release artifacts, startup, memory, capture latency, and profile growth are
      measured under the accepted baseline/regression policy.

## Renderer-transition acceptance

### Protocol

Done when R1/R2 from `ROADMAP.md` prove:

R1's dependency-free DTO/reference-validation half is landed and covered by
`just test-api`; C ABI/Dart round trips, retained-payload release, and the
worker-blocked renderer broker remain R2 work.

- compound revisions include context/document/source/style/viewport/resource
  generations;
- incremental batches require exact base revisions and deterministically request
  bounded full resync after a gap;
- malformed ids, unknown resources, non-finite geometry, excess depth/count/
  bytes, truncation, stale commits, and double release fail closed;
- forged, unknown, stale-commit, stale-generation, and replayed Semantics actions
  fail closed;
- C ABI and Dart models round-trip the same wire values; and
- the renderer broker remains serviceable while the originating BrowserCore/V8
  command waits, with cancellation, timeout, and shutdown proof.

### First renderer vertical

Done when one controlled background/text/PNG document proves, from one commit:

- Vixen Dart CSS box/inline formatting over BrowserCore computed inputs;
- Flutter Paragraph shaping/wrapping/range/caret geometry;
- Canvas pixels, paint order, clips, transforms, and image pixels;
- returned immutable basic geometry and renderer-authoritative hit testing;
- scroll limits/offsets and semantic bounds;
- scene capture without browser/compositor chrome;
- mutation update, stale rejection, renderer loss, and full resync; and
- no production claim while the old renderer still serves normal browsing.

### Interactive renderer vertical

Done when the displayed commit drives pointer target validation, cancelable wheel/
key/script scrolling and returned scroll state, find/text/caret ranges, viewport/
zoom revision, native Semantics bounds/actions, lifecycle hide/resume, and stale
scene suppression through widget/core/ABI tests plus Cage smoke.

### Synchronous geometry

Done when tests cover:

```text
DOM/style mutation
  → Stylo flush
  → RenderMutationBatch
  → EnsureLayout(required revision)
  → matching RenderCommit
  → synchronous DOM/CSSOM/CDP geometry
```

No browser mutex is held while waiting; Flutter cannot re-enter BrowserCore;
navigate/stop/close/shutdown and deadline cancel the request; late replies are
inert; repeated geometry reads reuse the accepted commit.

### Cutover and deletion

Done when source/dependency/gate searches prove the full R7 inventory is gone:
WebRender/gleam, `GlContext`, both EGL paths, image upload, frame ABI/tokens/pools,
the Dart frame worker, texture presenter/plugin and recovery tests, superseded
Rust layout/paint, duplicate scale/hit/scroll/text/semantic projections, obsolete
fixtures/gates/docs/dependencies, and renderer-internal CLI flags. GUI and
chrome-less automation share one renderer implementation, and no compatibility
flag/API preserves deleted details. Any retained pure Rust CSS algorithm has an
active Dart consumer through a named stable formatter contract, focused
cross-language tests, and documented evidence that reuse is simpler than
deletion; no Rust geometry, text measurement, hit testing, or paint authority
survives.

## Browser capability acceptance

### HTML, cascade, and selectors

- HTML parser/serialization profiles are green.
- Stylo/selectors profiles cover the supported selector/cascade/computed-value
  surface.
- A computed-style mutation creates the correct renderer source revision; stale
  commits cannot answer inspection.

### DOM/runtime/events/forms

- DOM, events, forms, history, storage, and selected Web API profiles run through
  the live `deno_core` realm and BrowserCore document.
- Script mutation drives a visible Flutter commit and CDP observes the same nodes.
- Focus/event/form-validation ordering pinned by `SPEC.md` remains exact.

### Layout and paint

The Flutter-hosted Vixen formatter passes the published layout/paint profile for
the claimed subset. Nested geometry, clips, transforms, scroll, hit testing,
text/range geometry, semantic bounds, and pixels agree by commit without frontend
coordinate repair. Unsupported tables/floats/fragmentation/writing modes remain
explicit until promoted by measured tests.

### Networking/security/storage/downloads

- `vixen-net` policy and transport tests are green, including URL/private-host,
  cookies, CSP, CORS, mixed content, referrer, integrity, nosniff, and cache rules.
- Policy runs before resource bytes/handles cross to Flutter.
- redb profile tables preserve partitioning, bounds, recovery, clear-data, and
  reopen behavior.
- Download transfer, filename, destination, cancellation, persistence, and UI
  handoff are complete for any download claim.

### Accessibility

BrowserCore-authored roles/names/values/states/relationships/focus/actions combine
with Flutter bounds/text geometry only for the displayed commit. Native AT smoke
proves content and actions; pixels alone do not satisfy accessibility.

## CLI, CDP, WPT, and automation

- Every non-transitional flag in `SPEC.md` works with stable errors.
- Screenshot, visible extraction, coordinate input, layout CDP, and visual WPT
  use the chrome-less Flutter host; text-only fast paths fabricate no geometry.
- CDP supports the declared methods, independent contexts/targets, reliable waits,
  exact-commit input/layout/screenshots, runtime handles, network/lifecycle/
  console/dialog events, permissions, downloads, and bounded traces.
- WPT reports overall/category/source/source×category counts and uses production
  BrowserCore plus the Flutter renderer for every geometry/pixel check.
- External Playwright smoke passes against the same renderer and BrowserCore.

## Shell and Linux product

Manual and automated Linux smoke covers:

- tab create/close/duplicate/reopen and session restore;
- address/search, back/forward/reload/active stop;
- find, zoom, downloads/permissions, settings/privacy, diagnostics, and errors;
- visible Flutter-rendered page content, input, scrolling, text/IME, viewport/
  scale, lifecycle, renderer loss, and recovery;
- native Wayland only; X11/XWayland fail explicitly;
- BrowserCore state ownership and exact displayed-commit input/Semantics; and
- native keyboard/IBus, virtual pointer, AT-SPI, Cage launch/capture, and release
  archive evidence.

FlatPark publication remains after basic browser behavior, host services, and
release evidence. Registry reach never outranks browser correctness.

## Platform gates

A framework-supported platform becomes Vixen-supported only after the latest
stable major OS gate in [`FLUTTER_SHELL.md`](FLUTTER_SHELL.md):

- **Linux first:** final mutation/commit renderer, GUI and chrome-less host,
  Wayland input/IME/AT, host services, deterministic archive, compatibility,
  size, memory, startup, frame, and capture evidence.
- **macOS/Windows:** native BrowserCore/V8 and the same broker/formatter, input/
  IME/accessibility, host services, signing/packaging, capture, and architecture-
  specific measurements.
- **Android:** pinned V8 source/toolchain, renderer broker, lifecycle/process
  recreation, touch/IME/accessibility, host services, capture, and split-ABI proof.
- **iOS Simulator:** `aarch64-apple-ios-sim` BrowserCore/V8/Flutter renderer,
  JavaScript/WebAssembly, simulated lifecycle/input/accessibility/host services,
  capture, and repeatable Xcode build. Physical iOS requires a new decision.
- **WebAssembly:** the single V8 path passes the same API, malformed-module,
  resource-limit, and conformance evidence on every declared target.

## Size and performance gates

Measure separately:

1. like-for-like hello-Flutter;
2. Flutter+Vixen GUI;
3. chrome-less rendered automation host; and
4. any text-only launcher/client.

Reports attribute Flutter engine/ICU, Dart AOT/formatter/assets, native runner/
plugins, BrowserCore/Rust, V8/ICU/snapshots, resources, packaging, symbols, and
any transitional WebRender/EGL code still present. After cutover, removed native
renderer costs must be absent. Reports include locks/revisions, commands, hashes,
architecture, AOT/strip/LTO settings, compressed/unpacked/install sizes, startup,
memory, layout/commit/frame/capture timings, and comparison statistics.

Adopt warnings before hard numeric budgets. Rebaseline only for a documented
product/dependency tradeoff; never hide growth by changing attribution.

## Release ladder

- **Renderer transition:** every R1–R8 gate passes and transitional renderer code
  is deleted.
- **Alpha:** one BrowserCore and Flutter renderer support independent contexts,
  live mutation, synchronous geometry, input, inspection, Semantics, and
  cancellation without stale commits.
- **Beta:** the controlled Linux corridor is usable in GUI and chrome-less
  automation with published compatibility/performance/recovery evidence.
- **v1.0:** daily-driver corridor, security/release operations, host integration,
  automation, accessibility, and every declared platform/capability claim satisfy
  their gates.

Post-v1 replacement work follows `ROADMAP.md`; no fixed version number overrides
measured user/site impact.
