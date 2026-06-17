# Vixen

A small, GNOME-native web browser built in Rust on Firefox-family
components. Targets Firefox-grade compatibility with the smallest credible
binary.

The hard, spec-heavy, easy-to-get-wrong subsystems (CSS cascade, HTML
parsing, JS, selector matching, GPU paint) are delegated to the same
Mozilla crates Firefox and Servo use — **Stylo** for CSS, **SpiderMonkey**
for JS, **WebRender** for paint, **html5ever** for HTML. Vixen itself is
the product glue: a libadwaita shell, a networking/security layer, a
persistence layer, headless tooling, and the integration code that wires
the upstream crates together.

---

## Status

Pre-v1.0. This repository contains the specification, architecture, plan,
and reference material. Source code lands starting at Phase 0 of
[`docs/PLAN.md`](docs/PLAN.md).

---

## Repository map

| Path                                        | Purpose                                                       |
|---------------------------------------------|---------------------------------------------------------------|
| [`docs/SPEC.md`](docs/SPEC.md)              | **What Vixen must do.** Capabilities, CLI, behaviour contracts. |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | **How Vixen is structured.** Crates, data flow, trust boundaries, trait APIs. |
| [`docs/DECISIONS.md`](docs/DECISIONS.md)    | **Why these choices.** ADR-style records for the major decisions. |
| [`docs/PLAN.md`](docs/PLAN.md)              | **How to build it.** Phased execution runbook with phase gates. |
| [`docs/REFERENCES.md`](docs/REFERENCES.md)  | **Where to look for truth.** Pinned reference trees + how to consult each. |
| [`docs/ACCEPTANCE.md`](docs/ACCEPTANCE.md)  | **When it's done.** Release gates per capability. |
| `LICENSE`                                   | Apache 2.0 (lands at Phase 0). |

---

## Reading order

If executing the build:

1. `docs/SPEC.md` — the contract
2. `docs/ARCHITECTURE.md` — the shape
3. `docs/DECISIONS.md` — confirm the choices
4. `docs/PLAN.md` — the runbook
5. `docs/REFERENCES.md` — consult as integration questions arise
6. `docs/ACCEPTANCE.md` — check against, every phase

If evaluating the project: read `docs/SPEC.md` and
`docs/DECISIONS.md`, then sample `docs/PLAN.md`.

When a doc and a decision record disagree, the **decision record wins**.
Update both when resolving.

---

## Working assumptions

- Target platform: **Linux + GNOME 50 SDK**, distributed via Flatpak.
  Other platforms are best-effort, not a release blocker.
- Build profile is optimal already and carries forward unchanged:
  `strip = true`, `lto = "thin"`, `codegen-units = 1`, `panic = "abort"`.
- App IDs: `org.vixen.Vixen` (production), `org.vixen.Vixen.Devel` (devel).

## License

Apache 2.0 — see [`LICENSE`](LICENSE).
