# Flutter shell

The Flutter shell is Vixen's only rendered frontend. BrowserCore owns browser
truth; Flutter owns formatter, paint, hit testing, scroll geometry, semantic
bounds, and scene capture. R7 removed every native rendering fallback.

## Composition

The Linux application is rooted at `BrowserShell` and uses the locked
Yaru/Adwaita-blue chrome. The page body is `BrowserContentSurface`, which paints
only a current `FormatterCommitView` through `RenderCommitPainter`.

A missing, stale, retired, hidden, or viewport-mismatched commit displays the
explicit `Renderer commit unavailable` surface. It does not request native
pixels or create a texture fallback.

The Linux runner:

- requires native Wayland and rejects X11/XWayland;
- uses Flutter's GTK embedder but owns no second Rust/GTK browser widget tree;
- has no pixel-buffer texture channel, native frame pool, EGL surface, or
  compositor-specific web-content path; and
- keeps the native GTK headerbar only as a startup fallback until the Flutter
  titlebar is ready.

## Browser bridge

`NativeBrowserController` owns one worker isolate and one opaque C ABI browser
handle. The worker serializes bounded copied JSON commands/events and releases
native output buffers by opaque token. No Rust pointer or callback crosses into
Dart.

The renderer uses separate bounded channels for:

- full source snapshots and exact incremental mutation batches;
- reset/resync;
- atomic commit submission and post-frame presentation acknowledgement;
- synchronous `EnsureLayout`;
- commit-bound Paragraph text queries; and
- commit-bound hit-test/input and semantic actions.

The UI isolate services renderer broker work independently of the browser
command worker. A V8 command blocked on `EnsureLayout` therefore cannot block the
Flutter work needed to answer it. Navigation, stop, close, shutdown, and V8
execution deadlines cancel pending renderer work; late replies are inert.

## Renderer ownership

BrowserCore publishes immutable DOM topology, stable element ids, renderer-only
text ids, computed styles, accepted resources, semantic descriptors, viewport,
page zoom, and root scroll intent. Flutter validates the revision graph and owns:

- CSS block/inline/flex/grid formatting and fragmentation;
- Paragraph shaping, line breaking, caret/range geometry, and text hit testing;
- image measurement and decode at the renderer boundary;
- Canvas/Picture/Scene paint order and clipping;
- mechanical scroll extents/offsets;
- hit-test handles and local coordinates;
- semantic bounds; and
- direct scene PNG capture.

Only Flutter public `dart:ui` APIs are used. Impeller must be explicitly enabled
for accepted rendered evidence; a Skia-backed run is not renderer proof.

## Input and accessibility

Pointer coordinates are normalized from Flutter logical space into the exact
commit viewport. Pointer input crosses the C ABI only as
`dispatch_renderer_mouse_event`, carrying the displayed commit revision, query
handle, query id, point, and optional Flutter hit target. BrowserCore validates
that target before DOM dispatch. The former raw coordinate-input command is
deleted.

Keyboard and text-input commands remain generation/viewport bound. Focused
writable controls use BrowserCore-authored value/selection/input intent and the
platform text-input connection. BrowserCore authors semantic role/name/value,
relationships, focus, and permitted actions; the displayed Flutter commit owns
semantic bounds. Stale action generations and stale commits fail closed.

Accessibility metadata refresh is independent of scene capture. There is no
frame/Semantics pairing or BrowserCore layout bbox fallback.

The pinned Flutter 3.47 GTK3 bridge recursively asks its non-component
`FlViewAccessible` root for `Component` extents. The Linux runner terminates only
that root walk with view-local bounds; descendant transforms and sizes remain
the displayed Flutter semantics geometry, and Flutter's native semantic-action
dispatch remains unchanged.

## Lifecycle and recovery

Host-view commands carry a monotonic generation, physical viewport, scale,
focus, visibility, and lifecycle state. Hidden/detached/paused views retire
presentation; a late commit cannot reappear after resume. Renderer reset forces
one bounded full-source resync. Timeout, malformed commit, missed state, and
resync each receive at most one bounded recovery attempt.

## Automation

Normal GUI, page-only automation, rendered CDP, Playwright smoke, and fixture
manifest use the same formatter and painter. Chrome-less mode changes
composition and output routing only; it does not select another browser core or
renderer.

Renderer-dependent manifest checks are explicit:

- `flutter-js-eval` for JavaScript whose result needs commit geometry/scroll;
- `layout-box` for exact commit bounds;
- `visual-hash` for Flutter scene hashes; and
- `ref-equivalent` for exact Flutter reference scenes.

The native WPT/headless runner executes source/runtime checks only.

## Deleted R7 path

R7 deleted WebRender/gleam, `GlContext`, native-headless and FFI EGL, native
screenshots/incremental captures, Rust layout/display-list/paint and paint-helper
modules, RGBA frame ABI/tokens/pools, Dart frame transfer, Linux pixel-buffer
texture plugin/presenter, raw coordinate input, and obsolete recovery/gate tests.
Do not add compatibility shims for those details.

## Verification

Focused commands:

```bash
just test-r6
just test-r7
```

Full rendered composition:

```bash
just gate-r7
```

`test-r7` performs source/dependency absence scans, native tests, clippy, C header
syntax, manifest/script validation, Dart formatting/analyze, and the full
Impeller-requested Flutter suite. `gate-r7` first preserves all R5/R6 release
Cage, fixture, CDP, mutation, synchronous geometry, cancellation, and recovery
evidence.

A release Linux runner build additionally requires CMake and the normal Flutter
Linux toolchain.
