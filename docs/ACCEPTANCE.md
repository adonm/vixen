# Vixen acceptance criteria

Release is "done" when every gate below passes. Per-capability criteria
are expressed as fixture passes plus specific invariants; this document
does not re-list the delegated web-platform features or the Vixen-owned
layout subset (see [`SPEC.md`](SPEC.md) and [`COMPAT.md`](COMPAT.md) for the
actual contracts).

Alpha is defined separately in [`PROJECT_DIRECTION.md`](PROJECT_DIRECTION.md):
architecture frozen and validated, with API surface still allowed to move.

---

## Hard gates (release-blocking for v1.0)

- [ ] `crates/` Rust LOC ≤ 20 k
- [ ] `crates/` unique `Cargo.lock` dependencies ≤ 220
- [ ] `rg -e 'boa_engine|boa_runtime|taffy|tiny-skia|fontdue' Cargo.lock`
      returns nothing
- [ ] One display list and one WebRender paint path (per ADR-003 / ADR-006 /
      ADR-018): headless EGL plus bounded GUI texture transport, with no CPU
      rasterizer, fallback painter, or `PaintBackend` trait
- [ ] No `sandbox.rs`, no `process_pool.rs`, no `ipc/` (per ADR-004)
- [ ] No WebKit dependency, no `engine-webkit` feature (per ADR-002)
- [ ] GUI renders a real web page to the screen via WebRender (manual
      smoke on `fixtures/realworld/` shows visible content — no static
      placeholders)
- [ ] `vixen-headless` reproduces every flag in `SPEC.md` "Headless CLI
      surface" with stable error codes preserved
- [ ] WPT target profile in `docs/COMPAT.md` is green; measured pass counts
      are published for every supported category
- [ ] GUI/headless artifact sizes are published by platform and ABI using the
      accepted baseline/regression policy in section "Binary size gates" below
- [ ] `docs/COMPAT.md` published with honest capability matrix
- [ ] `just audit` passes (`cargo audit` + `cargo deny check`)
- [ ] `just check` passes
- [ ] hk hooks are installed or `hk run pre-push --check` passes from a clean
      checkout
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

**Done when** the Vixen-owned Rust layout engine (ADR-013) passes the v1 WPT
target profile in `docs/COMPAT.md`: normal-flow block layout, inline line
boxes, margin/border/padding/box sizing, positioned descendants,
overflow/scroll containers, and useful flex/grid coverage. Nested-container
coordinates must be correct *without* any post-pass fixup. A realworld fixture
set (`fixtures/realworld/`) renders without obvious breakage.

Documented gaps allowed in `docs/COMPAT.md`: tables, floats, full vertical
writing, fragmentation/pagination, and advanced intrinsic sizing.

### Paint and presentation

**Done when**:

- Flutter GUI presents WebRender output through the bounded external-texture
  contract; the GTK GLArea path is accepted only as the temporary Linux
  compatibility baseline
- Headless uses EGL surfaceless (per ADR-009), and GUI/headless reference
  comparisons prove both consume the same WebRender output semantics
- Headless works on CI with `LIBGL_ALWAYS_SOFTWARE=1` + Mesa
  `llvmpipe` (verified)
- Display-list invariants from `SPEC.md` enforced by the display-list
  builder (z-index stacking, clip stacking, opacity groups, visibility
  skip-paint, background clip/origin/attachment)

### JavaScript

**Done when**:

- `vixen-headless --url fixtures/dom/basic.html --eval 'document.title'`
  returns the document title
- The `deno_core`/V8-backed embedded runtime passes the JS smoke/test262 subset
  selected for release
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
- Runs pinned external WPT profiles without vendoring their upstream HTML into
  the repo
- Every check type in `SPEC.md` passes its existing assertions
- The new `ref-equivalent` check works against at least 3 fixtures
- Reports pass count/rate overall, per category, per source, and per
  source×category
- Separates local Vixen fixtures from imported upstream WPT fixtures so release
  notes can state exactly what was measured

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
- BrowserCore remains the sole browser owner; Dart owns chrome/presentation and
  host-service UI only
- Pointer, wheel, keyboard, text/IME, focus, scale, viewport, and lifecycle
  changes cross the generation-checked bridge
- BrowserCore's accessibility projection reaches Flutter Semantics and native
  assistive-technology smoke; texture pixels alone do not satisfy accessibility

The current GTK/Relm4 shell is evidence for the interaction list, not the v1 GUI
target. Linux Flutter parity must pass it before that shell is removed. Flutter's
Linux embedder uses GTK, so removal means no Relm4/libadwaita/custom GLArea
ownership, not necessarily no GTK runtime dependency.

### Platform gates

Flutter 3.44 supports native deployment to all five targets, but Vixen supports a
platform only after its gate in [`FLUTTER_SHELL.md`](FLUTTER_SHELL.md) passes:

- **Linux:** real BrowserCore bridge, bounded RGBA texture, input/viewport,
  Semantics/AT, host services, parity, and pinned offline source-built Flatpak.
- **macOS and Windows:** native BrowserCore/V8/WebRender builds plus texture,
  input/IME, accessibility, host services, signing/packaging, and per-architecture
  size/performance evidence.
- **Android:** pinned V8 source/toolchain, reproducible cross-build, GLES,
  lifecycle/surface recovery, input/IME/accessibility, and split-ABI proof.
- **WebAssembly:** the existing `deno_core`/V8 path passes the same MVP API,
  validation, resource-limit, malformed-module, and conformance suite on every
  declared target.
- **iOS Simulator:** `aarch64-apple-ios-sim` BrowserCore/V8/WebRender plus
  JavaScript/WebAssembly, simulated lifecycle, input/accessibility/host-service
  smoke, and a repeatable Flutter simulator runner on Apple Silicon. Physical
  iOS, TestFlight, and App Store packaging are explicitly outside this gate.

No iOS JavaScriptCore/WebKit, alternate Wasm runtime, or physical-device fallback
is accepted without a new ADR.

---

## Binary size gates

`just size-fp` and `just size-headless` remain measurement commands for the
current GTK compatibility Flatpak and release headless binary. They do not prove
a Flutter artifact. Flutter reports are required for every platform and shipped
ABI/architecture and compare like-for-like release/AOT/stripped hello-Flutter and
Flutter+Vixen builds.

Reports attribute compressed, unpacked/install, executable, and shared-runtime
costs to Flutter engine/ICU, Dart AOT/assets, native runner/plugins,
BrowserCore/Rust, V8/ICU/snapshots, WebRender/GPU dependencies, resources,
packaging, and symbols. They include exact locks/source revisions, commands,
hashes, architecture, AOT/strip/LTO settings, and runtime exclusions.

GUI bundles contain no debug Flutter engines, unstripped symbols, duplicate
ABIs, development snapshots, headless/CDP/WPT tools, source archives, build
tools, or caches without a documented release need. Adopt warning thresholds
only after representative reports are reproduced and reviewed. Hard budgets
follow only after warning behavior establishes variance, ownership, comparison
statistics, platform/ABI scope, and an override policy. No numerical Flutter
artifact budget is currently accepted.

## Performance baseline gates

`just baseline-headless` measures separate committed startup/version,
navigation/runtime, layout, paint/display-list, and screenshot controls. It
reports wall-time and best-effort Linux process-memory samples with artifact,
git, toolchain, host, and renderer fingerprints. `just baseline-profile-growth`
measures an opaque temporary profile after deterministic repeated and unique
visits and verifies localStorage after reopening it. `just baseline-beta` runs
those hermetic controls plus headless artifact sizing.

This completes the local Linux latency, memory, profile-growth, headless, and
compatibility-shell artifact-size measurement foundation. All values remain
measurement-only until the
accepted-report process in [`BASELINES.md`](BASELINES.md) produces reviewed host
baselines and explicit policies here. Real external-site measurements, the
GUI/Flatpak host matrix, frame time, JS heap, and transfer throughput remain
unmeasured; do not turn local controls into complete-site or release-budget
claims.

---

## Phase gates summary

Restated from `PLAN.md` as the per-phase acceptance check.

| Phase                             | Gate                                                                                  |
|-----------------------------------|---------------------------------------------------------------------------------------|
| 0 — Scaffolding                   | `just gate-phase0` passes                                                             |
| 1 — Net + store crown jewels      | `just gate-phase1` passes                                                             |
| 2 — JS runtime                    | `just gate-phase2` (`vixen-headless --url <file> --eval '1+2'` returns `3`); runtime is `deno_core` per ADR-014 |
| 3 — HTML + Stylo                  | `just gate-phase3`; then WPT CSS fixtures pass with cascade output correct            |
| 4 — Vixen-owned layout            | `just gate-phase4`; then the v1 WPT layout target profile in `docs/COMPAT.md` is green |
| 5 — Paint                         | `just gate-phase5`; then `just run` shows a page and headless PNG diff ≤ 1 %          |
| 6 — Host bindings                 | `just gate-phase6`; then `fixtures/{dom,events,forms,storage,network}/` all pass      |
| 7 — Security                      | `just audit` clean; all security tests green; fuzz stable                             |
| 8 — Headless CDP                  | Every CLI flag works; CDP responds to required methods                                |
| 9 — Release                       | `just gate-smoke` and all gates above green; tag `v1.0.0`                             |

A phase is not done until its gate passes *and* the tock discipline
(dead-code removal, ≤ 1 kLOC modules, references cited) has been observed.

---

## Post-v1.0 scope

Deferred per [`DECISIONS.md`](DECISIONS.md) ADR-008 and other explicit non-goals:

- WebKit fallback (rejected, ADR-002)
- Runtime engine switching (rejected, ADR-002)
- WebGPU (v1.1, via `wgpu`)
- Media playback (v1.1, via GStreamer)
- Full writing modes / vertical text (v1.1)
- Tables, floats, advanced intrinsic sizing (v1.1/v1.2, WPT-prioritized)
- Page fragmentation / pagination (v1.2)
- Service workers (v1.2)
- WebRTC (not planned)

Byte-for-byte Firefox rendering match is **not** the contract —
behavioural parity on the WPT subset that matters for real sites is.
