# Vixen architecture

How Vixen is structured: crate layout, data flow, trust boundaries, and
the public trait APIs at each seam.

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
│   │       └── http_helpers.rs
│   ├── vixen-store/                     # persistence (redb)
│   │   └── src/lib.rs
│   ├── vixen-engine/                      # engine integration glue + Vixen-owned layout
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── engine.rs                # Engine: load lifecycle, history
│   │       ├── doc.rs                   # Document impl of Stylo TNode/TElement
│   │       ├── style.rs                 # Stylo cascade driver
│   │       ├── script.rs                # JS runtime seam backed by deno_core/V8
│   │       ├── script/                  # host modules: ops/resources/bootstrap JS
│   │       ├── layout.rs                # Vixen-owned Rust layout entry point
│   │       ├── layout_tree.rs           # styled DOM → arena-backed layout tree
│   │       ├── formatting_context.rs    # block/inline/flex/grid layout algorithms
│   │       ├── paint.rs                 # display list → WebRender (single paint path; consumes the GlContext trait, two impls)
│   │       ├── snapshot.rs              # PageSnapshot, ElementInfo
│   │       ├── inspector.rs             # inspect_element_at, computed-style export
│   │       ├── diagnostics.rs           # EngineDiagnostic shape
│   │       ├── engine_error.rs          # typed errors + stable codes
│   │       └── cleanup.rs               # navigation cleanup hooks
│   ├── vixen-shell/                     # Relm4/libadwaita browser chrome (idiomatic Relm4)
│   │   └── src/
│   │       ├── lib.rs                   # run()
│   │       ├── app.rs                   # top-level App component, root message enum
│   │       ├── browser_window.rs        # window component (header bar, tab view, find bar slot)
│   │       ├── tabs.rs                  # FactoryVecDeque<TabModel> — dynamic tab list
│   │       ├── tab.rs                   # Tab component: owns EngineWorker, address bar, status row
│   │       ├── location_entry.rs        # address/search component
│   │       ├── find_bar.rs              # find-in-page component
│   │       ├── engine_factory.rs        # creates EngineWorker + wraps gtk4::GLArea as GlAreaSurface (GlContext)
│   │       ├── engine_worker.rs         # Relm4 Worker: owns Engine, posts EngineDelegate msgs
│   │       ├── settings.rs              # GSettings wrapper
│   │       ├── profile.rs               # app-ID scoped paths
│   │       ├── config.rs                # APP_ID, VERSION
│   │       └── modals/                  # about, preferences, shortcuts (relm4-components where possible)
│   ├── vixen-wpt/                       # WPT harness
│   │   └── src/
│   │       ├── lib.rs                   # manifest, runner, WPT-style check types
│   │       └── visual_hash.rs
│   └── vixen-headless/                  # CLI
│       └── src/
│           ├── lib.rs                   # HeadlessPage
│           ├── main.rs                  # clap + arg dispatch
│           ├── load.rs
│           ├── inspect.rs
│           ├── interact.rs
│           ├── surface.rs               # SurfacelessSurface: EGL surfaceless GlContext impl
│           ├── types.rs
│           └── cdp.rs                   # CDP WebSocket server
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
 EngineWorker/tab)                        EngineInspector  dev-dep: vixen-wpt)
                          │
                          ▼
                       vixen-net   (leaf — HTTP, cookies, CSP,
                                    URL policy, permissions;
                                    no vixen-crate deps)

vixen-store — standalone leaf (redb persistence; not yet wired into the
              build, no vixen-crate dependencies today)
```

The only edge into `vixen-net` is `vixen-engine → vixen-net`: the script host
hooks delegate `fetch`/`document.cookie` into `vixen-net`, and URL policy /
CSP are re-applied at every fetch from `script.rs`. The four engine
consumers — `vixen-shell`, `vixen-engine`, `vixen-wpt`, `vixen-headless` — all
depend on `vixen-api`; `vixen-net` and `vixen-store` are leaf crates with no
dependencies on other vixen crates.

**Boundary rules** (enforced by `cargo tree` audit in CI):

| Crate                | Owns                                          | Must not own                          |
|----------------------|-----------------------------------------------|---------------------------------------|
| `vixen-api`          | `Engine` trait, DTOs, diagnostics shape      | Any concrete engine dep                |
| `vixen-net`          | HTTP, cookies, CSP, URL policy, permissions   | GTK, JS engine, DOM, layout            |
| `vixen-store`        | redb-backed persistence                       | Anything networked                     |
| `vixen-engine`         | Stylo + `deno_core` JS runtime + layout + paint glue | GTK, EGL, CLI arg parsing (GL comes in via the `GlContext` trait) |
| `vixen-shell`        | Browser chrome, Relm4/libadwaita, `GlAreaSurface` (`GlContext` impl) | Engine internals (only via `Engine`) |
| `vixen-headless`     | CLI args, screenshot/dump/CDP entry points, `SurfacelessSurface` (`GlContext` impl) | GTK, libadwaita                        |
| `vixen-wpt`          | WPT manifest + runner + check types           | Engine internals                       |

---

## Data flow per navigation

```
URL
 │
 ▼
vixen-net::Network::get_text_with_cookies       (reqwest + rustls + URL/CSP policy)
 │  → TextResponse { body, headers, cookies, redirects }
 ▼
vixen-engine::page::Page                         (URL + pipeline state facade)
 │
 ▼
html5ever::parse_document                        (HTML5 parser)
 │  → RcDom
 ▼
vixen-engine::doc::Document::from_dom              (Stylo-compatible DOM)
 │  → impls style::dom::TNode / TElement
 ▼
vixen-engine::style::cascade                       (Stylo traversal)
 │  → per-element ComputedValues
 ▼
vixen-engine::layout::layout                       (Vixen-owned Rust layout; ADR-013)
 │  → positioned box tree
 ▼
vixen-engine::paint::build_display_list            (single display list)
 │  → webrender::DisplayListBuilder
 ▼
vixen-engine::paint::Renderer                      (single WebRender paint path)
 │  → GL framebuffer, via &dyn GlContext
 ▼
GUI:  GlAreaSurface (wraps gtk4::GLArea)   OR   Headless: SurfacelessSurface (EGL surfaceless) → glReadPixels → PNG
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

---

## Trust boundaries

Web content is untrusted. Validation lives at:

| Boundary                              | Code                                       | Rule                                                                       |
|---------------------------------------|--------------------------------------------|----------------------------------------------------------------------------|
| Network fetch entry                   | `vixen-net::url_policy::validate_http_url` | SSRF / private-IP / reserved-TLD block (see `SPEC.md` "URL policy")        |
| HTTP response → cookie jar            | `vixen-net::cookie::CookieJar::set_cookie` | RFC 6265 rules (see `SPEC.md` "Cookie contract")                           |
| HTTP response → CSP                   | `vixen-net::csp::Enforcer::from_headers`   | Parse + store CSP for the document                                         |
| Script execution                      | `vixen-engine::script::evaluate`             | CSP gating before `EvaluateScript`; compartment per origin                 |
| JS → document.cookie                  | `vixen-net::cookie::set_document_cookie`   | HttpOnly rejected; domain/secure/samesite rules enforced                   |
| JS → fetch / XHR                      | `vixen-engine::script` → `vixen-net`         | CSP `connect-src` enforcement; URL policy re-applied                       |
| JS → localStorage/sessionStorage      | `vixen-store`                              | Per-origin partitioning; size limits enforced                              |
| Persistence                           | `vixen-store`                              | Per-origin partitioning; never persist untrusted script output             |

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
cookies.redb           # CookieEntry table
fetch-cache.redb       # GET response cache
history.redb           # visited URLs + timestamps
session.redb           # open tabs at last quit (if session restore enabled)
localstorage/<origin>/ # per-origin localStorage
sessionstorage/<origin>/ # per-origin sessionStorage (cleared on exit)
```

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
