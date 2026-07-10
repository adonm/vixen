# Executable gates and evidence

This file is intentionally not a second roadmap. Product order and future
milestones live in [`ROADMAP.md`](ROADMAP.md); historical phase instructions live
in [`PLAN.md`](PLAN.md); measured compatibility lives in
[`COMPAT.md`](COMPAT.md). This file answers only: “which checked-in command proves
which layer today?”

## Gate index

| Command | Current evidence |
|---------|------------------|
| `just gate-alpha` | formatting, clippy, host workspace checks, generated WebIDL/runtime seams, BrowserCore ownership tests, BrowserCore-backed committed fixture runner, and stable crate-boundary allowlist |
| `just gate-architecture` | leaf-crate dependency rules for `vixen-api`, `vixen-net`, `vixen-store`, and `vixen-wpt`; frontend direct-composition debt remains documented until ADR-017 migration |
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
| `just gate-alpha6-cdp` | external Playwright/CDP smoke over BrowserCore targets, including ordered lifecycle, DOM/input, network, permissions, tracing, and stable errors |
| `just test-browser-core` | ADR-017 production owner/thread/typed-generation proof with two independent contexts, shared profile localStorage, isolated runtime/sessionStorage/history, asynchronous source loading, ordered phases, redirect event ordering, stop/supersede/reload/history race late-completion rejection, bounded event lag, headless adapter coverage, and GTK-free multi-context shell routing |
| `just compat-report` | current BrowserCore-backed committed fixture/profile counts and per-source/category output |
| `just fuzz-security` | URL, CSP, cookie, and HTML parser fuzz targets at the configured run count |
| `just audit` | `cargo audit` plus `cargo deny check` |
| `just flatpak-build` | supported GNOME SDK/Flatpak GUI build path |
| `just size-fp` | measured Flatpak GUI and release headless artifact sizes; measurement only until baselines become accepted budgets |
| `just baseline-headless` | measured release headless startup + first-navigation + eval latency on a committed DOM fixture; measurement only until baselines become accepted budgets |

## Evidence rules

- Run the cheapest focused crate test while editing, then the relevant gate above.
- A pure unit test proves an algorithm. A browser claim also needs a shared-core
  integration path, fixture/profile, external automation smoke, or GUI smoke.
- Fixture behavior changes update `COMPAT.md` from `just compat-report`; do not
  hand-invent counts.
- ADR-017 frontend ownership migration is enforced by `gate-architecture`;
  subsequent lifecycle work adds cancellation/partition/live-document evidence
  without restoring direct frontend composition.
- GTK changes use `just flatpak-build` when host development packages are absent.
- Size/performance thresholds become gates only after a representative baseline,
  environment, and comparison method are committed.

## Current measured anchors

- Compatibility baseline: **269 fixtures / 2,015 checks / 100% passing** as of
  2026-07-10. `COMPAT.md` is authoritative.
- External automation contract: [`CDP_PLAYWRIGHT_SMOKE.md`](CDP_PLAYWRIGHT_SMOKE.md).
- Browser ownership/cancellation vertical: `just test-browser-core` (engine,
  headless, and GTK-free shell adapters through the production command/event
  handle).
- Release requirements: [`ACCEPTANCE.md`](ACCEPTANCE.md).

When a gate and its description diverge, fix this table in the same change as the
recipe. Do not copy already-landed feature inventories back into the roadmap.
