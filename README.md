# Vixen

[![CI](https://github.com/adonm/vixen/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/adonm/vixen/actions/workflows/ci.yml)
[![Pages](https://github.com/adonm/vixen/actions/workflows/pages.yml/badge.svg?branch=main)](https://github.com/adonm/vixen/actions/workflows/pages.yml)
[![Docs](https://img.shields.io/badge/docs-vixen.adonm.dev-blue)](https://vixen.adonm.dev/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](Cargo.toml)
[![GUI target](https://img.shields.io/badge/GUI-Flutter%203.47%20beta-02569B.svg)](docs/FLUTTER_SHELL.md)

A focused cross-platform Firefox replacement: one Flutter web renderer and GUI
targeting Linux, macOS, Windows, Android, and the Apple Silicon iOS Simulator,
plus first-class chrome-less Flutter/CDP automation and the most web capability
per byte.

**Linux is Vixen's highest-priority GUI and release target.** Browser usability,
host integration, packaging, accessibility evidence, and performance gates land
on Linux first. macOS, Windows, Android, and the iOS Simulator remain committed
targets and reuse the proven BrowserCore/Flutter contract after the Linux path.
FlatPark publishing is deliberately deferred until the Linux Flutter shell is a
basic usable browser: visible navigation, scrolling, text input/IME,
back/forward/reload/stop, find/zoom, and bounded failure recovery take priority
over package-registry work.

The hard, spec-heavy primitives are delegated where that keeps Vixen smaller and
more correct: **Stylo/selectors** for CSS matching and cascade,
**deno_core/V8** for JavaScript, **html5ever** for HTML, and Flutter's
**Paragraph/Canvas/scene/Semantics** substrate for cross-platform rendering.
BrowserCore owns DOM/runtime/navigation/network/security/persistence, computed
styles, accepted resources, web events, and accessibility meaning. The Vixen
renderer hosted in Flutter owns CSS formatting, text/image measurement, paint,
hit testing, scroll geometry, semantic bounds, and scene capture through bounded
revision/mutation/commit/query protocols. Flutter's public scene APIs run over
explicitly enabled Impeller; a Skia-backed launch is not accepted renderer or
release evidence. See
[`docs/PROJECT_DIRECTION.md`](docs/PROJECT_DIRECTION.md) for the current focus.

**The renderer migration is complete through R7.** The production GUI,
chrome-less automation host, rendered CDP, fixture manifest, synchronous CSSOM
geometry, hit testing, semantic bounds, scroll geometry, and scene capture all
use Flutter formatter commits. BrowserCore publishes exact styled source and
accepts only revision-matched, bounded commits and queries.

The deleted path includes WebRender/gleam, `GlContext`, both EGL owners, native
visual headless, Rust paint/display-list/layout owners and paint primitives, the
RGBA C/Dart frame transport, Linux pixel-buffer texture plugin/presenter, raw
coordinate-input ABI, and their obsolete gates/tests. Native `vixen-headless`
remains a text/runtime/profile utility; rendered operations fail closed there.
Manifest checks that require geometry or pixels are explicitly
`flutter-js-eval`, `layout-box`, `visual-hash`, or `ref-equivalent` and run only
through the Flutter fixture host.

R1–R6 protocol, formatter, presentation, interaction, rendered-product, and
synchronous-layout evidence remains composed by `just gate-r6`. `just test-r7`
adds deletion scans plus native/Flutter tests and lint; `just gate-r7` composes
all prior rendered evidence with that final cutover proof. The Linux shell is
native-Wayland-only, uses Yaru/Adwaita-blue chrome, owns one BrowserCore, and
shows an explicit commit-unavailable surface rather than falling back to native
renderer pixels.

## Status

Pre-v1.0. The current integrated vertical includes:

- BrowserCore-owned browsing contexts, navigation/history, profile/session state,
  network/security policy, html5ever DOM, Stylo-compatible selector/cascade
  projection, V8 page realms, web events, forms, text input, accessibility
  meaning, and accepted image resources.
- One Flutter-owned renderer with bounded full snapshots/incremental mutations,
  atomic commits/presentation, Paragraph text queries, hit-test queries,
  mechanical scroll state, semantic bounds, scene PNG capture, reset/resync, and
  cancellation-safe synchronous CSSOM geometry.
- One native-Wayland Flutter shell and one chrome-less automation mode sharing
  the same formatter and painter. Missing/stale commits fail closed; there is no
  native pixel fallback.
- A text/runtime-only `vixen-headless`, CDP core, C ABI/Dart worker bridge, Rust
  source-inspection WPT runner, and Flutter-rendered fixture/CDP/Playwright
  runners. Render-dependent manifest checks are explicitly routed to Flutter.
- R1–R8 evidence. `just test-r7` is the focused cutover/deletion gate;
  `just gate-r7` composes all prior rendered product evidence, while the R8
  checkpoints add compatibility/release/frame/GPU rebaselines and a passing
  real Mozc plus native AT-SPI interaction corridor.
- The x86_64 Linux release now uses the checksum-pinned flutter-dev GTK4 SDK,
  Dart 3.14, and `libflutter_linux_gtk4.so`. GTK3-only Yaru window plugins are
  excluded while pure-Dart Yaru styling remains. The GTK4 AT-SPI corridor
  observes BrowserCore names, roles, states, and positive local bounds and
  verifies from `/proc` that the process loads GTK4 but not GTK3. The current
  custom engine does not yet expose its semantic nodes through AT-SPI Action or
  transformed screen-coordinate bounds; native Wayland input remains the
  interaction path until that upstream surface advances.

The project is not yet a daily-driver browser. Remaining work is product breadth:
standards compatibility, accessibility/IME/device matrices, performance and size,
process hardening, packaging/update/distribution, and sustained release evidence.

## Setup

Workspace setup is split deliberately:

- [mise](https://mise.jdx.dev) pins tool versions and exports the workspace
  environment (`CARGO_HOME`, `PATH`, Rust toolchain selection, `hk`).
- [`just`](justfile) owns project actions. Prefer a recipe over spelling out raw
  `cargo ...` commands in docs, CI, or local scripts.
- [hk](https://hk.jdx.dev/) owns git lifecycle enforcement: quick pre-commit,
  long pre-push.

```sh
mise trust
mise bootstrap --yes     # pinned tools + optional Cargo tools + `just setup`
eval "$(mise activate bash)"
just hooks-install       # installs hk hooks through mise
just check               # alias: check-all-host
just test                # alias: test-host
just smoke               # fmt-check + clippy + check + tests
```

Headless runs use isolated temporary profiles by default. Pass
`--profile-dir <DIR>` to persist BrowserCore state in `<DIR>/profile.redb`,
including for `--cdp`:

```sh
cargo run -p vixen-headless -- --url https://example.com --profile-dir .tmp/vixen-profile --eval 'document.title'
```

Common recipes:

| Recipe | Use |
|--------|-----|
| `just setup` | Nightly for fuzzing, optional Cargo tools, then `check-all-host` |
| `just hooks-install` | Install/update hk git hooks via `hk install --mise` |
| `just check` / `just check-all-host` | Type-check the host-runnable workspace |
| `just test` / `just test-host` | Run host-runnable tests |
| `just smoke` / `just gate-smoke` | Reviewer baseline used by pre-push |
| `just gate-push` | Long pre-push gate invoked by hk |
| `just webidl` / `just gate-webidl` | Generated WebIDL/runtime host seam coverage |
| `just audit` | `cargo audit` + `cargo deny check` |
| `just baseline-headless` / `just baseline-headless-json` | Measure the hermetic local headless scenario suite |
| `just baseline-flutter-linux` / `just baseline-flutter-linux-json` | Measure software-rendered release/AOT Flutter startup, capture, memory, exact commit frames, and synthetic mutation/input endpoints under Cage |
| `just baseline-flutter-linux-hardware` / `just baseline-flutter-linux-hardware-json` | Run the same measurement only after a non-software Wayland EGL renderer is fingerprinted |
| `just baseline-profile-growth` | Measure temporary profile growth and storage persistence across reopen |
| `just size-headless` | Report structured headless artifact size |
| `just flutter-size-prefetch` | Network-capable staging for pinned Linux Flutter size inputs; not evidence |
| `just size-flutter-linux` / `just size-flutter-linux-json` | Release/AOT hello-Flutter versus Flutter+Vixen raw-bundle comparison |
| `just baseline-beta` | Run the local headless, profile-growth, and headless-size measurement batch |
| `just docker-builder-pull` | Pull the digest-pinned GNOME 50 release-builder image |
| `just linux-release-prefetch` | Stage locked release inputs and the pinned rusty_v8 archive |
| `just linux-release-smoke` | Build, archive, extract, and Impeller-smoke the official Linux release |

These commands complete the local latency, Linux process-memory, profile-growth,
headless-path, and artifact-size measurement foundation. They are measurement-
only: real external-site coverage, the GUI/FlatPark host matrix, animation/
scanout/physical-input timing, JS heap, and transfer throughput remain future
baselines. See
[`docs/BASELINES.md`](docs/BASELINES.md).

The Flutter release and size recipes use a controlled checked-in hello
application, the digest-pinned GNOME 50 Docker builder, checksum-pinned
Rust/Flutter toolchains, locked dependencies, and a separately staged pinned
rusty_v8 archive. The first clean measurement-only x86_64 reference is recorded in
[`docs/BASELINES.md`](docs/BASELINES.md).

`mise bootstrap` and recipes run from a mise-active shell use
`CARGO_HOME=<workspace>/.cargo`, so the Cargo registry cache and installed dev
tooling stay inside the workspace (see
[`docs/guidance/cargo-home.md`](docs/guidance/cargo-home.md)).

**The GNOME 50 SDK is not installed on the host.** Local Linux release builds
run with plain `docker pull`, `docker image inspect`, and `docker run` against
the pinned builder image. The network-capable prefetch fills workspace-local
toolchain/package caches; release compilation runs with `--network=none` and
does not mount a host Flutter or Rust installation:

```sh
just docker-builder-pull
just linux-release-prefetch
just linux-release-smoke
```

The result is a deterministic `vixen-linux-x86_64.tar.gz` GitHub Release asset.
FlatPark pins and repackages that unchanged upstream archive, signs the Flatpak,
and hosts the update repository. Vixen does not maintain a parallel OSTree
repository. Flutter's Linux embedder uses GTK, so this removes packaged
application-owned GTK widgets without promising a GTK-free runtime. The archive
remains reproducible engineering evidence; FlatPark
submission and publishing are not current priorities and resume only after the
basic-browser gate in `docs/ROADMAP.md` passes.

See [`docs/guidance/flatpark-release.md`](docs/guidance/flatpark-release.md)
for the full workflow. Headless/CI hosts that only build `vixen-api` /
`vixen-net` / `vixen-store` need neither the GNOME SDK nor the container.
`mise install` now provisions the pinned flutter-dev SDK as a project dependency,
but `just check` does not execute it.

See [`.mise.toml`](.mise.toml) and the
[mise bootstrap guide](https://mise.jdx.dev/bootstrap.html). The library
MSRV is 1.88 (let-chains); the developer toolchain is pinned in
[`.mise.toml`](.mise.toml).

---

## Repository map

| Path                                        | Purpose                                                       |
|---------------------------------------------|---------------------------------------------------------------|
| [`docs/SPEC.md`](docs/SPEC.md)              | **What Vixen must do.** Capabilities, CLI, behaviour contracts. |
| [`docs/PROJECT_DIRECTION.md`](docs/PROJECT_DIRECTION.md) | **What Vixen is optimizing for.** North star, users, priorities, alpha definition. |
| [`docs/ROADMAP.md`](docs/ROADMAP.md)        | **What comes next.** Alpha convergence through the full replacement horizon. |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | **How Vixen is structured.** Crates, data flow, trust boundaries, trait APIs. |
| [`docs/FLUTTER_SHELL.md`](docs/FLUTTER_SHELL.md) | **How the GUI migrates.** Five-platform bridge, rendering, accessibility, packaging, size, and gates. |
| [`docs/DECISIONS.md`](docs/DECISIONS.md)    | **Why these choices.** ADR-style records for the major decisions. |
| [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md) | **How to move fast safely.** Alpha/dev workflow, gate tiers, maintainability budget. |
| [`docs/RUNTIME_WEB_PLATFORM.md`](docs/RUNTIME_WEB_PLATFORM.md) | **How WebIDL/DOM/Web APIs are exposed.** JS bootstrap vs Rust op/resource strategy. |
| [`docs/AUTONOMOUS_WORK.md`](docs/AUTONOMOUS_WORK.md) | **How agents/maintainers can proceed.** Commit/push policy, hk gates, report format. |
| [`docs/PLAN.md`](docs/PLAN.md)              | **Historical record.** Original Linux/Relm4 phased runbook. |
| [`docs/REFERENCES.md`](docs/REFERENCES.md)  | **Where to look for truth.** Pinned reference trees + how to consult each. |
| [`docs/ACCEPTANCE.md`](docs/ACCEPTANCE.md)  | **When it's done.** Release gates per capability. |
| [`docs/BASELINES.md`](docs/BASELINES.md)    | **How it is measured.** Local latency, memory, profile-growth, and artifact reports. |
| [`docs/guidance/`](docs/guidance)           | **How to do specific tasks.** Tooling, release archives, and FlatPark packaging. |
| `LICENSE`                                   | Apache 2.0 (lands at Phase 0). |

---

## Reading order

If executing the build:

1. `docs/PROJECT_DIRECTION.md` — the north star
2. `docs/ROADMAP.md` — the next delivery order
3. `docs/ARCHITECTURE.md` — the shape
4. `docs/FLUTTER_SHELL.md` — Flutter GUI contract and platform gates
5. `docs/RUNTIME_WEB_PLATFORM.md` — runtime host strategy
6. `docs/DEVELOPMENT.md` and `docs/AUTONOMOUS_WORK.md` — workflow and gates
7. `docs/DECISIONS.md` — confirm the choices
8. `docs/SPEC.md`, `docs/PLAN.md`, `docs/REFERENCES.md`, `docs/ACCEPTANCE.md`
   — contracts, historical runbook, references, release checks

If evaluating the project: read `docs/SPEC.md` and
`docs/DECISIONS.md`, then sample `docs/PLAN.md`.

When a doc and a decision record disagree, the **decision record wins**.
Update both when resolving.

---

## Working assumptions

- Primary GUI targets: **Linux, macOS, Windows, Android, and Apple Silicon iOS
  Simulator** through the pinned Flutter `3.47.0-1.0.pre-160` flutter-dev SDK
  (framework `328b829d35`, Dart `3.14.0-28.0.dev`). Each remains
  evidence-gated; the Linux Flutter renderer/shell and deterministic release
  path are implemented, while non-Linux runners remain open. Validation tracks
  each target's latest stable major OS release at
  the release cutoff; older releases are best-effort unless explicitly tested.
- Linux publishes an official x86_64 release archive that FlatPark repackages
  unchanged as a signed convenience Flatpak after the basic-browser gate.
  Registry publishing is deferred meanwhile. Flutter is the sole rendered
  frontend target; its Linux embedder may still depend on GTK at runtime.
- The current Rust release profile starts with `strip = true`, `lto = "thin"`,
  `codegen-units = 1`, and `panic = "abort"`; Flutter release/AOT and native
  packaging are measured per platform before any stronger optimization claim.
- App IDs: `dev.adonm.vixen` (production), `dev.adonm.vixen.Devel` (devel).

## License

Apache 2.0 — see [`LICENSE`](LICENSE).
