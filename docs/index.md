# Vixen

[![CI](https://github.com/adonm/vixen/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/adonm/vixen/actions/workflows/ci.yml)
[![Pages](https://github.com/adonm/vixen/actions/workflows/pages.yml/badge.svg?branch=main)](https://github.com/adonm/vixen/actions/workflows/pages.yml)
[![Docs](https://img.shields.io/badge/docs-vixen.adonm.dev-blue)](https://vixen.adonm.dev/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/adonm/vixen/blob/main/LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](https://github.com/adonm/vixen/blob/main/Cargo.toml)
[![Flatpak](https://img.shields.io/badge/flatpak-GNOME%2050-4a86cf.svg)](guidance/gnome-sdk-flatpak-builder.md)

Vixen is a modern-Linux Firefox replacement built in Rust: a minimal desktop
browser, first-class headless/CDP automation, and the most web capability per
byte.

The hard, spec-heavy subsystems are delegated where that keeps Vixen smaller
and more correct: **Stylo/selectors** for CSS matching and cascade,
**deno_core/V8** for JS execution and host packaging, **WebRender** for paint,
and **html5ever** for HTML. Vixen owns the product glue, modern-Linux
Relm4/libadwaita shell, networking/security layer, persistence, headless
tooling, WPT reporting, and the Rust layout engine.

## Start here

- [Project Direction](PROJECT_DIRECTION.md) — current focus and constraints.
- [Architecture](ARCHITECTURE.md) — crate layout and dependency direction.
- [Plan](PLAN.md) and [Milestones](MILESTONES.md) — implementation roadmap.
- [Development](DEVELOPMENT.md) — local workflow and contribution mechanics.
- [GNOME SDK Flatpak builder](guidance/gnome-sdk-flatpak-builder.md) — the
  Flatpak build/release path used by CI.

## Repository

- Source: <https://github.com/adonm/vixen>
- Releases: <https://github.com/adonm/vixen/releases>
- Docs: <https://vixen.adonm.dev/>
