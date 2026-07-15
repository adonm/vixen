# Vixen implementation plan

This document describes the active plan after ADR-022 R7. Historical plans for
the deleted Rust layout/display-list/WebRender/EGL/RGBA architecture are no
longer normative and were removed with that implementation.

## Landed foundation

R1–R7 are complete:

- BrowserCore owns contexts, navigation/history, DOM/cascade/runtime, profile and
  network policy, input intent, resource acceptance, and accessibility meaning.
- BrowserCore publishes bounded full renderer snapshots and deterministic
  incremental mutations with exact context/document/source/style/viewport/
  resource generations.
- Flutter owns formatting, Paragraph text measurement, Canvas/Picture/Scene
  paint, root and nested scroll mechanics, hit testing, semantic bounds, find
  geometry, and direct scene PNG capture.
- Synchronous CSSOM geometry waits for the matching Flutter commit and supports
  cancellation, timeout, one bounded resync, and late-response rejection.
- GUI, page-only automation, rendered CDP/Playwright, and rendered fixture checks
  use the same Flutter formatter and commit painter.
- Native `vixen-headless` is text/runtime/profile-only. Renderer-dependent
  operations fail closed instead of inventing geometry or pixels.
- R7 deleted WebRender/gleam, `GlContext`, both EGL owners, native screenshots,
  Rust layout/display-list/paint and paint-helper modules, RGBA frame transport,
  Linux pixel-buffer texture presentation, raw coordinate input, and obsolete
  gates/tests.

The current ownership contract is specified in `ARCHITECTURE.md`, the renderer
and shell contract in `FLUTTER_SHELL.md`, acceptance in `ACCEPTANCE.md`, and
compatibility evidence in `COMPAT.md`.

## Immediate queue: R8 stabilization and rebaseline

1. Reproduce the complete 270-fixture Flutter manifest after the R7 cutover,
   including `flutter-js-eval`, `layout-box`, `visual-hash`, and
   `ref-equivalent` checks.
2. Reproduce external rendered CDP/Playwright and two-viewport target isolation
   under the release Flutter host and Cage.
3. Reproduce root/nested horizontal and vertical scrolling, find reveal,
   commit-bound pointer input, keyboard defaults, text input/IME, lifecycle
   retirement, renderer reset, and exact scene recovery.
4. Reproduce accessibility role/state/action/bounds behavior with native Linux
   AT evidence and no BrowserCore layout fallback.
5. Rebaseline release size, startup, navigation, memory, scene capture, and
   package attribution. Pre-R7 renderer measurements are historical only.
6. Fix cutover regressions before widening standards or product breadth.

## Post-stabilization priorities

### Compatibility

- Expand CSS formatting and painting only in the Flutter formatter.
- Expand Paragraph shaping, writing modes, bidi, selection/caret, and font
  fallback with exact commit tests.
- Expand images and replaced elements after BrowserCore policy/resource
  acceptance; decoding and intrinsic rendered geometry remain Flutter-owned.
- Increase WPT coverage with explicit source/runtime versus rendered ownership.

### Interaction and accessibility

- Complete pointer, touch, gesture, drag/drop, selection, nested/smooth scroll,
  and overscroll behavior through commit-bound input.
- Complete Linux IME and accessibility device matrices, then add equivalent
  evidence for each supported platform runner.
- Keep BrowserCore semantic meaning independent of scene capture while requiring
  displayed Flutter commits for bounds and pointer-like semantic activation.

### Performance and hardening

- Measure formatter build, incremental mutation, Paragraph query, scene capture,
  bridge queue, and BrowserCore owner-thread latency.
- Enforce release size, startup, memory, and renderer recovery budgets.
- Add process/sandbox boundaries only with an explicit threat model and bounded
  protocol; do not recreate renderer ownership in native code.

### Product and distribution

- Finish browser chrome behavior, downloads, settings, permissions, and session
  UX after the stabilization gate.
- Add non-Linux runners one at a time with native build, input/IME/AT, package,
  and sustained smoke evidence.
- Complete signed packaging, update, rollback, provenance, and release channels.

## Invariants for all new work

1. There is one production renderer: Flutter.
2. BrowserCore never fabricates rendered geometry, hit tests, semantic bounds,
   or screenshots.
3. Pointer input names an exact displayed commit and Flutter hit target.
4. Render commits and queries are bounded, generation checked, cancellable where
   blocking, and fail closed when stale.
5. Renderer-dependent fixture checks run in the Flutter host; native runners do
   not claim rendered evidence.
6. No compatibility shim may restore deleted WebRender/EGL/frame/texture/Rust
   layout-paint details.
7. Prefer deletion and direct data flow over parallel ownership or abstraction.

## Gates

Focused final-cutover proof:

```bash
just test-r7
```

Full composed rendered proof:

```bash
just gate-r7
```

`test-r7` checks source/dependency absence, native tests, clippy, C header syntax,
manifest/script validity, Dart formatting/analyze, and the complete
Impeller-requested Flutter suite. `gate-r7` first preserves all R5/R6 release,
Cage, fixture, CDP, synchronous-layout, cancellation, and recovery evidence.

A Linux release build additionally requires CMake and the standard Flutter Linux
toolchain.