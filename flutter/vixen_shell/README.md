# Vixen Flutter shell

Vixen's primary browser chrome for Linux. Flutter owns presentation while the
Rust `BrowserCore` remains the sole owner of profiles, contexts, navigation,
documents, network policy, and ordered browser events.

The production entry point uses a persistent Dart worker isolate and the
handwritten `vixen-ffi` C ABI. It fails closed if the bundled native library or
ABI is unavailable. Unit and widget tests inject a scripted controller; the
production binary never silently falls back to it.

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
Uncanceled wheel events apply one bounded Page-owned root scroll offset used by
paint, hit testing, and accessibility bounds; fixed-position content remains
anchored. Text/IME, nested and keyboard/touch/script scrolling, CSS/physical
scale correctness, and lifecycle/surface recovery remain follow-up work.

## Accessibility contract

BrowserCore supplies a bounded projection of authoritative roles, names, values,
states, focus, tap/focus actions, physical bounds, and nearest emitted semantic-parent
relationships. Bounded `aria-controls`, `aria-describedby`, and `aria-details`
targets plus descriptions cross the ABI; controls map to stable Flutter semantic
identifiers. Native and authored ranges expose bounded values and route
increase/decrease through the exact-generation live runtime action path. Focused
writable native text controls also project live UTF-16 selection offsets. The ABI exposes at most 192 document-order nodes and tags the
exact projection with a deterministic mutation generation. The coordinator
publishes it only with the matching frame/context/document/viewport generation;
Flutter maps the hierarchy to keyed nested `Semantics` nodes and routes taps back
through BrowserCore hit testing. Focus requires exact source and capped-wire
generations and executes through the live runtime before a refreshed projection
is published. A 16 KiB-bounded `onSetText` path uses the same generation checks
and live value/event machinery for enabled writable native text controls;
passwords, readonly controls, unsupported types, and ARIA-only textboxes are not
advertised. Live regions and event-driven same-document full refresh are also
implemented, as are bounded `aria-owns` reparenting, heading levels, and mixed
checkbox state. Same-document refreshes atomically swap frame/semantics pairs,
and content-sensitive keys reconcile only changed nodes. Long-tail relationships,
document/contenteditable selection, and broader screen-reader coverage remain
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
```

That path builds release/AOT Flutter and the Rust bridge, creates and extracts
the deterministic GitHub Release archive, and requires an Impeller launch log.
FlatPark repackages the released archive unchanged; broader host-matrix, native
AT, portal, and accepted size-baseline gates remain separate. FlatPark
submission and publishing are deferred until visible navigation/rendering,
engine-owned scrolling, text/IME, core navigation controls, find/zoom, and
bounded recovery make this a basic usable browser.

Set `VIXEN_FFI_LIBRARY` to an absolute `libvixen_ffi.so` path only for the
native bridge smoke test. Normal Linux bundles load `lib/libvixen_ffi.so`
relative to the executable.
