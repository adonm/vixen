# Decision records

Architecture decisions for Vixen, recorded ADR-style. Each entry carries
context, the decision, the alternatives considered, and the consequences.

When a future decision reverses one of these, append a new entry that
supersedes it; do not edit the originals.

---

## ADR-001: Build on Firefox-family components, not from scratch

**Status:** accepted

**Context.** Vixen's goal is Firefox-grade web compatibility at the
smallest credible binary size. Every modern browser engine that reaches
that grade — Firefox, Chromium, Servo, WebKit — has person-decades of
work in its CSS cascade, layout, JS runtime, and paint pipeline. Building
any of these from a blank slate is a multi-year undertaking that
guarantees perpetual trailing-edge compatibility.

**Decision.** Delegate the spec-heavy subsystems to the same Mozilla
crates Firefox and Servo use:

| Subsystem        | Crate                              |
|------------------|------------------------------------|
| HTML parsing     | `html5ever`                        |
| CSS cascade      | `style` (Stylo)                    |
| Selector matching| `selectors`                        |
| String interning | `string_cache`, `servo_arc`        |
| JS engine        | `deno_core` / V8 embedding           |
| Layout           | Servo `layout_2020` crate          |
| Paint            | `webrender` + `gleam` + `euclid`   |

Vixen writes only: the integration glue, the product shell, the
networking/security layer, the persistence layer, and the headless
tooling.

**Alternatives considered.**

- *Build everything from scratch.* Rejected: see Context. Cannot reach
  Firefox-grade compatibility on any realistic timeline.
- *Embed Servo whole via `libservo`.* Rejected: ~80+ MiB binary, hundreds
  of transitive deps, unstable embedding API, fights Servo's own
  networking/storage story. The selected crates get the same
  compatibility at a fraction of the binary size.

**Consequences.**

- Vixen's web compatibility is roughly Servo's, which is roughly
  Firefox's. The compat ceiling is upstream; Vixen's job is the product
  around it.
- Vixen tracks Servo crate releases. Major upstream API changes
  (e.g. Stylo `TElement` trait evolution) require integration updates,
  typically every 6–12 months.
- Binary size grows by the volume of these crates. Runtime packaging and size
  are remeasured against the active `deno_core`/V8 dependency.

---

## ADR-002: Single-engine project, no fallback engine

**Status:** accepted

**Context.** A browser project can support multiple engines behind an
abstraction (e.g. WebKit + custom, switchable at compile time or runtime).
This doubles the maintenance surface for no end-user win: every shell
change must be validated against both engines, dependency isolation
requires constant auditing, and only one engine can be the production
path anyway.

**Decision.** Vixen has exactly one engine: the Servo-component-backed
engine described in ADR-001. There is no WebKit fallback, no compile-time
engine selection, no runtime engine switching.

**Alternatives considered.**

- *WebKitGTK as production + custom engine as preview.* Rejected: at
  that point the project is a WebKitGTK wrapper, not a browser engine
  project. If WebKitGTK is the goal, use GNOME Web directly.
- *Compile-time engine feature flag (one binary, either engine).* Rejected:
  adds dep-leak gates, doubles test matrix, no end-user benefit.

**Consequences.**

- One engine to test, one engine to ship, one engine to document.
- If the Servo-component path ever proves unworkable, a WebKit adapter
  against the `Engine` trait is a small, contained addition — but it is
  not the v1.0 plan.
- Web compatibility is Servo-grade, not WebKit-grade. The two are
  different; document which one Vixen targets.

---

## ADR-003: GPU-only — no CPU paint path

**Status:** accepted

**Context.** A browser paint pipeline can have one paint path (GPU) or
two (GPU + a software CPU fallback). A CPU fallback exists traditionally
for two reasons: headless screenshot generation without a display, and
CI without GPU access.

**Decision.** Vixen has exactly one paint path: WebRender against a
GPU context. The GUI uses `gtk4::GLArea` (EGL/GLX). Headless uses EGL
surfaceless (`EGL_MESA_platform_surfaceless`, with
`EGL_KHR_surfaceless` + pbuffer as fallback) — same WebRender, no
display server required. There is no `tiny-skia`, no `fontdue`
rasterizer, no CPU paint path.

**Alternatives considered.**

- *CPU fallback for headless and CI.* Rejected: GPU is a reasonable
  requirement for a GNOME Flatpak daily driver (every target device has
  one), EGL surfaceless covers headless/CI without a second renderer to
  maintain, and removing the CPU path collapses four duplicated
  painters into one.
- *Headless Wayland compositor as the headless path.* Rejected as the
  *default*: EGL surfaceless is sufficient for screenshot/CDP pipelines
  and adds no runtime deps. A headless Wayland compositor (`weston` or
  `cage` on a virtual output) is supported as an opt-in fallback when
  full compositor semantics (pointer focus, XDG toplevel) are needed
  for CDP interaction tests.

**Consequences.**

- One renderer to test, one renderer to maintain. Display-list changes
  ripple to exactly one paint path.
- Headless requires a GPU device (even if virtual). CI must provide one
  (Mesa software rasterizer via `llvmpipe` is sufficient; most CI
  runners already have it).
- No binary-size cost from `tiny-skia`/`fontdue` and their font raster
  pipelines. WebRender has its own glyph atlas.
- If a truly GPU-less environment is ever required (embedded, server),
  treat it as a separate v1.x target with its own paint path — not the
  v1.0 plan.

---

## ADR-004: Drop the multi-process JS sandbox

**Status:** accepted

**Context.** A previous design used a process-per-origin JS sandbox
(spawned binaries communicating over IPC) for isolation. The embedded JS runtime
already provides in-process context isolation, and out-of-process isolation
(proper OOPIF) is a separate, much larger effort.

**Decision.** Single-process engine. JS isolation is via runtime contexts (one
per origin once host bindings are widened). No `JsSandbox`, no `JsSandboxPool`,
no `process_pool`, no `ipc` module.

**Alternatives considered.**

- *Keep the multi-process sandbox.* Rejected: the complexity (IPC
  framing, pool management, origin-keyed spawn) is not justified by the
  security payoff for a single-user browser. Site isolation, if ever
  needed, is a future Servo-style OOPIF effort.

**Consequences.**

- ~1.5 kLOC less code.
- A single malicious page can still OOM or hang the engine process. This
  matches every other browser's pre-OOPIF behaviour.
- If genuine site isolation becomes a v1.x goal, design it as OOPIF
  against the upstream Servo pattern, not as a forked-engine-per-origin
  approach.

---

## ADR-005: JS runtime packaging size gate

**Status:** superseded by ADR-014

**Context.** The old runtime-packaging decision optimized around shared/static
packaging for the previous JS engine. ADR-014 changed the active runtime to
`deno_core`/V8, so those package-specific details are no longer active guidance.

**Decision.** Re-measure release binaries with the active `deno_core`/V8
dependency. Do not carry forward pre-ADR-014 runtime size assumptions as release
promises.

**Alternatives considered.**

- *Keep historical runtime-packaging guidance around for builds.* Rejected: it
  no longer matches the dependency graph and creates false release expectations.

**Consequences.**

- `just size-fp` is the source of truth for current binary-size budgets.
- Distribution guidance should discuss `deno_core`/V8 artifacts and cache
  behavior, not removed runtime dependencies.

---

## ADR-006: One display list, one paint path, two GL surfaces

**Status:** accepted

**Context.** A previous design had four parallel paint implementations
(CPU compositor, CPU renderer, "GPU renderer" that was actually CPU,
text/glyph rasterizer), each duplicating the draw-command dispatch
logic. Even a "two backend" design (WebRender + a software fallback)
duplicates the dispatch contract.

**Decision.** Vixen has one `DisplayList` type defined in `vixen-engine`
and exactly one paint path (WebRender). There is no `PaintBackend`
trait — a single-impl trait would be dead abstraction. WebRender
consumes a small `GlContext` trait (defined in `vixen-api`, so
`vixen-engine` stays GTK- and EGL-free) with two implementations:

- `GlAreaSurface` (in `vixen-shell`) — wraps `gtk4::GLArea`, used by GUI.
  GL work runs inside the `GLArea::render` signal, where GTK has already
  made the `gdk::GLContext` current.
- `SurfacelessSurface` (in `vixen-headless`) — wraps an EGL surfaceless
  context, used by headless screenshots, CDP, and CI.

Both surfaces produce the same `webrender::Renderer`; the only
difference is the `GlContext` implementation behind it.

**Alternatives considered.**

- *One display list, two backends (WebRender + tiny-skia).* Rejected per
  ADR-003: GPU is a reasonable requirement, EGL surfaceless covers the
  headless case, and avoiding a second backend removes a large
  maintenance surface.
- *A `PaintBackend` trait with one impl.* Rejected: a single-impl trait
  is premature abstraction and contradicts the "one paint path" goal.
  The `GlContext` trait (two impls) is the only seam that earns its keep.
- *Generate backends from a shared spec.* Rejected: the display list
  is the spec; sharing it is sufficient.

**Consequences.**

- Adding a new paint command requires one change in the display-list
  builder and zero renderer changes (WebRender handles it).
- The display list is a stable internal API; changes ripple
  predictably.
- The GL↔WebRender seam is the `GlContext` trait in `vixen-api`, not a
  vixen-engine type — keeping GL details out of engine internals and GTK
  out of vixen-engine.
- CI must provide a GPU device (Mesa `llvmpipe` is sufficient).

---

## ADR-007: GNOME-only target at v1.0

**Status:** accepted

**Context.** Cross-platform browsers either limit themselves to what the
upstream crates already abstract (Servo works on macOS/Windows) or carry
large per-platform shims (GTK on macOS via Quartz, etc.). Vixen's product
goal is a GNOME browser.

**Decision.** v1.0 targets Linux + GNOME 50 SDK only. Distribution via
Flatpak. Other platforms are best-effort (if Servo crates happen to work,
fine; no release blocker).

**Alternatives considered.**

- *Cross-platform from day one.* Rejected: dilutes focus. If a macOS or
  Windows port becomes a goal, design it as a v1.x effort with its own
  shell crate.

**Consequences.**

- Shell uses GTK4/libadwaita unconditionally.
- Flatpak manifest is the canonical distribution.
- macOS/Windows users have no v1.0 path. Documented as a non-goal.

---

## ADR-008: WebGPU and media are post-v1.0

**Status:** accepted

**Context.** WebGPU and media playback are real features but require
substantial integration work (`wgpu` surface sharing with WebRender;
GStreamer pipeline + element wiring). Neither is on the critical path
for a useful daily browser.

**Decision.**

- **WebGPU**: not in v1.0. Land via `wgpu` (which has its own WGSL
  compiler and pipeline model) in v1.1.
- **Media (`<audio>`, `<video>`)**: not in v1.0. Land via GStreamer
  bindings in v1.1.

**Alternatives considered.**

- *Build WebGPU/media scaffolding now, fill in backends later.* Rejected:
  scaffolding without backends is dead code that rots and misleads users.

**Consequences.**

- v1.0 cannot run WebGPU demos or play videos. Documented in
  `docs/COMPAT.md`.
- v1.1 scope includes both.

---

## ADR-009: Headless render path is EGL surfaceless, not a CPU rasterizer

**Status:** accepted

**Context.** With ADR-003 committing to GPU-only, the headless path
needs a GPU context. Two viable approaches: EGL surfaceless (a GPU
context with no display server) or a headless Wayland compositor
(`weston`/`cage` on a virtual output). A third option — a CPU
rasterizer — is rejected by ADR-003.

**Decision.** EGL surfaceless (`EGL_MESA_platform_surfaceless`, with
`EGL_KHR_surfaceless` + pbuffer as fallback) is the default headless
render context. WebRender renders into a framebuffer object;
`glReadPixels` extracts RGBA; `png` encodes the screenshot. No display
server is needed.

A headless Wayland compositor is supported as an opt-in via
`VIXEN_HEADLESS_WAYLAND=1` for tests that need full compositor
semantics (pointer focus, XDG toplevel, real input events).

**Alternatives considered.**

- *Headless Wayland as the only path.* Rejected: requires running a
  compositor (extra runtime dep, slower startup, more moving parts).
  EGL surfaceless is simpler and covers 95% of headless use cases
  (screenshots, CDP screenshots, layout dump).
- *CPU rasterizer (`tiny-skia`).* Rejected per ADR-003.

**Consequences.**

- Headless requires a GPU device even on CI. Mesa's `llvmpipe`
  software rasterizer satisfies this; most CI runners already provide
  it via `LIBGL_ALWAYS_SOFTWARE=1` if no hardware GPU is present.
- CDP interaction tests that depend on real focus events may need the
  Wayland fallback. Document this in `vixen-headless/README.md`.
- One render path to test across GUI and headless; bugs reproducible in
  either context.

---

## ADR-010: Idiomatic Relm4 shell

**Status:** accepted

**Context.** The shell can be written in three styles against Relm4:
(1) hand-rolled GTK with Relm4 only as an app entry point, (2) Relm4
components for top-level windows but hand-rolled widget management
inside, (3) fully idiomatic Relm4 with factories, workers, components,
and `relm4-components` reuse.

**Decision.** Vixen's shell is fully idiomatic Relm4 (style 3):

- **Tabs** are a `FactoryVecDeque<TabModel>` — dynamic add/remove via
  factory, no hand-rolled `Vec<TabState>` + ad-hoc signal handlers.
- **Each tab** is a `Component` with its own model/update/view, owning
  an `EngineWorker`.
- **Engine ownership** is via `relm4::Worker` — one worker per tab, on
  a background thread. The worker holds the `Box<dyn Engine>` and
  forwards `EngineDelegate` callbacks as messages to the tab component.
  The shell thread never blocks on engine work.
- **Address bar, find bar, status row, preferences rows** are each a
  `Component`, not hand-rolled widgets.
- **`relm4-components`** is the first stop for any standard widget
  (`Alert`, `SimpleAdwComboBox`, `ComboRow`, `Dialog`, `Toast`,
  `LoadingButtons`, `consts::CSS_CLASSES`). Reinventing any of these is
  a code-review blocker.
- **Workers** for any non-trivial background task: history writes,
  screenshot encoding, CDP I/O.

**Alternatives considered.**

- *Hand-rolled GTK, Relm4 as entry point only.* Rejected: produces
  larger shell code, harder to reason about, doesn't benefit from
  upstream component maintenance.
- *Partial Relm4 (windows only, hand-rolled internals).* Rejected:
  splits the codebase across two idioms; bugs slip through the seam.

**Consequences.**

- Shell source is smaller and more uniform.
- Engine ↔ shell message flow is explicit: `EngineWorker` emits
  `EngineMsg::{UriChanged, TitleChanged, ...}` consumed by the tab
  component's `update`.
- Engine callbacks never run on the shell thread directly; no
  re-entrancy, no GTK mutate-from-background bugs.
- Stronger dependency on Relm4 upstream. Track `relm4` releases;
  breaking changes (rare) require shell-side updates.
- `.tmp/ref/relm4/examples/` and `.tmp/ref/relm4/relm4-components/` are the
  primary reference for any new shell widget.

---

## ADR-011: Stylo via the crates.io-published `stylo` crate

**Status:** accepted

**Context.** ADR-001 commits to Stylo (`style`) for the CSS cascade.
When Phase 0–2 landed, `style` was only available as a Servo git
dependency — a clone of `https://github.com/servo/servo` plus a
`[patch.crates-io]` table. That made the build non-reproducible from
crates.io alone and left Phase 3 marked "blocked" in `docs/PLAN.md`.

Since then, the Stylo team split the engine out of the Servo monorepo
into `https://github.com/servo/stylo` and now publish it on crates.io
as [`stylo`](https://crates.io/crates/stylo) (lib name `style`). All
subsystems Vixen needs — cascade, selector matching, rule tree,
computed values — are in that crate.

**Decision.** Depend on `stylo = "0.18"` (with the `servo` feature for
the non-Gecko config) directly. Do not pull a Servo git checkout, do
not patch crates.io, do not vendor the source. Implement
`selectors::Element` (and, for the cascade, `TNode`/`TElement`/
`TDocument`) over Vixen's html5ever `RcDom` in
`crates/vixen-engine/src/style_dom.rs`.

**Alternatives considered.**

- *Hand-roll selector matching on top of `selectors` alone, defer the
  cascade.* Rejected: doubles the selector-matching surface (Vixen's
  plus Stylo's), and the cascade is the actual reason we wanted Stylo
  in the first place.
- *Pin a Servo git revision of `style`.* Rejected: bigger dep surface
  (the whole `servo` repo at that revision), non-reproducible from
  crates.io, blocks Phase 3 indefinitely.
- *Switch CSS engine to `taffy` or another standalone cascade.* Rejected
  per ACCEPTANCE.md hard gates (no `taffy`); also re-introduces the
  perpetual trailing-edge compatibility ADR-001 rejects.

**Consequences.**

- Phase 3 unblocks. The selector-matching surface (`vixen-engine::
  style_dom`) is live; the WPT selector fixtures pass end-to-end.
- The crate ships with its lib name as `style` even though the package
  is `stylo`; source uses `use style::…` while `Cargo.toml` says
  `stylo = …`. Documented in `style_dom.rs` to head off confusion.
- Dep budget: ~45 additional crates (icu, euclid, rayon, etc.). The
  Phase 9 dep-count gate (≤ 220) remains the release-blocking contract;
  this is the right trade for getting real Firefox-grade cascade.
- Future Stylo releases may shift trait shapes (`TElement` etc.). Pin
  `stylo = "0.18"` and bump deliberately; track upstream
  `https://github.com/servo/stylo/releases`.

---

## ADR-012: Verify and pin the layout source before the full layout adapter

**Status:** accepted

**Context.** ADR-001 selected Servo `layout_2020` for layout. The refreshed
Firefox/Servo reference pin (`46e9f12a8f9b`) no longer contains the historical
`servo/components/layout_2020/` or `servo/components/layout/` trees; the
current Servo subtree under Firefox contains Stylo and selector support only.
Continuing to cite removed paths would make implementation decisions
non-reproducible.

**Decision.** Keep the Phase 4 executable line-layout slice behind
`vixen_engine::page::Page`, but do not add a full layout dependency or cite
historical Servo layout paths until a current Rust layout source is verified
and pinned in `docs/REFERENCES.md`. If no maintained Servo-family layout source
is available, narrow the v1.0 layout scope explicitly in `docs/COMPAT.md`
rather than silently swapping to an unrelated fallback crate.

**Alternatives considered.**

- *Keep citing the old Firefox/Servo layout paths.* Rejected: those paths are
  absent from the current reference pin and violate the citation discipline.
- *Switch immediately to an unrelated layout crate.* Rejected: ADR-001's
  compatibility rationale still applies; a fallback would need its own ADR and
  acceptance impact.

**Consequences.**

- Phase 3 Stylo work remains unblocked: `style`/`selectors` are present and
  pinned via the current Firefox/Servo reference and the crates.io `stylo`
  dependency.
- Phase 4 can continue with vertical `Page` fixtures and pure layout helpers,
  but the full positioned-box-tree adapter has an explicit source-selection
  gate.
- Future layout commits must cite either the new layout source pin or the
  narrowed v1.0 compatibility document, not historical `layout_2020` paths.

---

## ADR-013: Vixen-owned Rust layout, Ladybird architecture reference

**Status:** accepted

**Supersedes:** ADR-001's `layout_2020` layout row and ADR-012's open
source-selection gate.

**Context.** The refreshed Firefox/Servo reference pin no longer contains a
maintained Rust layout crate. Keeping layout blocked on historical Servo paths
would stop the vertical browser slices, while switching to a generic UI layout
crate would not implement web layout semantics. Ladybird's LibWeb layout stack
at `0de15a5dd2a9` is a current, readable browser-layout architecture:
`Libraries/LibWeb/Layout/TreeBuilder.cpp` centralizes DOM-to-layout-tree
construction, `Libraries/LibWeb/Layout/*FormattingContext*` separates block,
inline, flex, grid, and table algorithms, and `Libraries/LibWeb/Painting/`
keeps paint/display-list construction behind a later seam.

**Decision.** Vixen owns its layout engine in Rust. Stylo remains the CSS
cascade/computed-value source, `deno_core` remains the JS runtime, and WebRender
remains the only paint backend. The layout layer follows Ladybird's
architecture but uses Rust/data-oriented internals: stable `NodeId` /
`LayoutNodeId` handles, arenas, compact structs/enums, explicit dirty bits,
cached intrinsic sizes, and deterministic formatting-context passes.

The v1.0 layout target is not "all of CSS layout." It is the subset needed for
simple real pages and the release WPT profile: normal-flow block layout, inline
line boxes, basic replaced elements, margin/border/padding/box sizing,
positioned descendants, overflow/scroll containers, and useful flex/grid
coverage. Tables, floats, fragmentation, full vertical writing, advanced
intrinsic sizing, and complete print/page layout are post-v1 unless promoted by
WPT/real-site evidence.

**Alternatives considered.**

- *Keep waiting for Servo `layout_2020`.* Rejected: it is absent from the
  current Firefox/Servo reference pin and would leave Phase 4 without a
  reproducible source path.
- *Use a generic UI layout crate.* Rejected: UI-layout crates do not implement
  web layout semantics, cascade interactions, inline formatting, fragmentation,
  or WPT-compatible CSS behavior.
- *Port Ladybird C++ directly.* Rejected: Vixen should reuse the architecture
  and tests, not import C++ ownership patterns or create a transliteration that
  fights Rust.

**Consequences.**

- Vixen's compatibility claim narrows: Firefox/Servo-family cascade, selector,
  JS, and paint components, but Vixen-owned layout with WPT-gated coverage.
- Layout becomes a core Vixen subsystem and a multi-phase effort. The plan must
  prefer small vertical slices through `Page`, not large unexercised layout
  modules.
- Every layout semantic decision cites either Ladybird layout/painting paths at
  `0de15a5dd2a9` for architecture or Firefox/Stylo/WebRender paths at
  `46e9f12a8f9b` for computed values and rendering contracts.
- `docs/COMPAT.md` is release-blocking and must state the WPT profile, achieved
  pass rates, and known layout gaps honestly.

---

## ADR-014: Move JS runtime to `deno_core`

**Status:** accepted

**Supersedes:** ADR-001's JS-engine row, ADR-004's SpiderMonkey compartment
wording, and ADR-005's mozjs packaging decision.

**Context.** The first Phase 2 implementation used `mozjs` because the original
plan optimized for Firefox-family components end-to-end. The later Phase 6 work
showed that Vixen's actual risk is the Rust-side host API layer: object
registration, bootstrap JS packaging, resource/permission boundaries, testing,
and long-term maintenance of many Web API families. The `deno_core` crate solves
that packaging problem directly. It brings a well-maintained Rust embedding layer
for V8, explicit extension/op registration, module loading, resource tables,
structured errors, and the runtime architecture Deno uses to expose large Web API
surfaces from Rust.

`deno_core` does mean Vixen no longer uses a Firefox-family JS engine. That is an
acceptable trade: JS language compatibility comes from V8, Web API compatibility
remains Vixen-owned and fixture/WPT-gated, and Rust host-layer velocity matters
more for alpha progress than preserving SpiderMonkey specifically.

**Decision.** Migrate Vixen's JS runtime from `mozjs`/SpiderMonkey to
`deno_core`/V8 and use `deno_core` directly inside `vixen-engine::script`. Do
not introduce a generic JS-engine abstraction or a `dyn JavaScriptRuntime` layer:
Vixen has one JS runtime target, and `deno_core` already provides the embedding
API shape we want. The migration has landed behind the existing
`JsRuntime`/`JsValue`, headless `--eval`, and CDP `Runtime.evaluate` seams.

The target JS architecture is Deno-shaped:

- Host API families live in small modules under `vixen-engine::script` or pure
  sibling modules, not as one ever-growing `script.rs` file.
- Each family has a Rust op/resource surface, a JS bootstrap surface, and
  focused tests. The Rust side owns validation and stable errors; JS glue owns
  Web-shaped object ergonomics only.
- Registration uses a Deno-style extension list: ordered, explicit, testable,
  and feature-family scoped (`encoding`, `dom`, `url`, `fetch`, `storage`, etc.).
- Long-lived host state should use explicit resource IDs/handles and permission
  checks near the op boundary, following `deno_core`/Deno resource-table and
  permissions patterns rather than ad-hoc globals.
- Bootstrap JS is packaged as static assets or generated strings owned by the
  feature module, with Rust tests proving the installed surface.

**Alternatives considered.**

- *Stay on SpiderMonkey and only mimic Deno packaging.* Rejected: it keeps the
  hard part — building and maintaining a browser-scale Rust host layer — while
  missing the maintained `deno_core` abstractions that solve that exact problem.
- *Abstract over `mozjs` and `deno_core` behind an internal JS-engine trait.*
  Rejected: it would preserve two runtime mental models, hide useful
  `deno_core` concepts like extensions/resources/ops behind a leaky common
  denominator, and create a test matrix Vixen does not intend to support.
- *Keep all host glue inside `script.rs`.* Rejected: it does not scale past the
  first few host-object slices and hides feature-family boundaries.
- *Adopt Deno wholesale, including CLI/npm/Node compatibility.* Rejected: Vixen
  needs `deno_core`, not the Deno product surface. Node/npm semantics are not
  part of the browser runtime.
- *Copy Firefox WebIDL binding generation immediately.* Deferred: Firefox's
  binding stack is authoritative for many DOM semantics, but `deno_core` is the
  better Rust embedding/runtime substrate for Vixen.

**Consequences.**

- `deno_core` is the `vixen-engine::script` dependency; `mozjs` is no longer in
  the active engine dependency graph.
- Internal host modules may depend on `deno_core` APIs directly. The stable seam
  is the Vixen product API (`JsRuntime`, `JsValue`, headless/CDP behavior), not a
  portable JS-engine adapter.
- Binary-size gates must be remeasured for V8. The old system/static mozjs split
  no longer applies.
- `docs/REFERENCES.md` pins Deno as the primary JS runtime/host packaging
  reference. Firefox remains a DOM/Web API semantic reference, but not the JS
  engine target.
- New JS host families should be reviewed for module size, bootstrap locality,
  explicit registration, and permission/resource boundaries.
- Existing Page string-smoke projections and bootstrap snapshot pilots should
  migrate into explicit `deno_core` op/resource extensions one family at a time,
  while still reusing the same pure Rust modules.

---

## ADR-015: Modern-Linux Firefox replacement, optimized for capability per byte

**Status:** accepted

**Supersedes:** ADR-007's narrow "GNOME-only" product wording. The
Relm4/libadwaita/Flatpak implementation path remains accepted.

**Context.** The project direction is now explicit: Vixen should become a
Firefox replacement for modern Linux users, with both a focused desktop browser
and first-class CLI/CDP automation. The important differentiator is not a large
feature buffet; it is high web capability with low binary size, low memory use,
fast builds, and rapid iteration driven by useful text reports.

**Decision.** Optimize Vixen for maximum browser capability per byte. The
desktop product targets modern Linux broadly while using the Relm4/libadwaita
GUI path and Flatpak/GNOME SDK build path. The shell should stay minimal and
focused, closer to Ghostty's product philosophy than a kitchen-sink browser UI.
Headless CLI, CDP, and Playwright-style workflows are product surfaces, not just
test harnesses.

Priority order is recorded in `docs/PROJECT_DIRECTION.md`: rendering/layout,
runtime DOM/Web APIs, network/security, storage/history, minimal shell,
headless/CDP, WPT/reporting, HTML integration, CLI ergonomics, then embeddable
Rust API.

**Alternatives considered.**

- *GNOME-only browser identity.* Narrowed: the implementation remains GTK/
  libadwaita, but the user target is modern Linux rather than GNOME Shell only.
- *Kitchen-sink browser chrome.* Rejected: UI breadth competes with engine
  correctness, binary size, and iteration speed before alpha.
- *Automation as secondary.* Rejected: CLI/CDP users and text reports are part of
  how Vixen will iterate quickly and be useful early.

**Consequences.**

- Architecture choices should cite size, memory, build-speed, or correctness
  impact when there is a meaningful tradeoff.
- Non-Linux platforms remain best-effort.
- UI additions must justify themselves against the focused-shell goal.
- `docs/COMPAT.md` and WPT/profile output are product artifacts because they let
  humans and agents measure progress.

---

## ADR-016: hk owns git lifecycle gates

**Status:** accepted

**Context.** The previous gate story mixed raw cargo commands, many `just gate-*`
recipes, manual pre-push habits, and ad-hoc agent summaries. Iteration speed is a
north-star concern, but work leaving the machine still needs consistent checks.
The project already uses mise, and hk is built by the same toolchain ecosystem
for fast git hook orchestration.

**Decision.** Add checked-in `hk.pkl` and make hk the git lifecycle enforcement
layer. `just` remains the project command library; hk decides when those recipes
run. Pre-commit stays quick and mostly local: formatting, merge-conflict/private
key scans, and staged diff whitespace. Long gates run only pre-push through one
recipe, `just gate-push`.

The standard pre-push gate is:

```sh
just gate-alpha
just gate-phase6
just gate-smoke
git diff --check
git diff --cached --check
```

**Alternatives considered.**

- *Keep manual gate discipline.* Rejected: too easy for long autonomous sessions
  to drift.
- *Run all long gates pre-commit.* Rejected: hurts iteration speed and produces
  small, slow commits.
- *Replace `just` with hk commands.* Rejected: `just` recipes are still useful
  as explicit project actions and documentation anchors.

**Consequences.**

- Agents may commit and push automatically when hk gates pass.
- Hook setup is part of normal mise/bootstrap workflow.
- If pre-push becomes too slow or misses an important area, change
  `just gate-push` first; keep hk pointing at that stable recipe.

---

## ADR-017: One engine-owned browser, profile, and context lifecycle

**Status:** accepted

**Supersedes:** ADR-010's one-`EngineWorker`-per-tab engine ownership. ADR-010's
Relm4 component/factory/worker guidance remains accepted for GUI presentation and
message transport.

**Context.** The first vertical slices made `Page`, `JsRuntime`, network policy,
profile tables, shell loading, headless commands, and CDP behavior executable.
They also revealed that sharing component types is not the same as sharing a
browser. There is no production `impl vixen_api::Engine`: the GTK shell owns a
separate navigation/history/network/cookie state machine and constructs `Page`
on the UI thread, while headless/CDP separately owns `Page`, one scripted
`JsRuntime`, history, target/session shape, network configuration, and automation
overrides.

Continuing to add APIs to those coordinators would make lifecycle semantics
frontend-specific. Profile sharing, independent tabs, active navigation
cancellation, stale-result rejection, downloads, frames, and renderer/runtime
recovery all need an owner above an individual `Page` or protocol session.

**Decision.** `vixen-engine` owns one `BrowserCore` per open profile. It runs on
an engine-owned thread/local executor suitable for the non-`Send` Rc DOM and
`deno_core::JsRuntime`, and owns:

- one profile service for store, cookies/cache, permissions, HSTS, downloads,
  clear-data policy, and host configuration;
- one registry of top-level browsing contexts and future child frames;
- context-scoped session history, sessionStorage, viewport/input state, active
  navigation, runtime realms, and committed document state; and
- document-scoped DOM, style/layout/paint invalidation, script resources, and
  inspector state.

Commands and events cross a browser-scoped `vixen-api` seam and carry typed
context/navigation/document/request/runtime/download ids. Asynchronous work also
carries its creation generation. Cancelling or superseding work invalidates that
generation; late results are rejected before state mutation, cache/profile side
effects, or success events.

The shell, headless CLI, CDP, and WPT harness become adapters over this core. They
may own widgets, GL/EGL surfaces, sockets, protocol session routing, and
presentation snapshots, but not alternate navigation, history, page-runtime,
permission, cookie/cache, or profile state. Composition roots may construct
`vixen-engine`; adapters do not directly combine its leaf subsystems.

The existing `Engine` trait may evolve or be replaced by browser-scoped command,
event, query, and factory contracts. Vixen still has one concrete engine; this is
not an engine-plug-in abstraction.

**Alternatives considered.**

- *Keep one independent engine worker per tab and share only a `Store`.* Rejected:
  cookies/cache/permissions/downloads and host configuration require coordinated
  in-memory state, while CDP target routing and browser-wide clear-data operations
  still need a higher owner.
- *Keep shell and headless coordinators but extract more common helpers.*
  Rejected: helpers can share algorithms but cannot define atomic commit,
  cancellation, event ordering, or teardown across independently owned state.
- *Move the Rc DOM and V8 runtime to the GTK thread.* Rejected: it couples the
  engine to GUI scheduling and gives headless/CDP a different execution model.
- *Make every subsystem `Send + Sync` and distribute it across a worker pool.*
  Rejected for alpha: it adds locking and re-entrancy complexity without a proven
  need. External transport/blocking work can return generational messages to one
  deterministic engine executor.

**Consequences.**

- The next alpha work is lifecycle migration before broad API growth.
- Shell/headless direct `vixen-net`/`vixen-store` and direct orchestration are
  documented temporary exceptions. `just gate-architecture` protects stable leaf
  boundaries now and should ban each exception once migrated.
- Two tabs/targets can own independent documents/runtimes while sharing intended
  profile state through one service.
- `stop`, redirects, history, form navigation, page-driven navigation, session
  restore, downloads, and error pages gain one event/commit model.
- The browser-core executor becomes a reliability boundary. Long script/layout
  work needs budgets and cooperative scheduling; stronger process isolation is a
  later explicit architecture generation, not an accidental worker pool.
- Existing Page/network/store/runtime tests remain useful but need browser-core
  integration tests for ownership, partitioning, event ordering, cancellation,
  stale completions, and frontend parity.

**Implementation status (2026-07-10).** The A1 migration is complete: shell,
headless, CDP, and WPT route contexts through BrowserCore, and the architecture
gate forbids their former direct leaf composition. The first A2 slice runs source
loads on a bounded external Tokio runtime and returns generation-tagged results;
stop/supersede abort active transport and forced late completions are rejected
before cookie/profile/history/document/runtime mutation. Parser, page-script, and
discovered-resource jobs remain synchronous on the owner thread and require the
next cooperative-cancellation slice.
