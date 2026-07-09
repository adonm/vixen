# Vixen architecture

How Vixen is structured: crate layout, data flow, trust boundaries, and
the public trait APIs at each seam.

Product direction lives in [`PROJECT_DIRECTION.md`](PROJECT_DIRECTION.md): a
modern-Linux Firefox replacement with a minimal Relm4/libadwaita desktop shell,
headless/CDP automation, and maximum capability for the smallest credible
binary. Architecture choices below should be read through that lens.

---

## Crate layout

```
vixen/                                  # workspace root
├── Cargo.toml                           # workspace + binary crates
├── crates/
│   ├── vixen-api/                       # public engine trait + DTOs (no impl deps)
│   │   └── src/lib.rs
│   ├── vixen-net/                       # networking + security policy
│   │   └── src/
│   │       ├── lib.rs                   # Network (reqwest + rustls)
│   │       ├── cookie.rs                # RFC 6265 cookie jar
│   │       ├── url_policy.rs            # SSRF / private-IP blocking
│   │       ├── csp.rs                   # CSP parser + enforcer
│   │       ├── permissions.rs
│   │       ├── origin.rs
│   │       ├── fetch_types.rs
│   │       ├── cors.rs / referrer_policy.rs / mixed_content.rs
│   │       └── http_helpers.rs
│   ├── vixen-store/                     # single-file redb persistence
│   │   └── src/lib.rs                   # bounded profile tables + clear-data
│   ├── vixen-engine/                    # engine integration glue + Vixen-owned layout
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── page.rs                  # Page facade: URL, parsed DOM, style/layout/paint state
│   │       ├── doc.rs                   # Document impl of Stylo TNode/TElement
│   │       ├── style_dom.rs / style_cascade.rs
│   │       ├── script.rs                # JsRuntime seam backed by deno_core/V8
│   │       ├── script/                  # host modules: webidl, webapi, dom, cssom, encoding
│   │       ├── layout_tree.rs           # styled DOM → arena-backed layout tree
│   │       ├── line_layout.rs and focused layout/value modules
│   │       ├── display_list.rs / paint.rs # single display-list → WebRender path
│   │       ├── forms.rs / form_submission.rs / history.rs
│   │       └── engine_error.rs          # typed errors + stable codes
│   ├── vixen-shell/                     # Relm4/libadwaita browser chrome
│   │   └── src/
│   │       ├── lib.rs                   # run(), app IDs
│   │       ├── app.rs                   # top-level App component
│   │       ├── tab.rs                   # Tab component and browser controls
│   │       ├── engine_worker.rs         # background worker owns engine/network load
│   │       └── surface.rs               # gtk4::GLArea-backed GlContext
│   ├── vixen-wpt/                       # WPT/fixture harness
│   │   └── src/                         # manifest, harness, checks, profiles, visual hashes
│   └── vixen-headless/                  # CLI + CDP
│       └── src/
│           ├── lib.rs / main.rs
│           ├── cdp.rs                   # CDP WebSocket server
│           ├── interactions.rs
│           └── surface.rs               # EGL surfaceless GlContext impl
├── src/
│   └── main.rs                          # tiny: calls vixen_shell::run()
├── data/                                # GNOME app data (icons, desktop, gschema, metainfo)
├── build-aux/                           # Flatpak manifests
├── fixtures/                            # WPT fixtures + manifests (acceptance suite)
├── benches/                             # criterion benchmarks (parse, style, layout, render)
├── docs/                                # this directory
└── justfile
```

---

## Dependency graph (allowed direction)

```
                      vixen-api
   traits + DTOs only — Engine, EngineDelegate,
   EngineInspector, EngineProfile, GlContext
   (no concrete deps)
                          ▲
   ┌──────────────────────┼────────────────┬──────────────────┐
   │                      │                │                  │
vixen-shell           vixen-engine        vixen-wpt       vixen-headless
(GTK4 + Relm4; owns   (Stylo, deno_core, (manifest +     (CLI + CDP; EGL
 gtk4::GLArea as       WebRender,         runner;          surfaceless
 GlAreaSurface;        html5ever, layout) consumes         SurfacelessSurface;
 EngineWorker/tab,                        EngineInspector  dev-dep: vixen-wpt)
 profile paths/session)    │
        │        ┌─────────┴───────────┐
        │        ▼                     ▼
        └──► vixen-store           vixen-net
     (leaf — redb profile     (leaf — HTTP, cookies,
      persistence: cookies,    CSP, URL policy,
      fetch cache, history,    permissions; no
      session, Web Storage)    vixen-crate deps)
```

The only edge into `vixen-net` is from `vixen-engine`. `vixen-store` remains a
leaf crate, but both `vixen-engine` and the GTK-free `vixen-shell::profile` layer
may open it: runtime host hooks persist normalized cookies/cache/storage/history,
while shell startup/shutdown persists bounded tab session records. URL policy and
option validation are re-applied at the JS → Rust boundary. The four engine
consumers — `vixen-shell`, `vixen-engine`, `vixen-wpt`, `vixen-headless` — all
depend on `vixen-api`; `vixen-net` and `vixen-store` have no dependencies on
other vixen crates.

**Boundary rules** (enforced by `cargo tree` audit in CI):

| Crate                | Owns                                          | Must not own                          |
|----------------------|-----------------------------------------------|---------------------------------------|
| `vixen-api`          | `Engine` trait, DTOs, diagnostics shape      | Any concrete engine dep                |
| `vixen-net`          | HTTP, cookies, CSP, URL policy, permissions   | GTK, JS engine, DOM, layout            |
| `vixen-store`        | redb-backed persistence                       | Anything networked                     |
| `vixen-engine`         | Stylo + `deno_core` JS runtime + layout + paint glue | GTK, EGL, CLI arg parsing (GL comes in via the `GlContext` trait) |
| `vixen-shell`        | Browser chrome, Relm4/libadwaita, `GlAreaSurface` (`GlContext` impl), GTK-free profile/session service | Engine internals (only via `Engine`) |
| `vixen-headless`     | CLI args, screenshot/dump/CDP entry points, `SurfacelessSurface` (`GlContext` impl) | GTK, libadwaita                        |
| `vixen-wpt`          | WPT manifest + runner + check types           | Engine internals                       |

---

## Data flow per navigation

```
URL / navigation request
 │
 ▼
vixen-engine lifecycle boundary
 │  validate navigation intent, assign origin/profile partition, emit diagnostics
 │
 ├──► vixen-store::Store
 │    load persisted cookies/storage/cache/session data for the partition
 │
 ▼
vixen-net::Network::{get_text_with_cookies*, RedirectMode}
 │  reqwest + rustls; URL policy; manual redirects; request headers/bodies; cookie jar per hop
 │  → TextResponse { body, headers, set_cookie, redirects, final_url, events }
 │  script fetch applies mode/CORS preflight + visibility before exposing data to JS
 │
 ├──► vixen-store::Store
 │    persist normalized cookies, GET cache entries, history/session state
 │
 ▼
vixen-engine::page::Page                         (authoritative page state facade)
 │
 ▼
html5ever::parse_document                        (HTML5 parser)
 │  → RcDom
 ▼
vixen-engine::doc::Document::from_dom            (Stylo-compatible DOM)
 │  → impls style::dom::TNode / TElement
 ▼
vixen-engine::style_cascade                      (Stylo traversal)
 │  → per-element ComputedValues
 ▼
Vixen-owned layout modules                       (ADR-013)
 │  → positioned box tree / layout tree
 ▼
vixen-engine::display_list / paint               (single display list)
 │  → webrender::DisplayListBuilder
 ▼
vixen-engine::paint::Renderer                    (single WebRender paint path)
 │  → GL framebuffer, via &dyn GlContext
 ▼
GUI: GlAreaSurface (gtk4::GLArea) OR Headless: SurfacelessSurface (EGL surfaceless) → glReadPixels → PNG
```

One paint path. Two `GlContext` implementations: `GlAreaSurface`
(wrapping `gtk4::GLArea`) in the GUI window, `SurfacelessSurface`
(wrapping an EGL surfaceless context) in headless mode. `vixen-engine`
consumes the `GlContext` trait and never sees GTK or EGL types. Headless
never needs a display server; EGL_MESA_platform_surfaceless (or
EGL_KHR_surfaceless with a pbuffer fallback) provides a GPU context
without one. A headless Wayland compositor (`weston` / `cage` on a
virtual output) is an opt-in fallback when full compositor semantics
(pointer focus, XDG toplevel) are needed for CDP interaction tests.

Layout is Vixen-owned Rust code per ADR-013. The browser-engine seam mirrors
Ladybird's architecture at `0de15a5dd2a9`: a single tree builder converts the
styled DOM into layout nodes, then block/inline/flex/grid formatting contexts
produce geometry. Internally the Vixen implementation stays data-oriented
(stable node ids, arenas, explicit invalidation bits) rather than exposing a
pointer-heavy object graph across crates.

JS internals use `deno_core` per ADR-014. Do not add an internal JS-engine
abstraction: host modules should use `deno_core` concepts directly (extensions,
ops, resources, module loaders).
The stable boundary is Vixen's product seam (`JsRuntime`, `JsValue`, headless
`--eval`, CDP `Runtime.evaluate`), not portability between JS engines. Host APIs
should be packaged as Deno-style feature modules with explicit extension/op
registration, local JS bootstrap, pure Rust operation/data surfaces, resource
handles for long-lived host state, and permission checks near the host boundary.
Firefox remains a DOM/Web API semantic reference; Deno/`deno_core` is the JS
runtime substrate.
Generated WebIDL scaffolding lives in `script::webidl`; host-family bootstraps
adopt those generated interfaces. Pure value APIs may stay JS-only when that is
smaller and state-free; page/network/storage/security-backed APIs cross a Rust
op/resource boundary. See [`RUNTIME_WEB_PLATFORM.md`](RUNTIME_WEB_PLATFORM.md).

The main design lesson from the runtime-host, storage, fetch/XHR, and CDP/WPT
migrations is to avoid parallel browser models. A feature is not considered
browser-shaped until the same path serves all relevant seams: `Page` state,
headless `--eval`, CDP, WPT fixtures, and the GUI where visible.
`Page::evaluate_dom_expression` is now a fail-closed compatibility shim, not a
place to add behavior. New browser APIs must land as one of:

- JS-only value APIs in a bootstrap, when they are pure and state-free;
- `deno_core` ops/resources that validate at the JS → Rust boundary;
- authoritative `Page`/DOM/layout state reachable by runtime, headless, CDP, and
  GUI; or
- leaf-crate policy/storage/network primitives called by the engine.

Likewise, automation must not grow an automation-only DOM, profile state must not
stay in runtime-only maps when a backing store exists, networking must not grow a
test-only client path, and layout/paint must not grow post-pass correction layers
that hide bad authoritative state.

Implementation pattern to prefer after the recent fetch/CORS/cache/XHR work:

1. Put the pure policy/data type in a leaf crate (`vixen-net`/`vixen-store`) when
   it can be tested without JS or GTK.
2. Add a narrow engine host op/resource that validates untrusted inputs near the
   JS → Rust boundary and fails closed.
3. Surface the behavior through the browser-visible seam (`Page`, fetch/XHR,
   headless, CDP, or GUI), not through a test-only shortcut.
4. Add event/diagnostic DTOs once automation or the shell needs observability;
   keep them stable and small.
5. Persist bounded profile state as soon as user-visible state exists, and wire it
   into clear-data flows.

Peer-browser issue trackers reinforce three additional design constraints:

- **Host integration is an engine seam.** TLS roots, sandbox file allowlists,
  fontconfig/font fallback, XDG directories, Flatpak portals, downloads, and
  GL/EGL availability must be represented as explicit platform/profile services
  with diagnostics. Do not bury them in shell-only code or unstructured logs.
- **Reductions are part of the architecture.** Real-site failures should reduce
  into local fixtures or WPT imports that exercise the same pipeline. A screenshot
  without a reduced fixture is useful triage, not a regression guard.
- **Inspection is not read-only in practice.** CDP/devtools snapshots, highlight
  overlays, geometry queries, and mutation notifications can force style/layout
  work at awkward times. Inspector surfaces must tolerate stale layout by
  updating through explicit invalidation gates or returning stable errors; they
  must never assume layout is current just because the page is inspectable.

These rules deliberately keep platform, profile, inspector, and reduction
workflows inside the browser design instead of treating them as late product
polish.

---

## Trust boundaries

Web content is untrusted. Validation lives at:

| Boundary                              | Code                                       | Rule                                                                       |
|---------------------------------------|--------------------------------------------|----------------------------------------------------------------------------|
| Network/navigation fetch entry         | `vixen-net::url_policy::validate_http_url` | SSRF / private-IP / reserved-TLD block (see `SPEC.md` "URL policy")        |
| HTTP redirect hop                      | `vixen-net::Network` + `RedirectMode`       | URL policy re-applied; `follow` / `manual` / `error` honored fail-closed    |
| HTTP response → cookie jar             | `vixen-net::cookie::CookieJar::set_cookie` | RFC 6265 rules (see `SPEC.md` "Cookie contract")                           |
| HTTP response → CSP                    | `vixen-net::csp::Enforcer::from_headers`   | Parse + store CSP for the document                                         |
| Script execution                       | `vixen-engine::script::evaluate`            | CSP gating before `EvaluateScript`; compartment per origin                 |
| JS → document.cookie                   | `script::dom`/`webapi` → `vixen-net`        | HttpOnly rejected; domain/secure/samesite rules enforced                   |
| JS → fetch / XHR                       | `script::webapi` → `vixen-net`              | Validate method/mode/cache/credentials/redirect/body/headers; re-apply URL policy, CSP, CORS, mixed-content, referrer policy |
| JS → localStorage/sessionStorage       | `script::webapi` → `vixen-store`            | Per-origin partitioning; key/value size limits enforced                    |
| Runtime/profile persistence            | `vixen-store`                               | Persist only bounded, partitioned, normalized records                      |
| Page mutation/navigation invalidation  | `Page` + engine lifecycle                   | One authoritative page state; update layout/paint/history from same commit |
| Host platform integration              | Shell/profile/platform services             | Certs, fonts, XDG dirs, portals, GL/EGL fail with actionable diagnostics   |
| Inspector/CDP snapshotting              | `EngineInspector` / CDP                     | Never crash on stale layout; update explicitly or return stable errors     |

Every boundary **fails closed**. Unsupported operations return typed
diagnostics with stable codes, never silent fallbacks.

---

## Public trait APIs

### `Engine` (in `vixen-api`)

The shell-facing engine interface. Each tab owns a Relm4 `EngineWorker`
that owns the `Box<dyn Engine>` on a background thread (per ADR-010);
the tab component talks to the worker, never to the engine directly.

```rust
pub trait Engine {
    // Navigation
    fn load_uri(&mut self, uri: &str);
    fn reload(&mut self);
    fn stop(&mut self);
    fn go_back(&mut self);
    fn go_forward(&mut self);
    fn can_go_back(&self) -> bool;
    fn can_go_forward(&self) -> bool;

    // State
    fn current_uri(&self) -> Option<String>;
    fn current_title(&self) -> Option<String>;
    fn is_loading(&self) -> bool;
    fn estimated_load_progress(&self) -> f64;

    // Find + zoom
    fn find_text(&mut self, q: &str, case_sensitive: bool, forward: bool) -> u32;
    fn clear_find(&mut self);
    fn zoom_level(&self) -> f64;
    fn set_zoom_level(&mut self, z: f64);

    // Script
    fn execute_javascript(&mut self, src: &str);

    // Callbacks — single delegate replaces N Box<dyn Fn>
    fn set_delegate(&mut self, delegate: Box<dyn EngineDelegate>);

    // Snapshot/inspection — optional so headless/inspector can opt in
    fn inspector(&self) -> Option<&dyn EngineInspector>;

    // Diagnostics
    fn diagnostics(&self) -> Vec<EngineDiagnostic>;
}
```

### `EngineDelegate` (in `vixen-api`)

The shell implements this to receive engine callbacks. In practice the
shell-side implementation posts these into the Relm4 message stream for
the relevant tab component; the trait itself stays GUI-agnostic so
`vixen-engine` does not depend on `relm4`.

```rust
pub trait EngineDelegate: Send {
    fn uri_changed(&mut self, uri: &str);
    fn title_changed(&mut self, title: &str);
    fn load_progress(&mut self, progress: f64);
    fn load_finished(&mut self);
    fn load_failed(&mut self, message: &str);
    fn download_event(&mut self, event: DownloadEvent);
    fn permission_requested(&mut self, event: PermissionEvent);
    fn context_menu(&mut self, context: &str);
}
```

The shell's `EngineWorker` (Relm4 `Worker`) owns the engine on a
background thread; the delegate's methods become
`Worker::input_to_main` messages consumed by the tab component.

### `EngineInspector` (in `vixen-api`)

Optional inspection surface. The shell's right-click inspector uses this;
headless CDP uses this; WPT harness uses this.

```rust
pub trait EngineInspector {
    fn inspect_element_at(&self, x: f64, y: f64) -> Option<ElementInfo>;
    fn capture_snapshot(&self, vw: u32, vh: u32) -> PageSnapshot;
    fn computed_style_for_element(&self, node_id: usize) -> Vec<(String, String)>;
}
```

### `EngineProfile` (in `vixen-api`)

Configuration for instantiating an engine.

```rust
pub struct EngineProfile {
    pub start_url: String,
    pub restore_session: bool,
    pub zoom: f64,
    pub data_dir: Option<PathBuf>,
    pub user_agent: Option<String>,
    pub enable_javascript: bool,
    pub default_font_size: u32,
    pub hardware_acceleration: HardwareAccelerationMode,
}
```

### `GlContext` (in `vixen-api`)

Minimal graphics-context abstraction so `vixen-engine` can drive WebRender
without taking a GTK or EGL dependency. Following the GTK4/Relm4 idiom,
the shell owns the widget and connects to `gtk4::GLArea::render`; inside
that callback GTK has already made the `gdk::GLContext` current. The
shell's `GlAreaSurface` and the headless binary's `SurfacelessSurface`
are the only two implementations.

```rust
pub trait GlContext {
    /// Ensure this context is current on the calling thread. On the GUI
    /// path this is a no-op when called from inside `GLArea::render`.
    fn make_current(&self);
    /// GL function-pointer lookup; feeds WebRender's `gleam` loader.
    fn proc_address(&self, name: &str) -> *const std::ffi::c_void;
    /// Drawable size in physical pixels.
    fn drawable_size(&self) -> (u32, u32);
}
```

`vixen-engine::paint` builds its single WebRender `Renderer` against a
`&dyn GlContext`; per ADR-006 there is one paint path and no `PaintBackend`
trait — the `GlContext` implementations are the only thing that varies
between GUI and headless.

---

## App ID and profile paths

- Production: `org.vixen.Vixen` → `~/.local/share/org.vixen.Vixen/`
- Devel:     `org.vixen.Vixen.Devel` → `~/.local/share/org.vixen.Vixen.Devel/`

Inside the profile dir:

```
profile.redb             # one redb file opened by vixen-store::Store
  cookies                # partitioned CookieRecord entries
  fetch-cache            # bounded, partitioned GET response cache entries
  history                # visited URLs + timestamps
  session                # bounded SessionRecord tabs, active tab, scroll/focus/form restore hints
  web-storage            # localStorage/sessionStorage entries by storage partition
  downloads              # bounded profile-wide DownloadRecord history
  permissions            # per-origin permission decisions
  hsts                   # persisted HSTS/security-state records
downloads/               # target directory for accepted downloads
reports/                 # optional diagnostics/smoke artifacts
```

The exact redb filename is chosen by the caller today; the architectural shape is
one `Store` per profile with separate tables per concern. `vixen-store` never
depends on URL/origin types: callers pass opaque partition keys such as
`vixen_net::Origin::partition_key()`.

Profile state should also grow bounded product tables as features land:
favicons/icons (deduplicated blobs, not repeated base64 strings), settings, and
clear-data tombstones. Each table needs a clear owner, size bounds, and a
`vixen-store::ClearDataSelection` policy before it becomes part of the shell UI.

---

## Linux host-integration services

Modern Linux compatibility is not just GTK. The shell/profile layer should expose
small services to the engine instead of letting subsystems discover the host in
ad-hoc ways:

- certificate roots and optional custom CA bundle path;
- font discovery/fallback configuration and warning suppression policy;
- XDG user directories, especially downloads;
- Flatpak portal handles for files, downloads, permissions, and external opens;
- GL/EGL capability diagnostics for GUI and headless paths;
- profile/cache/download directories scoped by app ID.

`vixen-shell::profile` is the current GTK-free owner for app-ID scoped XDG data
paths, the profile redb location, profile download/report directories, and the
host XDG Downloads directory. It loads and saves the shell's bounded tab session
record through `vixen-store`, clamps restore indices at the profile boundary, and
falls back to the configured start page for empty profiles. It also routes
explicit `ClearDataSelection` requests through the same app-ID scoped store,
validates download destinations and “show in folder” targets against the known
user/profile downloads roots, and stays available in default builds so headless
and future profile services can reuse the same path policy without pulling GTK.

Each service should produce structured diagnostics consumable by GUI error pages,
headless text output, CDP, and WPT/fixture reports. “Works on my distro” is not a
release criterion; Fedora/Arch/openSUSE/Debian-like cert/font/XDG layouts need
controlled smoke coverage before beta.

---

## Reduction and real-site triage workflow

When a real site fails, classify it first, then reduce it:

1. network/security/platform (TLS, CSP, mixed content, sandbox, proxy, anti-bot),
2. DOM/Web API/WebIDL/events/forms,
3. layout/style/paint/compositor/text shaping,
4. storage/profile/downloads/session,
5. shell/chrome/platform UI,
6. performance/reliability/crash.

The preferred end state is a small local fixture or imported WPT profile plus a
`COMPAT.md` note. If the bug cannot be reduced yet, keep the real-site command,
screenshots, logs, and classification so it can drive later work without becoming
a vague compatibility claim.

---

## Build profile

Already optimal. Carry forward unchanged:

```toml
[profile.release]
strip = true
lto = "thin"
codegen-units = 1
panic = "abort"
```

`lto = "fat"` may be tried as a one-off to measure size delta; revert if
compile time is unacceptable.
