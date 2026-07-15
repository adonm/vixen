# Vixen Flutter shell

This package contains Vixen's only rendered frontend. BrowserCore owns browser,
DOM/runtime, computed-style, resource-policy, event, profile, and accessibility
meaning. Flutter owns formatting, Paragraph/Canvas scenes, geometry, hit testing,
scroll mechanics, semantic bounds, capture, chrome, and presentation.

## Entrypoints

`lib/main.dart` selects one of three compositions:

- normal Yaru browser chrome;
- page-only automation with an explicit URL/viewport/output; or
- long-lived chrome-less CDP.

All three use the same formatter, commit state, and `RenderCommitPainter`. The
normal body is `BrowserContentSurface`; a missing/stale/retired commit shows
`Renderer commit unavailable` and never falls back to native pixels.

## Bridge

`NativeBrowserController` owns one worker isolate and one opaque C ABI browser
handle. Commands/events are bounded copied JSON. Native output buffers use opaque
release tokens; no Rust pointer or callback crosses into Dart.

Renderer traffic has dedicated bounded update/submission/request channels for
full snapshots, exact mutations, reset/resync, commits, presentation,
`EnsureLayout`, Paragraph text queries, and hit tests. The UI isolate services
broker requests independently of the browser command worker. Navigation, stop,
close, shutdown, and V8 deadlines cancel pending layout; late responses are
inert.

## Presentation and input

`ShellCoordinator` accepts only exact source/viewport commits and acknowledges
presentation from a post-frame callback. Pointer input is answered by the
currently displayed Flutter hit-test handle. The native command includes that
commit query and optional target; raw coordinate-only input is not part of the
ABI. Keyboard and platform text input remain exact document/runtime/viewport
commands.

Physical viewport dimensions are bounded to 4096 per axis and a 64 MiB area
budget. Flutter logical coordinates are normalized into the exact commit
viewport. Hidden or lifecycle-retired views do not dispatch input or resurrect
late commits.

## Accessibility and text input

BrowserCore accessibility snapshots provide role/name/value/state,
relationships, focus, selection, input type/action, and allowed actions. Flutter
commit semantics provide displayed bounds. BrowserCore does not fabricate layout
bounds. Focused writable controls attach the platform text-input client and
reconcile UTF-16 selection/composition state.

Semantic actions are generation checked. Pointer-like activation uses displayed
commit hit testing; focus/value/range actions retain their BrowserCore capability
checks. Accessibility metadata refresh is independent of scene capture.

## Automation and fixtures

Page-only automation and rendered CDP capture direct Flutter scenes after exact
presentation; browser/runner/compositor chrome cannot enter those PNGs. The
fixture manifest routes `flutter-js-eval`, `layout-box`, `visual-hash`, and
`ref-equivalent` checks through this host. Native headless executes only
source/runtime checks.

## Linux runner

The runner requires native Wayland and rejects X11/XWayland. It has no
pixel-buffer texture plugin, frame pool, EGL surface, or native renderer. The GTK
headerbar is a startup fallback only; Yaru renders the in-scene titlebar/chrome.

## R7 deletion

Deleted components include WebRender/gleam, `GlContext`, both EGL owners, native
visual headless, Rust layout/display-list/paint owners and paint helpers, RGBA
frame ABI/worker transfer, Linux texture presentation, raw coordinate input, and
obsolete tests/gates.

## Verification

```bash
flutter analyze
flutter test --enable-impeller --dart-define=VIXEN_REQUIRE_IMPELLER=true
just test-r7
just gate-r7
```

The release Linux build additionally requires CMake and the standard Flutter
Linux toolchain.