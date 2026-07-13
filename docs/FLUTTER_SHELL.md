# Flutter shell plan

This is the authoritative plan for Vixen's primary GUI shell on Linux, macOS,
Windows, Android, and the Apple Silicon iOS Simulator. It defines migration order, ownership, rendering,
FFI, accessibility, packaging, artifact measurement, and platform gates. Product
scope remains in [`PROJECT_DIRECTION.md`](PROJECT_DIRECTION.md), browser-engine
ownership in [`ARCHITECTURE.md`](ARCHITECTURE.md), and accepted tradeoffs in
[`DECISIONS.md`](DECISIONS.md) ADR-018.

**Linux is the highest-priority shell, integration, packaging, and release
target.** Complete Linux parity and native evidence before equivalent platform
expansion. The other four targets remain part of the product direction, but
reuse the BrowserCore/Flutter boundary proven on Linux and must not delay Linux
convergence.

Within Linux work, browser behavior outranks distribution reach. FlatPark
submission, review, and publishing are deferred until the Flutter shell can
navigate and visibly render controlled sites, scroll through content, accept
keyboard plus IME text, perform back/forward/reload/stop and find/zoom, and
recover from bounded navigation/runtime/surface failures. The deterministic
release archive remains a build gate while that product gate is open.

## Status and evidence boundary

**Implemented Linux alpha slice:** the repository contains a Flutter 3.46 beta Linux
application, deterministic fake-controller tests, handwritten Dart FFI, a
persistent worker isolate that exclusively owns one BrowserCore handle and its
ordered event stream, and a generated GTK-backed Linux runner. Production fails
closed when its process-adjacent `lib/libvixen_ffi.so` or ABI v1 is unavailable;
it never substitutes the scripted controller.

The same additive ABI v1 now exports bounded retained RGBA frames. BrowserCore
captures one authoritative document generation through WebRender/EGL; Dart
copies the frame through `TransferableTypedData`; and the Linux runner publishes
it through one `FlPixelBufferTexture` with a mutex-protected three-buffer pool.
Dimensions are capped at 4096 per axis and 64 MiB per frame, with at most three
retained native frames and one in-flight Dart capture plus one newest
replacement. `just gate-flutter-shell` runs format, analysis, 55 Dart/widget/
native smoke tests, and the native ABI gate. A Fedora 43 container build also
produced a relocatable debug bundle containing the executable, Flutter embedder,
and `libvixen_ffi.so`.

This does not establish Linux parity: contenteditable/IME action and gesture
input, real native IME evidence, complete lifecycle recovery and scale handling, complete
semantic relationships/actions and native AT smoke, exact find highlighting,
downloads/permissions,
host services, broader FlatPark host/portal coverage, release size/performance,
and non-Linux runners remain open. The current
Relm4/libadwaita/GTK shell remains the compatibility baseline until those gates
pass.

Flutter officially supports native deployment to Android, iOS, Windows,
macOS, and Linux. That establishes a supported shell substrate, not proof that
Vixen's Rust/V8/WebRender stack builds, packages, performs, or satisfies store
policy on each target. Every Vixen platform remains gated below.

## Contemporary OS policy

Vixen validates the **latest generally available major OS release** for each
Flutter target at the release cutoff. Release manifests record the exact OS,
SDK, image, architecture, and toolchain versions used; moving that pin is an
ordinary tested platform update, not an implicit compatibility claim.

- Linux uses the latest stable Fedora Workstation major as its native reference
  host and the current FlatPark/GNOME runtime as its distributable runtime.
- macOS uses the latest generally available macOS major on supported Apple
  Silicon hardware.
- Windows uses the latest generally available Windows client release and current
  feature update.
- Android uses the latest stable Android major/API level and current stable
  Android toolchain.
- iOS Simulator uses the latest stable iOS Simulator major supplied by the
  latest stable Xcode on the latest supported macOS major.

Older OS releases are best-effort unless a release manifest explicitly adds a
tested compatibility tier. Preview/beta OS releases may inform development but
cannot satisfy a release gate.

## Fixed boundaries

- **BrowserCore is the sole browser owner.** It owns profiles, contexts,
  navigation, documents, runtime, network policy, storage, layout, display
  lists, WebRender, accessibility source data, and lifecycle ordering.
- **Dart owns chrome and presentation only.** Tabs, toolbars, menus, dialogs,
  window layout, platform-adaptive controls, and host-service UI live in
  Flutter. Dart must not acquire an alternate navigation, history, cookie,
  permission, profile, DOM, layout, or renderer model.
- **One engine, one JS runtime, one renderer.** The engine remains BrowserCore,
  the JS runtime remains `deno_core`/V8, and WebRender remains the only web-
  content renderer. Flutter renders browser chrome, not web content.
- **Headless remains Rust-owned.** CLI, CDP, WPT, and EGL surfaceless paths do
  not embed Flutter and are not shipped inside GUI bundles.
- **The bridge is browser-scoped.** It carries bounded typed commands, events,
  snapshots, frames, semantics updates, and host-service requests. It does not
  expose mutable Rust objects or let callbacks bypass BrowserCore ordering.

## Five-platform matrix

| Platform | Validation OS | Initial Vixen integration | Required release evidence | Current Vixen status |
|----------|---------------|---------------------------|---------------------------|----------------------|
| Linux — highest priority | Latest stable Fedora major plus pinned current FlatPark/GNOME runtime | Dart FFI bridge, bounded RGBA external texture, Flutter input/viewport, GTK-backed Flutter Linux embedder | Basic-browser gate and Flutter parity first; deterministic official archive throughout; checksum-pinned FlatPark publication only afterward; GPU/driver, portal, accessibility, size, and performance reports | Chrome, BrowserCore bridge, RGBA texture, viewport/input, root wheel/key scrolling, native text-control IME state, bounded find traversal/scroll-to-match, core-owned zoom, bounded semantics shape, tests, release/AOT archive build, clean extraction, and Impeller Xvfb smoke implemented; contenteditable/IME actions and native evidence, nested/touch/script scrolling, exact find highlighting, recovery, full semantics/native AT, host services, broader matrix, and parity remain open; FlatPark publishing is deferred |
| macOS | Latest stable macOS major | Same bridge and RGBA contract in a native Flutter runner | Native BrowserCore/V8/WebRender build, signing/notarization, input/IME, accessibility, host services, architecture attribution, size/performance reports | Target; unproven |
| Windows | Latest stable Windows client release/feature update | Same bridge and RGBA contract in a native Flutter runner | Native BrowserCore/V8/WebRender build, packaging/signing, input/IME, accessibility, host services, per-architecture size/performance reports | Target; unproven |
| Android | Latest stable Android major/API | Same bridge, RGBA external texture first, GLES-backed WebRender, lifecycle-aware runner | Pinned V8 source archive/toolchain, reproducible source cross-build, GLES, lifecycle/background recovery, input/IME, accessibility, split-ABI packaging, size/performance proof | Committed target behind gates; unproven |
| iOS Simulator | Latest stable iOS Simulator major in latest stable Xcode/macOS | Same bridge and RGBA external texture using Rust/V8 `aarch64-apple-ios-sim` | Simulator BrowserCore/V8/WebRender build, V8 JavaScript/WebAssembly, lifecycle/input/accessibility, and advisory size/performance proof | Committed simulator-only development target; unproven |

The Linux Flutter embedder uses GTK. Migrating the shell removes Vixen's direct
Relm4/libadwaita/custom `gtk4::GLArea` ownership; it does **not** promise that GTK
runtime dependencies disappear from Linux packages.

## Migration sequence

### 1. BrowserCore bridge and Linux fake shell

The safe Rust controller, narrow C-compatible ABI, Dart bindings, and Linux
Flutter chrome are implemented. `just gate-native-abi` remains native-only
evidence. `just gate-flutter-shell` additionally proves the injected scripted
controller, tab/routing/focus/shortcut/dialog/error/teardown behavior, production
worker protocol, and live process-adjacent native bridge. Scripted behavior is
test-only and cannot become a production fallback.

Bridge rules:

- one opaque browser handle per open profile, with typed context/frame ids in
  messages rather than caller-owned Rust pointers;
- explicit create/destroy and version negotiation, with no Rust references held
  by Dart;
- owned buffers with one documented allocator/free path and checked lengths;
- commands copied onto BrowserCore's owner queue; the current ABI consumes the
  controller's sole bounded ordered event stream and adds per-handle sequence
  numbers while retaining typed generation ids; a future Dart-facing transport
  must not add a second browser model or event consumer;
- no synchronous callback from BrowserCore into Dart while Rust locks or a V8
  scope are active;
- cancellation, stale-event rejection, and shutdown remain BrowserCore
  semantics; and
- structured stable errors cross FFI, never Rust panics or Dart exceptions as a
  lifecycle protocol.

Use handwritten FFI initially if that keeps ownership inspectable. A bridge
generator may be adopted only after its generated native/Dart surface, build
behavior, artifact cost, and platform support are measured.

### 2. Linux real shell and bounded RGBA texture

This slice is implemented for Linux. The same Flutter chrome connects to
BrowserCore, which renders web content through WebRender to an engine-owned
offscreen target. The interoperability contract exports a bounded RGBA frame for
a Flutter external texture:

- negotiate physical width, height, scale factor, pixel format, row stride, and
  monotonically increasing frame id;
- cap dimensions and byte length before allocation and on every resize;
- use a bounded pool with explicit acquire/publish/release ownership;
- drop or replace stale frames rather than grow an unbounded queue;
- invalidate/publish on Flutter's platform-thread rules without blocking the
  BrowserCore owner thread; and
- measure copies, conversion, frame latency, memory bandwidth, resize churn,
  dropped frames, and input-to-paint latency.

This is one WebRender path with a transport copy, not a second renderer. Flutter
must not repaint or reinterpret Vixen's display list.

### 3. Input, viewport, accessibility, and host services

The first Linux input slice is implemented. Flutter maps logical pointer and
wheel positions into the exact bounded physical frame viewport and sends strict
context/document/runtime-generation commands through a serialized 64-event
queue. BrowserCore performs authoritative hit testing before mouse dispatch;
the wire never accepts a Dart-selected node id. Keyboard down/up events preserve
modifiers and text where Flutter provides it, shell shortcuts remain chrome-owned,
and input responses retain runtime effects and navigation actions. Pointer
cancellation clears only the matching context/document/runtime primary press, so
a later release cannot synthesize a stale click. Each accepted input requests a
fresh generation-checked frame.

The first host-view lifecycle slice is implemented. A strict monotonic command
carries the selected context, bounded physical viewport, effective Flutter scale,
content focus, visibility, and resumed/inactive/hidden/paused/detached state.
BrowserCore owns the latest generation, rejects stale updates, preserves it across
document replacement, updates `document.hasFocus()`, `document.hidden`, and
`document.visibilityState`, emits live focus/blur and `visibilitychange`, and
rejects content input while inactive. Flutter invalidates queued input on every
transition, suppresses hidden captures, and cancels pending primary presses at
the controller boundary. The stored scale does not yet separate CSS layout pixels
from the bounded physical render target.

The remaining target adds contenteditable/IME action handling and native IME
evidence, touch and nested scrolling,
CSS/device-scale correctness, and platform lifecycle/surface recovery.
BrowserCore continues to own hit testing, selection, DOM event dispatch, and
navigation effects. Platform-specific raw data may be retained in bounded DTOs
where web semantics require it.

The first platform text-input vertical is implemented for focused writable
native text inputs and textareas. BrowserCore's semantic projection selects the
eligible control, Flutter attaches one `TextInputClient`, and every platform
update sends a value bounded to 16 KiB plus UTF-16 selection and optional
composing ranges through exact context/document/runtime ids. BrowserCore
revalidates the ranges and focused target before applying the state to the live
control and dispatching composition-shaped, cancelable `beforeinput`, and
`input` events. Stale targets and inactive host views fail closed. Widget/wire/
core tests exercise non-ASCII composition; contenteditable, IME action and
keyboard-type specialization, and real desktop-IME evidence remain.

The first engine-owned scrolling vertical is now implemented for the top-level
document. Flutter scales wheel deltas into frame coordinates, BrowserCore sends
cancelable wheel and keyboard events to the live target, and only uncanceled
defaults mutate a clamped Page scroll offset. Arrow, Page Up/Down, Home/End, and
Space keys use the BrowserCore-owned CSS viewport, while focused native/editing
controls retain their own key defaults. Paint, hit testing, selector and
Semantics bounds share the translated layout; fixed-position subtrees stay
anchored. Nested scrollers, touch gestures, restoration, smooth scrolling, and
script-driven scrolling remain in the target above.

The find-in-page vertical now includes traversal and scroll-to-match. Ctrl+F and
the menu open a Flutter-owned find bar, while exact context/document commands ask
BrowserCore for a 10,000-match-bounded case-insensitive rendered-text result.
Page owns the active match; Enter/F3 and Previous/Next advance or reverse with
wrapping and update the same clamped root offset consumed by paint, hit testing,
and Semantics. Results are generation-checked before presentation, announced
through a live region, and trigger a paired frame/Semantics refresh; Dart never
inspects page text. Exact glyph-range highlighting remains before find parity is
complete.

Per-context page zoom is now core-owned and bounded from 25% through 500%.
Flutter shortcuts/menu actions carry only zoom intent. BrowserCore derives a
CSS viewport from the physical target, scales the single display list back to
the frame, converts hit testing and wheel events into CSS coordinates, and
projects Semantics bounds into physical display coordinates. Zoom resets only
on explicit Ctrl+0 and survives navigation in the context; profile persistence,
text-shaping fidelity, and device-scale/surface recovery remain open.

The initial accessibility hierarchy is implemented. BrowserCore/Page derives native
and explicit ARIA roles, bounded names (including `aria-labelledby` and HTML
labels), bounded descriptions, values, states, focus, tap/focus actions, and
physical layout bounds. Bounded `aria-controls`, `aria-describedby`, and
`aria-details` ID references retain only nodes in the semantic projection;
controls map to stable Flutter semantic identifiers while resolved description
text maps to Flutter's hint. Enabled native `input[type=range]` controls expose
bounded numeric min/max/current/step state plus exact-generation
increase/decrease actions through the live value/input/change path. Authored
`slider` and `spinbutton` roles with finite `aria-valuenow` expose numeric state
(plus `aria-valuemin`, `aria-valuemax`, and `aria-valuetext`) and
exact-generation adjustments. Those actions focus the live target and dispatch
orientation-appropriate arrow-key events; only author script updates authored
ARIA state. Engine
snapshots cap at 1024 nodes and 512 UTF-8 bytes per string; the ABI caps the exact
wire projection at 192 nodes under 1 MiB. A deterministic nonzero semantic
generation invalidates document-order ids after mutation. The coordinator
publishes Semantics only when its context, document, viewport, and capture
generation match the displayed frame, and Flutter keys nodes by semantic
generation/id. Each node names its nearest emitted semantic DOM ancestor;
document-order validation guarantees retained parents precede children, and
Flutter builds nested Semantics without inferring hierarchy from geometry. Dart
does not infer meaning from pixels or maintain a second DOM.

Explicit polite/assertive `aria-live` and the implicit live-region roles map to
Flutter's live-region flag, with explicit `aria-live="off"` taking precedence.
An active-context `runtime_effects` event forces a new paired frame and full
semantic snapshot even when context/document/viewport keys are unchanged. The
existing one-in-flight/one-replacement bounds still apply. This prevents live
same-document changes from being hidden by key coalescing; it is not yet a
semantic delta transport or native AT announcement smoke.

Writable native text controls retain live runtime UTF-16 selection offsets in
Page-owned accessibility state. Only the focused native textbox/searchbox emits
that selection through the bounded ABI, and a small render-semantics adapter
sets Flutter's otherwise non-widget-exposed `textSelection` configuration.
Unfocused controls and authored ARIA-only textboxes remain unset.

The relationship/state mapping also supports bounded `aria-owns` reparenting
for retained later nodes, while preserving parent-before-child and first-owner
constraints. Native/authored heading levels and mixed checkbox state map to
Flutter's dedicated semantics properties instead of generic labels.

Same-document updates now stage the next frame and semantic projection and swap
them atomically; neither half is exposed alone. Flutter reconciliation keys are
content-sensitive per semantic node rather than tied to the whole snapshot
generation, preserving unchanged native semantic identities while replacing
changed nodes. The ABI deliberately remains a bounded full-snapshot protocol so
Dart never becomes the authoritative accessibility graph.

The first native Linux evidence is checked in as `just linux-at-spi-smoke`. It
launches the release/AOT bundle on Xvfb with a fresh profile and an explicit
local fixture URL, traverses the host AT-SPI tree under strict node/time bounds,
filters by the launched process id, and requires BrowserCore's `DOM Basic`
heading. The environment-only initial URL changes startup intent but does not
bypass BrowserCore URL/navigation policy. Broader Orca interaction and control
action/state coverage remain release work.

Semantic focus is dispatched only when the exact context, document, runtime,
viewport, source generation, capped wire generation, node id, and advertised
capability still match; BrowserCore executes live focus events/mutation and Dart
waits for the refreshed projection. The same boundary exposes a 16 KiB-bounded
set-value action only for enabled, writable native text inputs/textareas; it
uses the live control-value and input/change event path, while password,
readonly, unsupported input types, and authored ARIA-only textboxes remain
unadvertised. Complete accessibility still requires long-tail relationship and
state mappings, document/contenteditable selection, wire-delta optimization,
broader authored-range keyboard conventions, the disabled-fieldset
first-legend exception, full ARIA presentational-role conflict handling, and
native AT and screen-reader smoke on each additional platform.

Host-service UI is Flutter-owned presentation over BrowserCore decisions:
permissions, file/directory selection, downloads, external opens, credentials,
cert/proxy diagnostics, safe areas, notifications, platform menus, and
application lifecycle. Native plugins provide OS access only through a narrow
host-service interface; policy and durable decisions remain in BrowserCore.

### 4. Linux parity, release/FlatPark packaging, and compatibility-shell removal

Linux Flutter parity requires the current shell smoke surface: context/tab
create/close/duplicate/reopen, address/search, reload/stop, history traversal,
find, zoom, diagnostics, downloads/permissions, settings/privacy controls,
session restore, shortcuts, visible WebRender content, input, viewport changes,
error recovery, and accessibility projection.

FlatPark is sequenced after the smaller basic-browser gate, not alongside its
implementation. Until broader scrolling, native IME evidence, exact find highlighting,
core navigation controls, visible rendering, and bounded recovery are proven, maintain archive
reproducibility and launch smoke only; do not prioritize registry descriptor,
review, publication, or update-channel work.

The Linux release archive is now the Flutter composition root. It uses the
official x86_64 Flutter 3.46.0-0.3.pre beta archive and verifies its framework
and engine revisions. Cargo, Pub, and rusty_v8 remain locked/pinned inputs.
`just linux-release-smoke` builds release/AOT Flutter and `libvixen_ffi.so`,
creates a deterministic archive, extracts that exact file, and requires a
bounded Impeller Xvfb launch. FlatPark pins the immutable GitHub Release URL,
size, and SHA-256 and repackages those unchanged bytes as a signed convenience
Flatpak; Vixen does not maintain a parallel OSTree repository.

The Relm4/libadwaita/custom GLArea shell remains temporarily in-tree for parity
comparison but is no longer the package entrypoint. Remove it and its
shell-specific dependencies only after parity and required host smokes pass.
Linux still carries GTK through Flutter's embedder.

### 5. Desktop expansion

Bring up macOS and Windows from the same Dart chrome and bridge contract. Adapt
only native runner, texture registration, GPU surface interop experiments,
accessibility plumbing, host services, packaging, signing, and platform UI
where necessary. A platform cannot be marked supported merely because Flutter
creates an empty window; it must run BrowserCore, V8, WebRender, real input,
semantics, profile persistence, and recovery through platform-native builds.

### 6. Android

Android begins only after desktop bridge ownership and RGBA behavior are stable.
Pin the exact rusty_v8/V8 source revision or archive, Android NDK/toolchain, Rust
target, Flutter version, and all generated source metadata. Prove reproducible
source cross-builds for each shipped ABI. No host prebuilt V8 archive may be
silently reused.

Release gates include GLES context/surface behavior, pause/resume/background and
surface-loss recovery, process recreation, touch/gesture/IME, rotation and
device-pixel-ratio changes, Android accessibility, safe storage/network policy,
and split-ABI output. Measure each ABI independently and prove the store artifact
does not duplicate unrelated ABIs.

### 7. iOS Simulator track

Vixen targets the Apple Silicon iOS Simulator for development, demos, and
cross-form-factor testing. It does not target physical iPhone/iPad hardware,
TestFlight, or App Store distribution. Build Rust and rusty_v8 for
`aarch64-apple-ios-sim`; the simulator target retains V8's JIT and WebAssembly
support, so Vixen keeps one `deno_core`/V8 JavaScript and WebAssembly path across
its declared targets.

The simulator track proves BrowserCore startup, navigation, JavaScript,
WebAssembly, rendering, simulated lifecycle, touch/IME, accessibility projection,
host services, and repeatable Flutter runner builds on an Apple Silicon macOS
host. Size and performance are measured for regression visibility but are not
mobile-store release budgets.

There is no JavaScriptCore, WKWebView, WebKit, portable alternate Wasm runtime, or
physical-device fallback in this target. Supporting physical iOS later requires a
new ADR and a separate runtime/distribution feasibility decision.

## Shared GPU texture experiments

RGBA is deliberately simple and portable, but likely too expensive forever.
After RGBA correctness and measurements exist, evaluate platform-specific
shared GPU textures: DMA-BUF/EGL on Linux, IOSurface/Metal on Apple platforms,
D3D shared resources on Windows, and Android hardware buffers/EGL images on
Android. Each experiment must prove synchronization, lifetime, color/alpha
behavior, resize/surface-loss recovery, driver coverage, and measurable wins.

These are transport implementations behind one frame contract. They must not
fork WebRender behavior, make Dart own WebRender resources, or remove the RGBA
diagnostic/reference path before the optimized path is proven.

## Artifact size is a product goal

Small GUI artifacts are a first-class measured goal, not a slogan. Do not adopt
invented byte limits. For every platform and shipped ABI/architecture:

1. Build a release/AOT/stripped **hello-Flutter** application with the same
   Flutter version, runner mode, architecture, and packaging method.
2. Build **Flutter + Vixen** with the same controls.
3. Attribute compressed download, unpacked/install, executable, and runtime-
   shared costs to Flutter engine/ICU, Dart AOT and assets, native runner/plugins,
   BrowserCore/Rust dependencies, V8/ICU/snapshots, WebRender/GPU dependencies,
   resources, packaging metadata, and symbols.
4. Report deltas from hello-Flutter and from the prior accepted Vixen build.
5. Record exact toolchain/lock/source revisions, commands, architecture, hashes,
   strip/LTO/AOT settings, and whether system/shared runtimes are excluded.

Release GUI bundles use Flutter release/AOT mode, Rust release settings with
strip and LTO, and platform-native dead-code stripping where reproducible.
Bundles must not contain debug Flutter engines, unstripped symbols, duplicate
ABIs, development snapshots, test fixtures, headless/CDP/WPT executables, source
archives, build tools, or caches unless a documented release requirement proves
they belong there. Symbol packages may be retained separately.

After representative clean reports are reproduced, adopt warning thresholds
first. Turn a threshold into a hard budget only after review establishes normal
variance, component ownership, comparison statistics, platform/ABI scope, and an
explicit product override policy. Rebaseline only for a documented dependency or
product tradeoff; never hide growth by changing attribution.

The Linux raw-bundle foundation is checked in: a controlled hello-Flutter peer,
SHA-256-pinned rusty_v8 input, clean release/AOT recipes, and a
component/delta analyzer that rejects debug artifacts and mismatched shared
Flutter engine/ICU files. The recipes use the local GNOME 50 builder container
for CMake/Ninja/GTK while mounting the pinned Rust toolchain read-only. They
intentionally report `flatpak_evidence: false`.
The first clean x86_64 raw-bundle report is linked from `BASELINES.md`;
independent reproduction, compressed/install accounting, finer native linker
attribution, and a reviewed baseline for the FlatPark package remain
required.

## Cross-cutting acceptance

Every platform must eventually prove:

- one BrowserCore lifecycle and matching GUI/headless behavior where applicable;
- visible WebRender output through the current transport, without a second web
  renderer;
- bounded FFI buffers, queues, frames, snapshots, and semantics updates;
- input/viewport/IME/focus and lifecycle recovery;
- BrowserCore accessibility projection through Flutter Semantics and native AT;
- host-service policy, persistence, and safe failure behavior;
- release packaging, signing/store policy where applicable, and reproducibility;
- hello-Flutter versus Flutter+Vixen component-attributed size reports; and
- compatibility, performance, memory, frame, and known-gap reports named by
  platform and ABI.

Platform work proceeds alongside core browser correctness. It must not freeze
the live document/runtime, loader, fonts/images/layout, security, WPT, CDP, or
real-site reduction programs. Prefer bridge slices that expose those shared-core
improvements to every shell rather than shell-only feature breadth.

## External evidence

- [Flutter supported deployment platforms](https://docs.flutter.dev/reference/supported-platforms)
  lists Android, iOS, Windows, macOS, and Linux as supported native deployment
  platforms.
- [Flutter desktop support](https://docs.flutter.dev/platform-integration/desktop)
  and the [pinned Flutter beta Linux runner template](https://github.com/flutter/flutter/tree/677d472756f83c14371dd8cc624387065f3d32a7/packages/flutter_tools/templates/app/linux.tmpl)
  describe the native desktop/GTK runner substrate.
- [Dart native interoperability](https://dart.dev/interop/c-interop) documents
  Dart FFI; [Flutter `Texture`](https://api.flutter.dev/flutter/widgets/Texture-class.html)
  and [Semantics](https://api.flutter.dev/flutter/widgets/Semantics-class.html)
  are the presentation integration points.
- [FlatPark's publishing guide](https://flatpark.org/contributing/) documents
  checksum-pinned repackaging of official release archives and package review.
- [Current rusty_v8 source-build guidance](https://github.com/denoland/rusty_v8#build-v8-from-source)
  documents Android source cross-compilation and the
  `aarch64-apple-ios-sim` simulator target, which retains JIT support. Vixen uses
  that simulator target rather than the physical-device configuration.
