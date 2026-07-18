# Executable gates and evidence

This file is intentionally not a second roadmap. Product order and future
milestones live in [`ROADMAP.md`](ROADMAP.md); historical phase instructions live
in [`PLAN.md`](PLAN.md); measured compatibility lives in
[`COMPAT.md`](COMPAT.md). This file answers only: “which checked-in command proves
which layer today?”

**ADR-022 transition status:** R1–R7 are checked in. GUI and automation use one
Flutter formatter/commit/painter; synchronous geometry and recovery are landed;
the native/Rust renderer, frame transport, raw coordinate input, and obsolete
gates are deleted.

## Gate index

| Command | Current evidence |
|---------|------------------|
| `just test-api` | R1 renderer protocol v1 DTO/reference-state tests: typed nonzero ids, exact compound revisions and viewport values, bounded snapshot/mutation/resync, atomic commit/presented identity, immutable geometry, opaque handle retirement, bounded hit/text/scroll exchanges, displayed-commit input, and stale/late/replayed semantic-action rejection; model-only, not ABI/Dart/Flutter evidence |
| `just test-flutter-formatter-impeller` | R3 test-only data-oriented formatter over `dart:ui` Paragraph/Canvas/Picture/Scene and encoded PNG decode, with exact Impeller-requested RGBA hash, run/line geometry, hit/text/scroll/multi-rect semantic commit state, atomic mutation/presentation, failed/superseded-build disposal, stale/resync/reset and explicit release tests; not production cutover or Linux runner backend proof |
| `just gate-alpha` | formatting, all-target/all-feature Clippy, host workspace checks, generated WebIDL/runtime seams, BrowserCore ownership tests, BrowserCore-backed committed fixture runner, and stable crate-boundary allowlist |
| `just gate-architecture` | leaf-crate/frontend dependency rules; CDP has no renderer owner and rendered composition belongs only to Flutter |
| `just test-flutter-controller` | Safe controller and native boundary crate tests: one non-clone BrowserCore/event owner, immediate navigation acceptance, exact terminal events, active-load stop, contexts/profile session, and C ABI unit/integration coverage; not Dart or Flutter proof |
| `just gate-native-abi` | Builds `vixen-ffi` library forms and runs focused ABI v1 layout/header, opaque handle, bounded UTF-8/JSON command, stable response/event/error, event-sequence, output-buffer ownership, panic containment, and the R2 renderer poll/respond/submit/shutdown surface with bounded async updates/submissions, total in-flight request saturation, deadlines, cancellation, blocked-worker progress, shutdown wakeup, strict correlation, and retained-buffer release; native C ABI evidence only |
| `just gate-flutter-shell` | pinned Flutter/Yaru shell formatting, analysis, unit/widget tests, exact commit presentation/input/Semantics, lifecycle retirement, native bridge smoke, and Linux source evidence; no frame/texture fallback |
| `just gate-smoke` | reviewer baseline: formatting, clippy, host checks, and all host-runnable tests |
| `just gate-push` | hk pre-push integration point: alpha, phase-6 runtime, smoke, and diff checks |
| `just gate-webidl` | generated WebIDL constructor/prototype coverage plus headless/CDP runtime-host integration |
| `just gate-phase0` | workspace/API DTO and trait-shape foundation |
| `just gate-phase1` | network/store tests, audit, and security fuzz targets |
| `just gate-phase2` | `deno_core` runtime and headless eval seam |
| `just gate-phase3` | HTML/selector/cascade behavior and CSS fixture profile |
| `just gate-phase6` | engine host-family tests, WebIDL, headless runtime, and CDP runtime integration |
| `npm test` | bounded-process, timeout, percentile, `/proc` parser, hash, and recursive-size unit tests used by the baseline tools |
| `just wpt-profile <profile> <root>` | optional external profile execution after fail-closed validation of the canonical repository, full pinned commit, clean checkout root, and sparse-path coverage |
| `just test-browser-core` | BrowserCore owner/thread/generation proof for contexts, navigation cancellation, DOM/V8, resources, profile state, accessibility meaning, and headless source/runtime adapter; no layout or paint evidence |
| `just compat-report` | current BrowserCore-backed committed fixture/profile counts and per-source/category output |
| `just fuzz-security` | URL, CSP, cookie, and HTML parser fuzz targets at the configured run count |
| `just audit` | `cargo audit` plus `cargo deny check` |
| `just linux-release-smoke` | pinned x86_64 Flutter `3.47.0-1.0.pre-160`/Dart 3.14 GTK4 release/AOT plus Rust bridge build; exact GTK4 engine hash, no GTK3/native Yaru plugins, stripped ELFs, deterministic archive creation, clean extraction, and Impeller-aware Cage/headless-Wayland launch smoke |
| `just linux-at-spi-smoke` | real release/AOT GTK4 Flutter bundle in Cage's headless Wayland compositor with a fresh BrowserCore profile and local fixture; bounded process-filtered AT-SPI traversal must observe the BrowserCore-derived `DOM Basic` heading and `/proc` must show GTK4 with no GTK3; Linux native AT evidence, not a screen-reader matrix |
| `just linux-interaction-smoke` | real release/AOT GTK4 Flutter bundle in deterministic headless-window geometry with process-filtered AT-SPI contenteditable role/state/positive-local-bounds evidence and native-pointer focus → DOM → newer-commit checks; physical address entry visibly navigates to the controlled fixture, native back/forward and reload restore BrowserCore-owned root/nested offsets, a gated FIFO read proves the visible stop control cancels an active navigation and recovers the prior page, wtype drives IBus Mozc/GTK preedit+commit for native/contenteditable hosts, and a wlr virtual pointer proves nested wheel ownership/cancellation/root chaining; script/accepted-wheel DOM offsets correlate with newer exact Flutter commit ids while canceled wheel preserves the returned offset; the pinned engine exposes no AT-SPI Action interface, and this is one controlled Linux interaction proof rather than an IME/device matrix |
| `just linux-automation-smoke` | same release/AOT executable runtime-selects the page-only host under Cage, projects the controlled fixture's renderable full DOM/resolved styles/stable element ids through the renderer protocol, bypasses profile tabs and browser chrome, acknowledges one exact Flutter commit, and writes its direct scene PNG at 320×240 and 480×300; validates Impeller, strict PNG structure/dimensions, real document content, pinned full-scene hashes, and bounded exit; direct scene serialization excludes browser/runner/compositor chrome |
| `just flutter-cdp-playwright-smoke` | release/AOT Flutter host under Cage owns the sole BrowserCore and an in-process `vixen-cdp` subscriber; focused external Playwright proof obtains layout from Flutter commits, writes stable live DOM/attribute/collection/CSSOM objects, proves parser classic/static-dependency/dynamic-dependency/module/microtask/task ordering, matches same-task and CDP geometry/attributes/nodes, pins each exact scene, routes pointer input through Flutter hit testing, keeps 320×240 and 480×300 targets independent, and forces renderer reset/full-resync to a byte-identical scene; no browser/compositor chrome in direct PNGs |
| `just flutter-fixture-manifest` | one release/AOT Flutter host and BrowserCore execute all 270 fixtures / 2,027 checks in manifest order; each fixture gets an isolated target in the same core, 1,868 native-safe source/runtime checks use typed BrowserCore inspection, while 19 Flutter JS geometry checks, 104 layout boxes, 25 Flutter visual hashes, and 11 exact-pixel references use the matching presented Flutter commit |
| `just gate-r5` | complete R5 product gate: bounded one-shot scene capture, shared-core external Playwright/CDP input/capture/isolation/loss recovery, and the complete Flutter-hosted fixture manifest |
| `just test-r6` | focused R6 exact source diff, same-task DOM/style mutation → `EnsureLayout` → matching commit geometry, repeated-read reuse, Paragraph Range/caret queries, blocked-command broker progress, cancellation/late-reply races, malformed commit, and full-resync recovery evidence across Rust and Dart |
| `just gate-r6` | complete R6 gate: every R5 rendered fixture/CDP/Cage proof plus `test-r6` synchronous layout and recovery evidence |
| `just test-r7` | R7 absence scans plus native tests, Rust clippy, C header syntax, manifest/script validation, Dart format/analyze, and full Impeller-requested Flutter tests |
| `just gate-r7` | complete renderer-transition gate: every R5/R6 rendered product proof plus R7 cutover/deletion evidence |
| `just size-headless` | structured logical/allocated size, file count, and SHA-256 for the headless release binary |
| `just size-flutter-linux` | controlled release/AOT build and component-attributed raw-bundle comparison against the checked-in hello-Flutter peer; measurement-only and not FlatPark package evidence |
| `just baseline-headless` / `just baseline-headless-json` | per-scenario latency and Linux process-memory measurements for committed startup, navigation/runtime, layout, paint, and screenshot controls |
| `just baseline-flutter-linux` / `just baseline-flutter-linux-json` | release/AOT Flutter exact-commit startup/capture/memory plus serialized mutation/input-to-commit frame timing under Cage; measurement-only, software rendered, and outside `gate-push` |
| `just baseline-flutter-linux-hardware` / `just baseline-flutter-linux-hardware-json` | same bounded workload without the software override; fails unless the same Wayland display reports a non-software EGL renderer and records its GPU/driver fingerprint; one-host evidence, not a matrix or budget |
| `just baseline-profile-growth` | opaque temporary profile growth at init/repeated/unique/storage checkpoints with localStorage reopen proof |
| `just baseline-beta` | hermetic local headless scenarios, profile growth, and headless artifact size; measurement-only and outside `gate-push` |

## Evidence rules

- Run the cheapest focused crate test while editing, then the relevant gate above.
- After R7, renderer work must use the Flutter source/commit/query boundary; do
  not restore deleted native/Rust rendering ownership.
- A pure unit test proves an algorithm. A browser claim also needs a shared-core
  integration path, fixture/profile, external automation smoke, or GUI smoke.
- Fixture behavior changes update `COMPAT.md` from `just compat-report`; do not
  hand-invent counts.
- ADR-017 frontend ownership migration is enforced by `gate-architecture`;
  subsequent lifecycle work adds cancellation/partition/live-document evidence
  without restoring direct frontend composition.
- Released Linux shell changes use `just linux-release-smoke`. FlatPark package
  submission and verification follow only after the Linux basic-browser gate;
  an immutable GitHub Release alone does not make registry publishing a current
  priority. Flutter is the only rendered frontend target and parity concern.
- `just gate-native-abi` proves the handwritten C ABI/header/wire/buffer ownership
  milestone over the same safe controller. `just gate-flutter-shell` adds Dart,
  widget, worker-isolate, commit-painter, and live native smoke evidence. It
  proves physical viewport, pointer/wheel/keyboard routing, monotonic host
  focus/visibility/lifecycle state, and the bounded
  BrowserCore-to-Flutter Semantics hierarchy with bounded descriptions, three
  non-tree relationships, native/authored range actions, live regions, and
  event-driven full projection refresh. `linux-at-spi-smoke` adds first native
  Linux AT evidence; `linux-interaction-smoke` adds the controlled native IME and
  basic-navigation vertical. Neither proves complete screen-reader coverage,
  packages, broader release behavior, or non-Linux GUI support; use
  `FLUTTER_SHELL.md` for remaining gates.
- Size/performance thresholds become gates only after a representative baseline,
  environment, and comparison method are committed.
- Hosted `ci.yml` runs architecture/native-ABI checks, Node baseline tests, the
  workspace checks/tests, and the release native interaction/archive smoke;
  the external Playwright/CDP smoke remains a local/release gate. Its separate
  security job runs `cargo audit` and `cargo deny check`.
  `fuzz.yml` runs all four existing fuzz targets on a bounded weekly/manual CI
  budget and retains crashes. The one-million-iteration local/release command
  remains `just fuzz-security`.

## Current measured anchors

- Compatibility baseline: **270 fixtures / 2,027 checks / 100% passing**. R8
  reproduced all **1,868 native-safe checks** and then the full **2,027-check
  release/AOT Flutter-hosted manifest** on 2026-07-16. `COMPAT.md` is
  authoritative.
- Post-R7/Yaru Linux x86_64 Flutter raw-bundle reference:
  **21,398,668-byte hello / 85,377,960-byte Vixen / 63,979,292-byte delta**,
  plus a **31,913,890-byte** deterministic release archive; measurement-only,
  not independently reproduced, and not FlatPark package evidence. The Vixen
  bundle is 131,560 bytes smaller than the historical pre-R7 report; see
  `BASELINES.md` for component and control-version attribution.
- Post-R7 release/AOT renderer version-2 references: five software and five
  physical AMD/Mesa samples each joined **45 exact interaction frames**. Software
  median mutation/mouse-release/total-frame values are **15.402 ms / 26.364 ms /
  2,587 µs**; hardware values are **14.527 ms / 25.269 ms / 2,590 µs**. Cage
  reported no refresh rate. Single-host, measurement-only, not a budget or
  GPU/driver matrix; see `BASELINES.md`.
- Post-R7 profile-growth reference: five repeated and five unique visits caused
  **8,192 bytes** and **0 bytes** of allocated growth respectively; a persisted
  65,536-byte localStorage payload added **139,264 bytes** and passed reopen.
  Single-host, measurement-only, and not a budget; see `BASELINES.md`.
- R8 native-host checkpoint: the release/AOT Cage corridor passed with real IBus
  Mozc preedit/commit in native and contenteditable controls, positive Flutter
  AT-SPI editor bounds **(8, 187, 40, 20)**, unchanged native `Focus` → DOM focus
  → same-document commit **18 → 20**, wheel cancellation/scroll/navigation
  recovery, and clean exit. Single controlled Fedora host, not an IME,
  assistive-technology, compositor, or device matrix.
- External automation contract: [`CDP_PLAYWRIGHT_SMOKE.md`](CDP_PLAYWRIGHT_SMOKE.md).
- Browser ownership/cancellation vertical: `just test-browser-core` (engine,
  headless, and FFI controller adapters through the production command/event
  handle).
- Release requirements: [`ACCEPTANCE.md`](ACCEPTANCE.md).
- Measurement methods, report schemas, acceptance policy, and current gaps:
  [`BASELINES.md`](BASELINES.md).
- Five-platform Flutter GUI contract and gate plan:
  [`FLUTTER_SHELL.md`](FLUTTER_SHELL.md).

When a gate and its description diverge, fix this table in the same change as the
recipe. Do not copy already-landed feature inventories back into the roadmap.
