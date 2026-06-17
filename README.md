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
and reference material, plus:
- **Phase 0** — scaffolding (workspace + 7 crates).
- **Phase 1** — networking/security "crown jewels" (`vixen-net`, `vixen-store`).
- **Phase 2** — the SpiderMonkey runtime (`vixen-core::script`) and the
  `vixen-headless` CLI; the gate `vixen-headless --url <file> --eval '1+2'` →
  `3` passes.
- **Phase 3 (in progress)** — HTML parsing (`vixen-core::doc`,
  html5ever → RcDom) with `--dump-dom`/`--extract-text`; **selector matching
  via Stylo** (`vixen-core::style_dom` implementing `selectors::Element` over
  the RcDom), driving `--extract-selector` and the WPT selector fixtures;
  and the **WPT harness** (`vixen-wpt`: manifest + runner + all 13 check
  types). The full Stylo cascade (`TNode`/`TElement`/`TDocument` +
  `Stylist::update_stylist` + `computed_values_for(node_id)`) is the next
  slice; Stylo arrives via the crates.io-published `stylo` crate per
  ADR-011 (no Servo git dep).
- **Phase 6 prep** — pure form-constraint validation in `vixen-core::forms`
  (email/URL formats, step arithmetic, range/length flags) ready for the
  script-layer host hooks.
- **Phase 8 (partial)** — the CDP WebSocket server (`vixen-headless::cdp`)
  responds to the six required methods (`Browser.getVersion`,
  `Target.createTarget`, `Target.attachToTarget`, `Page.navigate`,
  `Page.loadEventFired`, `Runtime.evaluate`) with stable error codes.

Source for later phases lands per [`docs/PLAN.md`](docs/PLAN.md).

---

## Setup

Workspace setup is managed by [mise](https://mise.jdx.dev) for the Rust
toolchain, `just`, and project tooling:

```sh
mise trust
mise bootstrap --yes     # Rust toolchain + just + dev tooling + build check
just check-all-host      # type-check the workspace
just test-host           # run host-runnable tests
```

`mise bootstrap` also points `CARGO_HOME` at `<workspace>/.cargo` so the
Cargo registry cache and `cargo-binstall`-ed tooling stay inside the
workspace (see [`docs/guidance/cargo-home.md`](docs/guidance/cargo-home.md)).

**The GNOME 50 SDK is not installed on the host** — it is managed inside a
`flatpak-builder` container, so host churn stays at zero and the build is
reproducible. To build against the SDK (the shell, or the Flatpak):

```sh
just flatpak-update-sdk  # pull the image (= install the GNOME 50 SDK)
just flatpak-build       # flatpak-builder against org.gnome.Sdk//50 in the container
```

See [`docs/guidance/gnome-sdk-flatpak-builder.md`](docs/guidance/gnome-sdk-flatpak-builder.md)
for the full workflow. Headless/CI hosts that only build `vixen-api` /
`vixen-net` / `vixen-store` need neither the SDK nor the container —
`mise install` + `just check-all-host` is enough.

See [`.mise.toml`](.mise.toml) and the
[mise bootstrap guide](https://mise.jdx.dev/bootstrap.html). The library
MSRV is 1.88 (let-chains); the dev toolchain floats to latest stable.

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
| [`docs/guidance/`](docs/guidance)           | **How to do specific tasks.** e.g. the GNOME SDK via flatpak-builder containers. |
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
