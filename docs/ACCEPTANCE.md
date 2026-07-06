# Vixen acceptance criteria

Release is "done" when every gate below passes. Per-capability criteria
are expressed as fixture passes plus specific invariants; this document
does not re-list the web-platform features that come from the upstream
crates (see [`SPEC.md`](SPEC.md) for Vixen's actual contracts).

---

## Hard gates (release-blocking for v1.0)

- [ ] `crates/` Rust LOC ≤ 20 k
- [ ] `crates/` unique `Cargo.lock` dependencies ≤ 220
- [ ] `rg -e 'boa_engine|boa_runtime|taffy|tiny-skia|fontdue' Cargo.lock`
      returns nothing
- [ ] One display list, one paint path, two `GlContext` impls (per
      ADR-003 / ADR-006) — no CPU rasterizer, no fallback painter, no
      `PaintBackend` trait
- [ ] No `sandbox.rs`, no `process_pool.rs`, no `ipc/` (per ADR-004)
- [ ] No WebKit dependency, no `engine-webkit` feature (per ADR-002)
- [ ] GUI renders a real web page to the screen via WebRender (manual
      smoke on `fixtures/realworld/` shows visible content — no static
      placeholders)
- [ ] `vixen-headless` reproduces every flag in `SPEC.md` "Headless CLI
      surface" with stable error codes preserved
- [ ] WPT CSS+DOM fixture share ≥ 70 %
- [ ] Binary sizes meet §"Binary size gates" below
- [ ] `docs/COMPAT.md` published with honest capability matrix
- [ ] `cargo audit` clean; `cargo deny` checks pass
- [ ] `just check-all-host` passes
- [ ] No non-test module > 1,000 lines
- [ ] All fuzz targets stable at 1 M iterations

---

## Per-capability acceptance

Each capability is "done" when its fixture set passes. Where
`SPEC.md` pins a specific invariant, it's called out explicitly.

### CSS cascade

**Done when** every fixture in `fixtures/css/` passes.

### Selectors

**Done when** every selector fixture passes plus the dedicated
selector-corpus fixture set (covering `:has()`, `:is()`, `:where()`,
the user-action and form pseudo-classes, link history tracking).

### DOM

**Done when** every fixture in `fixtures/dom/` passes, and the
composed event dispatch invariants from `SPEC.md` hold (enforced by a
dedicated `fixtures/events/focus-order.html`).

### Layout

**Done when** every fixture in `fixtures/css/` that exercises layout
passes its visual-hash check, and nested-container coordinates are
correct *without* any post-pass fixup. A realworld fixture set
(`fixtures/realworld/`) renders without obvious breakage.

Documented gaps allowed in `docs/COMPAT.md`: writing modes,
page fragmentation (post-v1.0).

### Paint

**Done when**:

- GUI path renders to `gtk4::GLArea` via WebRender (manual smoke)
- Headless path uses EGL surfaceless (per ADR-009) and produces
  pixel-diff ≤ 1 % vs GUI on 5 reference fixtures — both renders go
  through the same WebRender paint path, so this is essentially a
  surface-binding correctness check
- Headless works on CI with `LIBGL_ALWAYS_SOFTWARE=1` + Mesa
  `llvmpipe` (verified)
- Display-list invariants from `SPEC.md` enforced by the display-list
  builder (z-index stacking, clip stacking, opacity groups, visibility
  skip-paint, background clip/origin/attachment)

### JavaScript

**Done when**:

- `vixen-headless --url fixtures/dom/basic.html --eval 'document.title'`
  returns the document title
- SpiderMonkey's bundled test262 passes its default subset against the
  embedded runtime
- Every fixture in `fixtures/dom/`, `fixtures/forms/`,
  `fixtures/network/`, `fixtures/storage/` passes
- Form-validation edge cases from `SPEC.md` enforced exactly (email
  format, URL format, step arithmetic)

### Networking

**Done when** every test in `vixen-net` passes, including the
Vixen-specific configurations from `SPEC.md`:

- URL policy blocklist (including the precise CGNAT check — see
  mandatory regression test below)
- Cookie defaults (Lax default SameSite, 512-entry FIFO cap, HttpOnly
  document-side rejection, safe-method Lax cross-site sending)
- CSP enforcement at script-exec / fetch / plugin-content boundaries
- Permissions API and origin isolation

**Mandatory regression test for the CGNAT check:**

```rust
assert!(is_private_host(&"100.64.0.1".parse::<Ipv4Addr>().unwrap().into()));
assert!(!is_private_host(&"100.128.0.1".parse::<Ipv4Addr>().unwrap().into()));
```

### Storage

**Done when** the redb schema round-trips cookies, fetch-cache,
history, and sessions per `vixen-store` tests, and per-origin
partitioning is preserved.

### Headless CLI

**Done when** every flag in `SPEC.md` "Headless CLI surface" works,
the stable error codes are returned exactly, and the CDP server
responds to every required method. The `--gpu` flag is removed (every
render path is GPU-backed per ADR-003); scripts depending on it should
drop the flag.

### WPT harness

**Done when** `vixen-wpt`:

- Runs the full `fixtures/manifest.json`
- Every check type in `SPEC.md` passes its existing assertions
- The new `ref-equivalent` check works against at least 3 fixtures
- Reports pass rate per category and overall

### Shell

**Done when** manual smoke passes:

- New / close / duplicate tab, reopen closed tab
- Address entry, paste-and-go
- Reload / stop, back / forward
- HTTPS / HTTP / local / failure status feedback
- Find bar
- Zoom
- Preferences, shortcuts, about windows
- Tab status diagnostics for load / TLS / download / permission events
- Engine actually renders page content to the visible window

---

## Binary size gates

Stripped release builds must meet:

| Binary              | System mozjs | Static mozjs |
|---------------------|-------------:|-------------:|
| `vixen` (GUI)       | ≤ 10 MiB     | ≤ 14 MiB     |
| `vixen-headless`    | ≤ 8 MiB      | ≤ 14 MiB     |

Measured via `just size-fp`. Any change exceeding +50 KiB must document
justification in the commit message.

---

## Phase gates summary

Restated from `PLAN.md` as the per-phase acceptance check.

| Phase                             | Gate                                                                                  |
|-----------------------------------|---------------------------------------------------------------------------------------|
| 0 — Scaffolding                   | `cargo check --workspace` passes; `cargo test -p vixen-api` passes                    |
| 1 — Net + store crown jewels      | `cargo test -p vixen-net -p vixen-store` green; fuzz 1 M iters stable                 |
| 2 — SpiderMonkey                  | `just gate-phase2` (`vixen-headless --url <file> --eval '1+2'` returns `3`)           |
| 3 — HTML + Stylo                  | `just gate-phase3`; then WPT CSS fixtures pass with cascade output correct            |
| 4 — Layout                        | `just gate-phase4`; then 20+ visual-hash fixtures match reference                     |
| 5 — Paint                         | `just gate-phase5`; then `just run` shows a page and headless PNG diff ≤ 1 %          |
| 6 — Host bindings                 | `just gate-phase6`; then `fixtures/{dom,events,forms,storage,network}/` all pass      |
| 7 — Security                      | `cargo audit` clean; all security tests green; fuzz stable                            |
| 8 — Headless CDP                  | Every CLI flag works; CDP responds to required methods                                |
| 9 — Release                       | `just gate-smoke` and all gates above green; tag `v1.0.0`                             |

A phase is not done until its gate passes *and* the tock discipline
(dead-code removal, ≤ 1 kLOC modules, references cited) has been observed.

---

## Post-v1.0 scope

Deferred per [`DECISIONS.md`](DECISIONS.md) ADR-007 / ADR-008 and other
implicit non-goals:

- WebKit fallback (rejected, ADR-002)
- Runtime engine switching (rejected, ADR-002)
- macOS / Windows native builds (rejected for v1.0, ADR-007)
- WebGPU (v1.1, via `wgpu`)
- Media playback (v1.1, via GStreamer)
- Writing modes / vertical text (v1.1)
- Page fragmentation / pagination (v1.2)
- Service workers (v1.2)
- WebRTC (not planned)

Byte-for-byte Firefox rendering match is **not** the contract —
behavioural parity on the WPT subset that matters for real sites is.
