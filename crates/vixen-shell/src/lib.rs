//! vixen-shell — Relm4/libadwaita browser chrome (ADR-010).
//!
//! ## Phase 0 status
//!
//! The real Relm4 shell (`App`, `FactoryVecDeque<TabModel>`, per-tab
//! `EngineWorker`) lands behind the `gtk-shell` feature, which requires the
//! GNOME SDK (`gtk4`, `libadwaita-1`). On hosts without those dev packages
//! the workspace still type-checks: `run()` reports the missing shell and
//! returns, so the `vixen` GUI binary compiles everywhere
//! (docs/PLAN.md Phase 0 gate: `cargo check --workspace`).
//!
//! Build the real shell with `cargo build --features vixen-shell/gtk-shell`.
//!
//! ## Planned module layout (docs/ARCHITECTURE.md "Crate layout")
//!
//! ```text
//! app.rs             — top-level App component, root message enum
//! browser_window.rs  — window component (header bar, tab view, find bar slot)
//! tabs.rs            — FactoryVecDeque<TabModel> — dynamic tab list
//! tab.rs             — Tab component: owns EngineWorker, address bar, status row
//! location_entry.rs  — address/search component
//! find_bar.rs        — find-in-page component
//! engine_factory.rs  — creates EngineWorker + wraps gtk4::GLArea as GlAreaSurface
//! engine_worker.rs   — Relm4 Worker: owns Engine, posts EngineDelegate msgs
//! settings.rs        — GSettings wrapper
//! profile.rs         — app-ID scoped paths
//! config.rs          — APP_ID, VERSION
//! modals/            — about, preferences, shortcuts
//! ```

#![forbid(unsafe_code)]

/// App ID constants (docs/ARCHITECTURE.md "App ID and profile paths").
pub mod config {
    /// Production app ID.
    pub const APP_ID: &str = "org.vixen.Vixen";
    /// Development app ID.
    pub const APP_ID_DEVEL: &str = "org.vixen.Vixen.Devel";
    /// Vixen version string (kept in sync with the workspace `Cargo.toml`).
    pub const VERSION: &str = env!("CARGO_PKG_VERSION");
}

#[cfg(feature = "gtk-shell")]
pub mod surface;

/// GUI entry point. The thin `vixen` binary calls this.
///
/// Without the `gtk-shell` feature this is a documented no-op so the
/// workspace compiles on non-GNOME hosts; with the feature it launches the
/// libadwaita browser window.
pub fn run() {
    #[cfg(not(feature = "gtk-shell"))]
    {
        eprintln!(
            "vixen: GUI shell not compiled in. Rebuild with:\n  \
             cargo build --features vixen-shell/gtk-shell\n\
             (requires gtk4 + libadwaita-1 dev packages; see docs/PLAN.md Phase 0)"
        );
    }

    #[cfg(feature = "gtk-shell")]
    run_gtk();
}

#[cfg(feature = "gtk-shell")]
fn run_gtk() {
    // Real Relm4 bootstrap lands with the GTK shell (Phase 5+ smoke, per
    // docs/PLAN.md). The skeleton is kept here so the `gtk-shell` feature
    // compiles today; `relm4::main_adw_application()` launches the app.
    relm4::main_adw_application();
}
