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
7. **Flutter is the renderer, not the browser owner.** Dart owns bounded
   formatting/Paragraph/Canvas state, atomic renderer commits, chrome, Semantics
   presentation, and host-service UI. BrowserCore owns DOM/runtime/navigation,
   computed styles, policy/persistence, web-event semantics, accepted resources,
   and accessibility meaning. Bridge payloads, queues, commits, queries, and
   handles are bounded with explicit revision/lifetime tests.

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
Flutter 3.47.0-0.1.pre beta archive declared in `.mise.toml`, then run its gate.
The beta pin is deliberate for required Linux Impeller support; do not replace it
with a stable SDK or accept a Skia-backed smoke without updating the renderer
decision and evidence:

```sh
just setup-flutter
just gate-flutter-shell
```

`just build-flutter-linux` and `just run-flutter` additionally need CMake, Ninja,
pkg-config, and GTK 3 development headers. Missing host packages are an
environment limitation; they do not turn Rust or Dart-only checks into Linux
bundle proof. The debug bundle has been reproduced in a Fedora 43 container.
The Linux runner requires native Wayland. `just run-flutter-cage` additionally
uses Cage with wlroots' headless backend for isolated local Wayland testing;
X11 and XWayland are intentionally unsupported.

The released Linux shell is Flutter. Local release builds use **Podman + the
pinned GNOME builder image**; CI builds the same release shape on Ubuntu 24.04.
Flutter is the sole rendered frontend target; the Rust workspace has no GTK4/
libadwaita/Relm4 feature or fallback GUI. `just check` and `just clippy` cover
every Rust target and feature without GNOME development packages. Verify Linux
Flutter release changes with:

```sh
just flutter-builder-update
just linux-release-prefetch
just linux-release-smoke
just linux-at-spi-smoke
just linux-interaction-smoke
```

Native GTK3 development packages are needed only for direct host Flutter Linux
builds; the pinned release container supplies them for official archive work.

GitHub Releases publish the deterministic x86_64 archive built with the
SHA-256-pinned official Flutter 3.47.0-0.1.pre beta, locked application/Cargo
dependencies, and pinned rusty_v8 input. FlatPark repackages those bytes as a
signed convenience Flatpak. Flutter's Linux embedder uses GTK, so migration
does not imply a GTK-free runtime; direct GTK code remains limited to Flutter's
native runner boundary.

The safe Rust controller and handwritten C ABI can be developed without Flutter
installed:

```sh
just test-flutter-controller
just gate-native-abi
just gate-architecture
```

These gates currently prove the transitional JSON/frame wire, registry, worker,
texture/input presenter, and native smoke. ADR-022 adds revision/mutation/commit/
query ABI and Canvas/Paragraph evidence, then deletes frame/texture-specific
proof at cutover. Existing checks remain comparison coverage, not target APIs.

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
- Keep Dart DTOs and native bridge code mechanical. Renderer box/fragment/scene
  state is ephemeral and commit-bound; do not mirror profile, navigation, DOM,
  permissions, policy, script state, or accessibility meaning in Flutter.
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
