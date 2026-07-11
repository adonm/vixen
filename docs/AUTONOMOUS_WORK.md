# Autonomous work contract

This document exists so agents and maintainers can make progress without
re-asking project-direction questions.

## Decision policy

Until alpha, continue without asking unless a change would alter architecture.
Architecture changes include:

- a new JS runtime target or abstraction,
- a GUI path other than the ADR-018 Flutter shell, or retention of GTK/Relm4
  beyond its Linux compatibility-baseline parity gate,
- a second render/paint path,
- a Flutter bridge that moves browser ownership, web rendering, or accessibility
  source data out of BrowserCore,
- a new layout architecture,
- a core dependency that changes binary-size or subsystem ownership materially,
- a security-policy change that makes behavior less fail-closed.

For ordinary implementation details, choose the safest path aligned with
`PROJECT_DIRECTION.md`, document assumptions briefly, and keep moving.

## Commit and push policy

- Automatic commits are allowed when the batch is coherent and gates pass.
- Automatic pushes are allowed when hk pre-push gates pass.
- Prefer milestone commits over tiny churn commits.
- Do not bypass hk. If hk fails, fix the issue or report the blocker.

## Gate policy

- **Inner loop:** focused `cargo check`/`cargo test`/`just gate-phaseN` as needed.
- **Before commit:** hk pre-commit hook; it should stay quick and fix formatting.
- **Before push:** hk pre-push hook; long gates run here because iteration speed
  matters.
- **Release:** `ACCEPTANCE.md` gates plus measured size/compatibility reports.

Flutter is not installed and no Flutter gate exists in this workspace. Until
real recipes land, never report Rust/GTK checks as Flutter proof. Platform work
follows `FLUTTER_SHELL.md`: Linux fake/real bridge, bounded RGBA and input/
viewport, offline Flatpak/size evidence, desktop expansion, Android, then the iOS
Simulator track, with V8 WebAssembly, accessibility, and host services kept
consistent across targets.

The project owns hook definitions in `hk.pkl`. `just` owns command recipes; hk
owns when those recipes run in the git lifecycle.

## Reporting format

Final handoff should be terse and evidence-first:

- objective completed,
- changed files,
- checks run and pass/fail status,
- commit hash and push status when applicable,
- remaining known gaps or next slice.

For large compatibility work, update `COMPAT.md` from actual fixture/WPT output
rather than prose guesses.

## Documentation rule

Prefer ADR-style docs that explain **why** and point to code for **how**. Avoid
parallel prose that must be maintained beside source unless it records product
direction, architecture constraints, compatibility results, or gate policy.
