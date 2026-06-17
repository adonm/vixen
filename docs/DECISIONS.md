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
| JS engine        | `mozjs` (SpiderMonkey Rust bindings) |
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
- Binary size grows by the volume of these crates. ADR-005 controls this
  via system-linking mozjs where possible.

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
(spawned binaries communicating over IPC) for isolation. SpiderMonkey
already provides compartment-based isolation in-process, and
out-of-process isolation (proper OOPIF) is a separate, much larger
effort.

**Decision.** Single-process engine. JS isolation is via SpiderMonkey
compartments (one per origin). No `JsSandbox`, no `JsSandboxPool`, no
`process_pool`, no `ipc` module.

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

## ADR-005: System mozjs by default, static fallback for distribution

**Status:** accepted

**Context.** SpiderMonkey is the largest single contributor to binary
size (~3–5 MiB stripped static). Most Linux distributions package
libmozjs (e.g. Fedora `mozjs102`, Debian `libmozjs-102-0`). GNOME
Flatpak manifests can vendor it as a shared module.

**Decision.**

- **Production Flatpak** (`org.vixen.Vixen`): link to a shared libmozjs
  module vendored into the Flatpak manifest. Binary ~10 MiB.
- **Headless CI / standalone distribution**: static mozjs. Binary ~14 MiB.
- **Development builds**: static mozjs (simpler, no system dependency). In
  practice the `mozjs`/`mozjs_sys` crate **downloads a prebuilt** static
  SpiderMonkey from `servo/mozjs` GitHub Releases by default (no from-source
  build); `MOZJS_ARCHIVE` pins/offline-mirrors it. See
  [`docs/guidance/mozjs.md`](guidance/mozjs.md).

**Alternatives considered.**

- *Static mozjs everywhere.* Rejected: +4 MiB on the production binary
  for no functional gain where the distro provides libmozjs.

**Consequences.**

- Production binary carries a runtime dependency on libmozjs. Flatpak
  handles this transparently; native packages declare it.
- SpiderMonkey version follows what the Flatpak manifest vendors. Major
  SM upgrades (every 6–12 months) require a manifest bump.

---

## ADR-006: One display list, one paint path, two GL surfaces

**Status:** accepted

**Context.** A previous design had four parallel paint implementations
(CPU compositor, CPU renderer, "GPU renderer" that was actually CPU,
text/glyph rasterizer), each duplicating the draw-command dispatch
logic. Even a "two backend" design (WebRender + a software fallback)
duplicates the dispatch contract.

**Decision.** Vixen has one `DisplayList` type defined in `vixen-core`
and exactly one paint path (WebRender). There is no `PaintBackend`
trait — a single-impl trait would be dead abstraction. WebRender
consumes a small `GlContext` trait (defined in `vixen-api`, so
`vixen-core` stays GTK- and EGL-free) with two implementations:

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
  vixen-core type — keeping GL details out of engine internals and GTK
  out of vixen-core.
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
- `reference-browsers/relm4/examples/` and
  `reference-browsers/relm4/relm4-components/` are the primary
  reference for any new shell widget.
