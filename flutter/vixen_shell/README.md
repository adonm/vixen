# Vixen Flutter shell

Vixen's first web renderer and browser chrome target on Linux. BrowserCore owns
profiles, contexts, navigation, committed DOM/V8, Stylo computed styles,
resource/security policy, events, persistence, and accessibility meaning.
Flutter owns bounded CSS formatting, Paragraph/Canvas scenes, geometry, hit
testing, text/scroll/semantic-bound commits, capture, chrome, and presentation.
The runner explicitly enables Impeller. Vixen uses public Flutter scene APIs and
does not treat a Skia-backed launch as renderer/release evidence.

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

## Linux commit presentation contract

The normal GUI and both automation entrypoints use the same Flutter formatter
and `RenderCommitPainter`. The old RGBA frame ABI, Dart worker transfer, Linux
pixel-buffer texture plugin/presenter, retry pool, and FFI EGL owner have been
deleted. A missing, stale, retired, or hidden commit shows the
renderer-unavailable placeholder; it never falls back to native renderer pixels.

Physical viewport dimensions remain bounded to 4096 per axis and a 64 MiB area
budget before BrowserCore source, input, accessibility metadata, or Flutter
formatting work is requested. Accessibility metadata is refreshed independently
of presentation so commit-bound semantic actions and platform text input remain
available without a legacy frame pairing.

## Target renderer contract

BrowserCore emits bounded mutation batches with exact context/document/source/
style/viewport/resource revisions. The Flutter renderer applies only the named
base revision, requests a full snapshot on a gap, builds Vixen CSS box/anonymous
trees, lays out text with `dart:ui` Paragraph, paints a Canvas scene, and returns
one atomic commit containing basic geometry, an opaque Flutter-side hit-test
handle, text-query state, scroll state, semantic bounds, and truncation. A
separate `Presented(commitId)` controls visible input and Semantics identity.

Common geometry is copied back for synchronous BrowserCore DOM/CSSOM/CDP reads;
Paragraph-specific caret/range queries use a bounded renderer service. Same-task
mutation plus geometry uses a dedicated `EnsureLayout` request/response broker
that the Flutter renderer can service while V8 waits, without direct callback or
BrowserCore re-entry. The chrome-less Flutter entrypoint uses this same formatter
and captures exact commits under Cage.

Flutter hit-tests the displayed commit; BrowserCore validates the target and owns
DOM dispatch, cancellation, and default actions. Flutter owns mechanical scroll
geometry, while BrowserCore owns script intent, event effects, history, and
persistence. BrowserCore semantic descriptors combine with commit bounds only for
the displayed commit.

## Input contract

The content surface maps Flutter logical pointer/wheel coordinates into the exact
physical renderer viewport. Commands carry current context, document, and runtime
ids; displayed-commit pointer input carries a Flutter hit-test query and
BrowserCore validates the returned target. Pointer and key events use a
serialized, bounded 64-event queue. A stale
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

From the repository root, install the pinned Flutter 3.47.0-0.1.pre beta through
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
