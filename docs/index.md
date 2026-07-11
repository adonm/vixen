# Vixen

[![CI](https://github.com/adonm/vixen/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/adonm/vixen/actions/workflows/ci.yml)
[![Pages](https://github.com/adonm/vixen/actions/workflows/pages.yml/badge.svg?branch=main)](https://github.com/adonm/vixen/actions/workflows/pages.yml)
[![Docs](https://img.shields.io/badge/docs-vixen.adonm.dev-blue)](https://vixen.adonm.dev/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/adonm/vixen/blob/main/LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](https://github.com/adonm/vixen/blob/main/Cargo.toml)
[![GUI target](https://img.shields.io/badge/GUI-Flutter%203.44-02569B.svg)](FLUTTER_SHELL.md)

Vixen is a focused cross-platform Firefox replacement: a Flutter GUI targeting
Linux, macOS, Windows, Android, and the Apple Silicon iOS Simulator, first-class headless/CDP automation, and
the most web capability per byte.

The hard, spec-heavy subsystems are delegated where that keeps Vixen smaller
and more correct: **Stylo/selectors** for CSS matching and cascade,
**deno_core/V8** for JS execution and host packaging, **WebRender** for paint,
and **html5ever** for HTML. BrowserCore owns browser truth and Flutter/Dart owns
only chrome, presentation, and host-service UI. Flutter is not installed here and
no Flutter build exists; the GTK/Relm4 shell is the temporary Linux compatibility
baseline.

## Start here

- [Project Direction](PROJECT_DIRECTION.md) — current focus and constraints.
- [Architecture](ARCHITECTURE.md) — crate layout and dependency direction.
- [Flutter Shell](FLUTTER_SHELL.md) — five-platform migration and gates.
- [Roadmap](ROADMAP.md) and [Milestones](MILESTONES.md) — current delivery and evidence.
- [Historical Plan](PLAN.md) — original Linux/Relm4 phase record.
- [Development](DEVELOPMENT.md) — local workflow and contribution mechanics.
- [GNOME SDK Flatpak builder](guidance/gnome-sdk-flatpak-builder.md) — the
  current compatibility-shell Flatpak path used by CI.

## Repository

- Source: <https://github.com/adonm/vixen>
- Releases: <https://github.com/adonm/vixen/releases>
- Docs: <https://vixen.adonm.dev/>
