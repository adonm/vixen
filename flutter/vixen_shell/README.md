# Vixen Flutter shell

Vixen's primary browser chrome for Linux. Flutter owns presentation while the
Rust `BrowserCore` remains the sole owner of profiles, contexts, navigation,
documents, network policy, and ordered browser events.

The production entry point uses a persistent Dart worker isolate and the
handwritten `vixen-ffi` C ABI. It fails closed if the bundled native library or
ABI is unavailable. Unit and widget tests inject a scripted controller; the
production binary never silently falls back to it.

## Chrome and window contract

The locked Yaru 10.2.0 suite supplies Adwaita-blue light, dark, and high-contrast
themes plus the compatible icon buttons, menu, progress, banner, and window
controls. BrowserCore-backed tabs are embedded in `YaruWindowTitleBar`; its Linux
plugin hides the native GTK headerbar and provides drag/minimize/maximize/
restore/close operations. The runner creates that GTK headerbar first as a
fallback if Dart/plugin startup fails. The titlebar lives in the browser
`Scaffold`, so modal routes block it normally.

Address and find fields intentionally remain Material `TextField`s under the
Yaru theme. Yaru's search field does not expose all Vixen requirements: disabled
state, the exact Go/Search input actions, and the find query's 4096-byte UI cap.
This is Yaru with an Adwaita accent/style target, not a claim that Yaru is
pixel-identical to libadwaita.

The Linux GUI is native-Wayland-only. The runner exits nonzero if GTK selected
X11 or XWayland. Use `just run-flutter` in a native Wayland session or `just
run-flutter-cage` for a local isolated compositor; release and AT-SPI smokes use
the same Cage headless-wlroots shape.

## Linux frame texture contract

The native worker captures only the selected, settled BrowserCore
context/document generation. Each successful ABI frame is exact packed RGBA8,
is copied before its native token is released, and crosses to the UI isolate as
`TransferableTypedData`. Capture dimensions are physical pixels and are bounded
to 4096 per axis, 64 MiB, and one in-flight request plus one newest replacement.

The Linux runner exposes `dev.adonm.vixen/texture` using Flutter's standard
method codec:

- `create` takes no arguments and returns the registered texture ID.
- `publish` takes `{width: int, height: int, rgba: Uint8List}`. Dimensions must
  be positive and at most 4096, and `rgba.length` must equal
  `width * height * 4` without exceeding 64 MiB.
- `dispose` takes no arguments and unregisters the texture before releasing it.

There is one `FlPixelBufferTexture` per window. It owns a mutex-protected,
bounded three-buffer pool so the render-thread pointer most recently returned
to Flutter is not overwritten before a later render tick. Unsupported platforms
and channel failures show the renderer-unavailable placeholder; they never
synthesize a frame.

Current-generation BrowserCore frame and Semantics captures each retry at most
twice. The presenter likewise disposes and recreates the texture after at most
two failed create/publish attempts; exhaustion shows `Surface recovery failed`
and a newer frame gets a fresh bounded attempt. Tests use deterministic failures;
native compositor/surface-loss evidence and full app-lifecycle recovery remain.

## Input contract

The content surface maps Flutter logical pointer/wheel coordinates into the exact
physical frame viewport. Commands carry current context, document, and runtime
ids; BrowserCore performs hit testing and the wire has no caller-selected node
id. Pointer and key events use a serialized, bounded 64-event queue. A stale
generation is discarded, input effects/navigation outcomes are retained, and an
accepted event requests a new frame. Pointer cancellation clears only the
matching pending primary press and cannot synthesize a click. A monotonic
BrowserCore-owned host-view command now carries effective scale, content focus,
visibility, and Flutter lifecycle; stale updates fail, hidden/inactive views
reject input, and the live document receives focus/visibility state and events.
Uncanceled wheel and navigation-key defaults apply one bounded Page-owned root
scroll offset used by paint, hit testing, and accessibility bounds; fixed-
position content remains anchored. Arrow, Page Up/Down, Home/End, and Space
scrolling respects focused controls and page `preventDefault()` handlers.
Page scripts also use that exact root offset through bounded numeric/options
`scroll()`/`scrollTo()`/`scrollBy()`, synchronized window offsets, and root/body
`scrollTop`/`scrollLeft`; BrowserCore refreshes its CSS viewport and overflow
clamp on host-view and page-zoom changes.
Focused writable native text controls and contenteditable editing hosts attach
Flutter's platform text-input client; bounded full values and UTF-16 selection/
composing ranges cross the exact BrowserCore generation and update the live DOM.
BrowserCore projects normalized `inputmode`, input-type, and `enterkeyhint`
intent for writable hosts; Flutter maps it to platform keyboard and action
configuration, and performed actions reuse exact-generation Enter down/up. A
controlled Cage/wtype/IBus Anthy gate now proves native and contenteditable
preedit/commit plus nested wheel ownership, cancellation, and root chaining;
broader IME/device and gesture matrices, scroll restoration, CSS/physical scale
correctness, and lifecycle/native surface-loss recovery remain follow-up work.
Single-touch drags already cross platform touch slop, cancel the pending
synthetic press, and reuse the cancelable BrowserCore root-wheel path; taps remain
taps and secondary touches are ignored.

Ctrl+F and the browser menu expose a find bar backed by an exact active-
document BrowserCore command. The query is bounded to 4 KiB at the native
boundary and traverses a Page-owned, 10,000-match-bounded rendered-text result;
stale responses are discarded and the active/total result is a Flutter live
region. Enter/F3 and Previous/Next traverse with wrapping, while BrowserCore
updates the shared root scroll offset to reveal the active match and Flutter
requests a fresh paired frame/Semantics projection. BrowserCore paints orange
active and yellow other range highlights in the same display list consumed by
WebRender. Soft-wrapped phrases remain one logical match with per-run
highlights; precision remains limited by the current deterministic text metrics.

Ctrl++/Ctrl+-/Ctrl+0 and menu actions adjust a 25–500% per-context zoom owned by
BrowserCore. The core derives the CSS viewport, scales the single display list
to the physical texture, converts hit testing/wheel input to CSS coordinates,
and emits matching physical Semantics bounds. Flutter does not rescale pixels or
own page zoom state. Profile persistence and device-scale/native surface recovery
remain open.

## Accessibility contract

BrowserCore supplies a bounded projection of authoritative roles, names, values,
states, focus, tap/focus actions, physical bounds, and nearest emitted semantic-parent
relationships. Bounded `aria-controls`, `aria-describedby`, and `aria-details`
targets plus descriptions cross the ABI; controls map to stable Flutter semantic
identifiers. Native and authored ranges expose bounded values and route
increase/decrease through the exact-generation live runtime action path. Focused
writable native text controls and contenteditable hosts also project live UTF-16
selection offsets. The ABI exposes at most 192 document-order nodes and tags the
exact projection with a deterministic mutation generation. The coordinator
publishes it only with the matching frame/context/document/viewport generation;
Flutter maps the hierarchy to keyed nested `Semantics` nodes and routes taps back
through BrowserCore hit testing. Focus requires exact source and capped-wire
generations and executes through the live runtime before a refreshed projection
is published. A 16 KiB-bounded `onSetText` path uses the same generation checks
and live value/event machinery for enabled writable native text controls and
contenteditable editing hosts;
passwords, readonly controls, unsupported types, and ARIA-only textboxes are not
advertised. Live regions and event-driven same-document full refresh are also
implemented, as are bounded `aria-owns` reparenting, heading levels, and mixed
checkbox state. Same-document refreshes atomically swap frame/semantics pairs,
and content-sensitive keys reconcile only changed nodes. Long-tail relationships,
general document-range selection and broader screen-reader coverage remain
open. `just linux-at-spi-smoke` already proves that the real release bundle
exports BrowserCore's `DOM Basic` heading through the process-filtered native
Linux AT-SPI tree.

From the repository root, install the pinned Flutter 3.46.0-0.3.pre beta through
mise and run:

```sh
just setup-flutter
just gate-flutter-shell
just run-flutter
```

For the measurement-only Linux release-bundle comparison against the checked-in
hello-Flutter peer:

```sh
just flutter-size-prefetch # network-capable input staging
just size-flutter-linux    # controlled hello-versus-Vixen comparison
```

The official Linux release path uses the same GNOME 50 builder environment:

```sh
just linux-release-prefetch
just linux-release-smoke
just linux-at-spi-smoke
just linux-interaction-smoke
```

That path builds release/AOT Flutter and the Rust bridge, creates and extracts
the deterministic GitHub Release archive, and requires an Impeller launch log.
FlatPark repackages the released archive unchanged; broader host-matrix, native
AT, portal, and accepted size-baseline gates remain separate. FlatPark
submission and publishing are deferred until visible navigation/rendering,
broader engine-owned scrolling and IME/device coverage, core navigation controls,
find/zoom, and bounded recovery make this a basic usable browser.

Set `VIXEN_FFI_LIBRARY` to an absolute `libvixen_ffi.so` path only for the
native bridge smoke test. Normal Linux bundles load `lib/libvixen_ffi.so`
relative to the executable.
