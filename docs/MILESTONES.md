# Executable gates and evidence

This file is intentionally not a second roadmap. Product order and future
milestones live in [`ROADMAP.md`](ROADMAP.md); historical phase instructions live
in [`PLAN.md`](PLAN.md); measured compatibility lives in
[`COMPAT.md`](COMPAT.md). This file answers only: “which checked-in command proves
which layer today?”

## Gate index

| Command | Current evidence |
|---------|------------------|
| `just gate-alpha` | formatting, all-target/all-feature Clippy, host workspace checks, generated WebIDL/runtime seams, BrowserCore ownership tests, BrowserCore-backed committed fixture runner, and stable crate-boundary allowlist |
| `just gate-architecture` | leaf-crate dependency rules plus frontend rules that forbid headless/FFI direct leaf composition; production frontends may use only `vixen-api` and `vixen-engine` |
| `just test-flutter-controller` | Safe controller and native boundary crate tests: one non-clone BrowserCore/event owner, immediate navigation acceptance, exact terminal events, active-load stop, contexts/profile session, and C ABI unit/integration coverage; not Dart or Flutter proof |
| `just gate-native-abi` | Builds `vixen-ffi` library forms and runs focused ABI v1 layout/header, opaque handle, bounded UTF-8/JSON command, stable response/event/error, event-sequence, output-buffer ownership, and panic-containment tests; native C ABI evidence only |
| `just gate-flutter-shell` | Exact mise-managed Flutter 3.46.0-0.3.pre beta framework/engine revisions, locked Yaru 10.2.0 Adwaita-blue chrome/in-scene titlebar, Dart formatting/analysis, 60 shell/coordinator/worker/texture/input/Semantics tests, including monotonic host focus/visibility/lifecycle state, physical wheel normalization and slop-gated single-touch dragging through BrowserCore-owned cancelable root scrolling, bounded native/contenteditable platform text/selection/composition routing, normalized `inputmode`/input-type/`enterkeyhint` keyboard and action configuration over the Enter key path, bounded BrowserCore-backed find traversal/scroll/highlighting, two-retry current-generation frame/Semantics capture and texture recreation, per-context BrowserCore-owned zoom across paint/input/Semantics, bounded descriptions and `aria-controls`/`aria-describedby`/`aria-details`/`aria-owns` relationships, heading/mixed-state mapping, native/authored range adjustment, live-region mapping, native/contenteditable text selection, atomic frame/semantics replacement, and node-level incremental reconciliation, live process-adjacent native bridge smoke, and focused native ABI/frame/input/accessibility tests; Linux source/test evidence, not a real native IME or screen-reader interaction, native surface-loss/lifecycle recovery, release/package, or non-Linux proof |
| `just gate-smoke` | reviewer baseline: formatting, clippy, host checks, and all host-runnable tests |
| `just gate-push` | hk pre-push integration point: alpha, phase-6 runtime, smoke, and diff checks |
| `just gate-webidl` | generated WebIDL constructor/prototype coverage plus headless/CDP runtime-host integration |
| `just gate-phase0` | workspace/API DTO and trait-shape foundation |
| `just gate-phase1` | network/store tests, audit, and security fuzz targets |
| `just gate-phase2` | `deno_core` runtime and headless eval seam |
| `just gate-phase3` | HTML/selector/cascade behavior and CSS fixture profile |
| `just gate-phase4` | Vixen layout-tree/line/fragment behavior and layout fixtures |
| `just gate-phase5` | display-list/WebRender screenshot and visual fixture path |
| `just gate-phase6` | engine host-family tests, WebIDL, headless runtime, and CDP runtime integration |
| `just gate-alpha6-cdp` | external Playwright/CDP smoke plus dispatcher/socket tests over BrowserCore targets, including ordered lifecycle, one-pump same-connection cancellation for page/history/runtime navigation, non-blocking target creation, committed author-exception reporting, DOM/input, network, permissions, tracing, and stable errors |
| `npm test` | bounded-process, timeout, percentile, `/proc` parser, hash, and recursive-size unit tests used by the baseline tools |
| `cargo test -p vixen-headless --test incremental` | one-context headless load, before-frame capture, live BrowserCore evaluation/mutation, after-frame capture, deterministic names, and distinct valid PNG evidence |
| `just wpt-profile <profile> <root>` | optional external profile execution after fail-closed validation of the canonical repository, full pinned commit, clean checkout root, and sparse-path coverage |
| `just test-browser-core` | ADR-017 production owner/thread/typed-generation proof with two independent contexts, shared profile localStorage/cookies, isolated runtime/sessionStorage/history, asynchronous source loading, bounded cooperative HTML parsing and per-item script/lifecycle work, deadline-bounded V8/promise execution with reusable-isolate, failed-evaluation mutation discard, and author-timeout continuation proof, generation-cancellable external classic-script I/O with pre-hop CSP/mixed-content policy, status/nosniff checks, delta-safe profile cookie persistence, and stale cookie/document/runtime rejection, ordered phases, one generation-checked terminalization boundary, live redirect delivery before a gated final response, latest-request stop and stale-progress rejection, source/parser/script/lifecycle stale-work rejection, author-exception separation, cancelable wheel/navigation-key plus bounded live-script root scrolling, native/contenteditable IME-state commits, bounded event lag, and headless adapter coverage |
| `just compat-report` | current BrowserCore-backed committed fixture/profile counts and per-source/category output |
| `just fuzz-security` | URL, CSP, cookie, and HTML parser fuzz targets at the configured run count |
| `just audit` | `cargo audit` plus `cargo deny check` |
| `just linux-release-smoke` | pinned x86_64 Flutter 3.46.0-0.3.pre beta release/AOT plus Rust bridge/Yaru window-plugin build; stripped runner/plugin ELFs, deterministic archive creation, clean extraction, and Impeller-aware Cage/headless-Wayland launch smoke |
| `just linux-at-spi-smoke` | real release/AOT Flutter bundle in Cage's headless Wayland compositor with a fresh BrowserCore profile and local fixture; bounded process-filtered AT-SPI traversal must observe the BrowserCore-derived `DOM Basic` heading; Linux native AT evidence, not a screen-reader matrix |
| `just size-headless` | structured logical/allocated size, file count, and SHA-256 for the headless release binary |
| `just size-flutter-linux` | controlled release/AOT build and component-attributed raw-bundle comparison against the checked-in hello-Flutter peer; measurement-only and not FlatPark package evidence |
| `just baseline-headless` / `just baseline-headless-json` | per-scenario latency and Linux process-memory measurements for committed startup, navigation/runtime, layout, paint, and screenshot controls |
| `just baseline-profile-growth` | opaque temporary profile growth at init/repeated/unique/storage checkpoints with localStorage reopen proof |
| `just baseline-beta` | hermetic local headless scenarios, profile growth, and headless artifact size; measurement-only and outside `gate-push` |

## Evidence rules

- Run the cheapest focused crate test while editing, then the relevant gate above.
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
  priority. Flutter is the only rendered GUI and parity concern.
- `just gate-native-abi` proves the handwritten C ABI/header/wire/frame ownership
  milestone over the same safe controller. `just gate-flutter-shell` adds Dart,
  widget, worker-isolate, texture-presenter, and live native smoke evidence. It
  proves physical viewport, pointer/wheel/keyboard routing, monotonic host
  focus/visibility/lifecycle state, and the bounded
  BrowserCore-to-Flutter Semantics hierarchy with bounded descriptions, three
  non-tree relationships, native/authored range actions, live regions, and
  event-driven full projection refresh. `linux-at-spi-smoke` adds first native
  Linux AT evidence, but not real native IME interaction, complete screen-reader coverage, packages,
  release behavior, or non-Linux GUI support; use
  `FLUTTER_SHELL.md` for remaining gates.
- Size/performance thresholds become gates only after a representative baseline,
  environment, and comparison method are committed.
- Hosted `ci.yml` runs architecture/native-ABI checks, Node baseline tests, the
  workspace gates, and the external Playwright/CDP smoke with Mesa software GL;
  its separate security job runs `cargo audit` and `cargo deny check`.
  `fuzz.yml` runs all four existing fuzz targets on a bounded weekly/manual CI
  budget and retains crashes. The one-million-iteration local/release command
  remains `just fuzz-security`.

## Current measured anchors

- Compatibility baseline: **269 fixtures / 2,015 checks / 100% passing** as of
  2026-07-10. `COMPAT.md` is authoritative.
- Historical pre-Yaru Linux x86_64 Flutter raw-bundle reference:
  **22,778,750-byte hello / 85,509,520-byte Vixen / 62,730,770-byte delta**,
  measurement-only and not a current dependency-graph or FlatPark package
  claim; see `BASELINES.md`.
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
