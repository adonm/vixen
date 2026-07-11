# Flutter shell plan

This is the authoritative plan for Vixen's primary GUI shell on Linux, macOS,
Windows, Android, and the Apple Silicon iOS Simulator. It defines migration order, ownership, rendering,
FFI, accessibility, packaging, artifact measurement, and platform gates. Product
scope remains in [`PROJECT_DIRECTION.md`](PROJECT_DIRECTION.md), browser-engine
ownership in [`ARCHITECTURE.md`](ARCHITECTURE.md), and accepted tradeoffs in
[`DECISIONS.md`](DECISIONS.md) ADR-018.

## Status and evidence boundary

**Partially scaffolded target:** Flutter is not installed in this workspace.
`vixen-ffi` provides a non-clone safe Rust controller over one BrowserCore
handle/event receiver and the first handwritten C-compatible ABI milestone over
that same controller. ABI v1 exports registry-backed opaque browser handles,
bounded copied UTF-8/JSON commands, polling/timeout event consumption, stable
tagged responses/events/errors, and explicitly size-capped, tokenized Rust-owned
output buffers. There is no Dart binding, checked-in Flutter application, fake
Flutter shell, generated runner, external texture, Semantics bridge, Flutter
build, Flutter Flatpak, mobile build, or five-platform smoke result yet. The
current Relm4/libadwaita/GTK shell and its Flatpak are the executable GUI
compatibility baseline until the Linux Flutter shell reaches parity.

Flutter 3.44 officially supports native deployment to Android, iOS, Windows,
macOS, and Linux. That establishes a supported shell substrate, not proof that
Vixen's Rust/V8/WebRender stack builds, packages, performs, or satisfies store
policy on each target. Every Vixen platform remains gated below.

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

| Platform | Flutter 3.44 substrate | Initial Vixen integration | Required release evidence | Current Vixen status |
|----------|------------------------|---------------------------|---------------------------|----------------------|
| Linux | Official native desktop support | Dart FFI bridge, bounded RGBA external texture, Flutter input/viewport, GTK-backed Flutter Linux embedder | Flutter parity with the compatibility shell; offline source-built Flatpak; GPU/driver, portal, accessibility, size, and performance reports | GTK/Relm4 shell exists; no Flutter build |
| macOS | Official native desktop support on x64/arm64 | Same bridge and RGBA contract in a native Flutter runner | Native BrowserCore/V8/WebRender build, signing/notarization, input/IME, accessibility, host services, universal-or-separated architecture attribution, size/performance reports | Target; unproven |
| Windows | Official native desktop support on x64/arm64 | Same bridge and RGBA contract in a native Flutter runner | Native BrowserCore/V8/WebRender build, packaging/signing, input/IME, accessibility, host services, per-architecture size/performance reports | Target; unproven |
| Android | Official native mobile support | Same bridge, RGBA external texture first, GLES-backed WebRender, lifecycle-aware runner | Pinned V8 source archive/toolchain, reproducible source cross-build, GLES, lifecycle/background recovery, input/IME, accessibility, split-ABI packaging, size/performance proof | Committed target behind gates; unproven |
| iOS Simulator | Official Flutter iOS development substrate on Apple Silicon | Same bridge and RGBA external texture using Rust/V8 `aarch64-apple-ios-sim` | Simulator BrowserCore/V8/WebRender build, V8 JavaScript/WebAssembly, lifecycle/input/accessibility, and advisory size/performance proof | Committed simulator-only development target; unproven |

The Linux Flutter embedder uses GTK. Migrating the shell removes Vixen's direct
Relm4/libadwaita/custom `gtk4::GLArea` ownership; it does **not** promise that GTK
runtime dependencies disappear from Linux packages.

## Migration sequence

### 1. BrowserCore bridge and Linux fake shell

The first audited native portion is implemented: the landed safe Rust controller
has a narrow handwritten C-compatible ABI over the existing browser-scoped
command/event API. `just gate-native-abi` builds the native library forms and
tests C layout/header declarations, bounded JSON v1 commands, registry ownership,
event sequencing, stable errors, and panic containment. This proves the native C
ABI/header/wire ownership contract only. It does not prove Dart or Flutter,
textures, Semantics, platform runners, or packages.

The planned first Flutter harness remains a Linux Flutter chrome shell backed by
a fake bridge so tabs, routing, focus, shortcuts, dialogs, bounded event queues,
error handling, and teardown can be tested without pretending the engine is
connected. No such fake shell is checked in today.

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

Connect the same Flutter chrome to BrowserCore. WebRender renders web content to
an engine-owned offscreen target. The initial interoperability contract exports
a bounded RGBA frame for a Flutter external texture:

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

Flutter sends normalized pointer, wheel, keyboard, text/IME, gesture, focus,
viewport, scale, visibility, and lifecycle commands with context and viewport
generations. BrowserCore performs hit testing, scrolling, selection, DOM event
dispatch, and navigation effects. Platform-specific raw data may be retained in
bounded DTOs where web semantics require it.

A texture has no semantic structure. Before a Flutter shell can be called
accessible, BrowserCore must project its authoritative DOM/layout/accessibility
state into a bounded, incremental semantics tree consumed by Flutter
`Semantics`. Nodes need stable ids, roles, names/values/states, bounds,
relationships, focus, actions, text selection, live-region changes, and
generation-aware removal. Flutter maps that projection to each platform's
accessibility bridge; Dart does not infer semantics from pixels or maintain a
second DOM.

Host-service UI is Flutter-owned presentation over BrowserCore decisions:
permissions, file/directory selection, downloads, external opens, credentials,
cert/proxy diagnostics, safe areas, notifications, platform menus, and
application lifecycle. Native plugins provide OS access only through a narrow
host-service interface; policy and durable decisions remain in BrowserCore.

### 4. Linux parity, offline Flatpak, and compatibility-shell removal

Linux Flutter parity requires the current shell smoke surface: context/tab
create/close/duplicate/reopen, address/search, reload/stop, history traversal,
find, zoom, diagnostics, downloads/permissions, settings/privacy controls,
session restore, shortcuts, visible WebRender content, input, viewport changes,
error recovery, and accessibility projection.

The Linux Flatpak must be a pinned offline source build. Pin Flutter 3.44.x and
`TheAppgineer/flatpak-flutter` 0.15.0, preprocess the Flutter plus Rust manifest,
include `Cargo.lock` and declared foreign dependencies, and prove
`flatpak-builder --sandbox` completes without network access. Generated sources
are reviewable build inputs, not a substitute for lock files or source
attribution.

Only after parity, required host smokes, and artifact reports pass should the
Relm4/libadwaita/custom GLArea shell and its shell-specific dependencies be
removed. Until then it is a temporary compatibility baseline, not a second
long-term product shell. Linux may still carry GTK through Flutter's embedder.

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

- [Flutter 3.44 supported deployment platforms](https://docs.flutter.dev/reference/supported-platforms)
  lists Android, iOS, Windows, macOS, and Linux as supported native deployment
  platforms.
- [Flutter desktop support](https://docs.flutter.dev/platform-integration/desktop)
  and the [Flutter 3.44 Linux runner template](https://github.com/flutter/flutter/tree/3.44.0/packages/flutter_tools/templates/app/linux.tmpl)
  describe the native desktop/GTK runner substrate.
- [Dart native interoperability](https://dart.dev/interop/c-interop) documents
  Dart FFI; [Flutter `Texture`](https://api.flutter.dev/flutter/widgets/Texture-class.html)
  and [Semantics](https://api.flutter.dev/flutter/widgets/Semantics-class.html)
  are the presentation integration points.
- [`flatpak-flutter` 0.15.0](https://github.com/TheAppgineer/flatpak-flutter/tree/0.15.0)
  documents manifest preprocessing for pinned offline Flutter builds, including
  Cargo lock inputs and foreign dependencies.
- [Current rusty_v8 source-build guidance](https://github.com/denoland/rusty_v8#build-v8-from-source)
  documents Android source cross-compilation and the
  `aarch64-apple-ios-sim` simulator target, which retains JIT support. Vixen uses
  that simulator target rather than the physical-device configuration.
