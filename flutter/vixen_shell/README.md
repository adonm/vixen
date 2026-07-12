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

The Linux runner exposes `org.vixen.Vixen/texture` using Flutter's standard
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
accepted event requests a new frame. Text/IME, gestures, explicit focus/lifecycle
commands remain follow-up work.

## Accessibility contract

BrowserCore supplies a bounded projection of authoritative roles, names, values,
states, focus, tap/focus actions, physical bounds, and nearest emitted semantic-parent
relationships. The ABI exposes at most 256 document-order nodes and tags the
exact projection with a deterministic mutation generation. The coordinator
publishes it only with the matching frame/context/document/viewport generation;
Flutter maps the hierarchy to keyed nested `Semantics` nodes and routes taps back
through BrowserCore hit testing. Focus requires exact source and capped-wire
generations and executes through the live runtime before a refreshed projection
is published. A 16 KiB-bounded `onSetText` path uses the same generation checks
and live value/event machinery for enabled writable native text controls;
passwords, readonly controls, unsupported types, and ARIA-only textboxes are not
advertised. Non-tree relationships, range actions,
incremental/live updates, text selection, and native assistive-technology smoke
remain open.

From the repository root, use the pinned Flutter 3.44 SDK and run:

```sh
just setup-flutter
just gate-flutter-shell
just run-flutter
```

For the measurement-only Linux release-bundle comparison against the checked-in
hello-Flutter peer:

```sh
just flutter-size-prefetch # network-capable input staging
just size-flutter-linux    # clean build with network disabled
```

This uses the local GNOME 50 builder image with networking disabled and does not
satisfy the offline Flatpak or accepted size-baseline gates.

Set `VIXEN_FFI_LIBRARY` to an absolute `libvixen_ffi.so` path only for the
native bridge smoke test. Normal Linux bundles load `lib/libvixen_ffi.so`
relative to the executable.
