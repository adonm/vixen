# Vixen development mode

This document defines **dev** for this repo: how to move quickly during alpha
without creating long-term maintenance debt.

Project focus is defined in [`PROJECT_DIRECTION.md`](PROJECT_DIRECTION.md).
Autonomous commit/push policy is defined in
[`AUTONOMOUS_WORK.md`](AUTONOMOUS_WORK.md). Git lifecycle gates are enforced by
[`hk`](https://hk.jdx.dev/) via the checked-in [`../hk.pkl`](../hk.pkl).

## Definitions

- **Dev / alpha** means partial browser capability is allowed when it is
  executable, tested, fail-closed, and honestly documented. Alpha work may be
  incomplete; it must not be vague, hidden, or unbounded.
- **A slice** is the smallest reviewable unit that makes one browser-visible
  seam better: usually one `Page`/headless/CDP/WPT fixture path plus the pure
  engine code it consumes.
- **A tock** is a cleanup-only follow-up after capability work: delete dead
  shims, split modules nearing 1 kLOC, move duplicated parsing to one helper,
  tighten docs, and retire stale fixtures.
- **Release mode** is stricter than dev mode and is governed by
  [`ACCEPTANCE.md`](ACCEPTANCE.md). Do not use this document to lower release
  gates.

## Alpha development contract

Every alpha slice should satisfy these rules:

1. **Visible seam first.** Prefer code that reaches the engine-owned browser/
   context/document path, `vixen-headless`, CDP, or a committed WPT/fixture
   check. A `Page` slice must preserve BrowserCore ownership and name the live
   document seam it advances. Pure prep is fine only when the next visible seam
   is named.
2. **One trust boundary at a time.** For security-sensitive paths, name the
   boundary, validate near it, fail closed, and surface stable error codes.
3. **Reuse pure modules without duplicating ownership.** JS host objects, Page
   projections, CLI, and CDP should call the same Rust implementation, but only
   the browser core decides lifecycle, commit, cancellation, and persistence.
4. **Partial APIs must be explicit.** A subset may ship in alpha if unsupported
   inputs fail closed and the supported behavior is documented in `COMPAT.md`.
   Interface shape without a backing subsystem must be labeled shape-only.
5. **No silent architecture drift.** New dependencies, crate edges, rendering
   paths, process boundaries, or storage/network policy changes must be backed by
   an ADR/update in `DECISIONS.md` or an explicit plan note.
6. **Tests travel with behavior.** Unit tests prove pure logic; one integration
   check proves the user-visible seam. If a fixture manifest assertion is the
   seam, keep it committed.
7. **Flutter stays an adapter.** Dart owns chrome and host-service presentation;
   BrowserCore owns browser state, WebRender, and accessibility source data.
   Bridge buffers, queues, frames, semantics updates, and native handles are
   bounded with explicit lifetime and generation tests.

## Gate tiers

Use the cheapest gate that matches the risk, then escalate before review or
push.

| Tier | Use when | Command shape |
|------|----------|---------------|
| Inner loop | Editing one crate/module | `cargo check -p <crate>` plus focused `cargo test ... <name>` |
| Pre-commit | A commit is being made | hk pre-commit: `cargo fmt`, merge-conflict/private-key scan, staged diff whitespace check |
| Alpha slice | A coherent partial capability is ready | focused tests + relevant `just gate-phaseN` |
| Pre-push | Work is ready to leave the machine | hk pre-push: `just gate-push` |
| Release | Versioned release readiness | every gate in `ACCEPTANCE.md` |

`just gate-push` is the long integration gate. Keep long gates out of the inner
loop and pre-commit path so iteration stays fast.

Current pre-push composition:

```sh
just gate-alpha
just gate-phase6
just gate-smoke
git diff --check
git diff --cached --check
```

Adjust `just gate-push` as the alpha architecture changes; hk should keep
calling that single recipe.

### GUI shell environment blockers

The Linux Flutter project and focused gate are checked in. Install the exact
Flutter 3.46.0-0.3.pre beta archive declared in `.mise.toml`, then run its gate:

```sh
just setup-flutter
just gate-flutter-shell
```

`just build-flutter-linux` and `just run-flutter` additionally need CMake, Ninja,
pkg-config, and GTK 3 development headers. Missing host packages are an
environment limitation; they do not turn Rust or Dart-only checks into Linux
bundle proof. The debug bundle has been reproduced in a Fedora 43 container.

The released Linux shell is Flutter. Local release builds use **Podman + the
pinned GNOME builder image**; CI builds the same release shape on Ubuntu 24.04.
The transitional compatibility shell still uses
GTK/libadwaita in-tree; if an ad-hoc native `cargo check --features
vixen-shell/gtk-shell` fails with missing native packages, treat that as a
host-environment limitation. Verify release-shell changes with:

```sh
just flutter-builder-update
just linux-release-prefetch
just linux-release-smoke
```

Use native GTK development packages only for ad-hoc local work. Keep blocker
notes explicit about this split so follow-up work points at the containerized
release path before asking for host package installs.

GitHub Releases publish the deterministic x86_64 archive built with the
SHA-256-pinned official Flutter 3.46.0-0.3.pre beta, locked application/Cargo
dependencies, and pinned rusty_v8 input. FlatPark repackages those bytes as a
signed convenience Flatpak. Flutter's Linux embedder uses GTK, so migration
removes Relm4/libadwaita/custom GLArea ownership without promising a GTK-free
runtime.

The safe Rust controller and handwritten C ABI can be developed without Flutter
installed:

```sh
just test-flutter-controller
just gate-native-abi
just gate-architecture
```

`just gate-native-abi` proves C ABI/header/layout, bounded JSON/frame wire
behavior, opaque registry ownership, stable errors/events, and release over the
one-owner controller. `just gate-flutter-shell` adds the Dart binding, injected
fake tests, production worker, Linux texture/input presenter, and live native
smoke. It proves physical viewport and pointer/wheel/key routing, but not
complete semantics/native AT, IME/gesture/lifecycle, a platform package, or a
release build. The bounded BrowserCore-to-Flutter Semantics hierarchy is covered.

## Larger alpha batches

Larger batches are encouraged when they reduce handoff overhead **and** stay
coherent. A batch is coherent if it has:

- one feature family or one host-object family,
- one primary visible seam,
- one docs/compat story,
- one verification story.

Stop and split when the next addition would introduce a second trust boundary, a
second unrelated feature family, or a second independent rollback concern.

## Maintainability budget

Alpha speed is acceptable only while these budgets stay visible:

- Non-test modules should stay below 1,000 lines. If a module crosses that while
  moving fast, create the split in the next tock before widening the feature.
- Prefer boring data flow over framework gravity: DTOs in `vixen-api`, lifecycle
  and pipeline state in the engine-owned browser/context/document graph, and
  browser-facing adapters in headless/CDP/shell.
- Keep Dart DTOs and native bridge code mechanical. Do not mirror profile,
  navigation, DOM, layout, permission, or accessibility truth in Flutter state.
- Avoid duplicate parsers/matchers. Runtime host objects and BrowserCore/Page
  operations must extract or call the same Rust implementation.
- Do not reintroduce string-expression shims. Retire transitional runtime/
  document snapshots as live resources replace them.
- Keep `COMPAT.md` honest: partial support is fine, overclaiming is not.

## Alpha definition of done

A dev/alpha slice is done when:

- the supported subset is named,
- unsupported inputs fail closed,
- docs mention the current state and next widening step,
- focused tests and the relevant gate pass,
- hk pre-commit/pre-push gates are clean before commit/push,
- any known debt is either removed immediately or named as the next tock.
