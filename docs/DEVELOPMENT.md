# Vixen development mode

This document defines **dev** for this repo: how to move quickly during alpha
without creating long-term maintenance debt.

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

1. **Visible seam first.** Prefer code that reaches `vixen_engine::page::Page`,
   `vixen-headless`, CDP, or a committed WPT/fixture check. Pure prep is fine
   only when the next visible seam is named in the same change or docs.
2. **One trust boundary at a time.** For security-sensitive paths, name the
   boundary, validate near it, fail closed, and surface stable error codes.
3. **Reuse pure modules.** JS host objects, Page projections, CLI, and
   CDP should call the same Rust implementation instead of growing parallel
   behavior.
4. **Partial APIs must be explicit.** A subset may ship in alpha if unsupported
   inputs fail closed and the supported shape is documented in `COMPAT.md`,
   `PLAN.md`, or `MILESTONES.md`.
5. **No silent architecture drift.** New dependencies, crate edges, rendering
   paths, process boundaries, or storage/network policy changes must be backed by
   an ADR/update in `DECISIONS.md` or an explicit plan note.
6. **Tests travel with behavior.** Unit tests prove pure logic; one integration
   check proves the user-visible seam. If a fixture manifest assertion is the
   seam, keep it committed.

## Gate tiers

Use the cheapest gate that matches the risk, then escalate before review or
push.

| Tier | Use when | Command shape |
|------|----------|---------------|
| Inner loop | Editing one crate/module | `cargo check -p <crate>` plus focused `cargo test ... <name>` |
| Alpha slice | A coherent partial capability is ready | focused tests + `just gate-alpha` + the relevant `just gate-phaseN` |
| Reviewer baseline | Before commit/push or handoff | `just gate-smoke` + changed phase gates + `git diff --check` |
| Release | Versioned release readiness | every gate in `ACCEPTANCE.md` |

`just gate-alpha` is intentionally faster than `gate-smoke`: it checks format,
clippy, workspace typechecking, and the committed fixture manifest runner, but it
does not replace focused tests or the relevant phase gate.

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
- Prefer boring data flow over framework gravity: DTOs in `vixen-api`, pipeline
  state in `Page`, browser-facing adapters in headless/CDP/shell.
- Avoid duplicate parsers/matchers. If a Page string projection and a JS host
  object both need behavior, extract or call the same Rust
  module.
- Remove obsolete string-smoke shims as host objects replace them. Do not leave
  two authoritative paths for the same supported expression.
- Keep `COMPAT.md` honest: partial support is fine, overclaiming is not.

## Alpha definition of done

A dev/alpha slice is done when:

- the supported subset is named,
- unsupported inputs fail closed,
- docs mention the current state and next widening step,
- focused tests and the relevant gate pass,
- `git diff --check` is clean,
- any known debt is either removed immediately or named as the next tock.
