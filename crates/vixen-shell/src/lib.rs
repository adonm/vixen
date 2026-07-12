//! vixen-shell — Relm4/libadwaita browser chrome (ADR-010).
//!
//! ## GTK shell status
//!
//! The first Relm4/libadwaita desktop vertical lands behind the `gtk-shell`
//! feature: one window, one URL entry, one visible page, navigation controls,
//! status diagnostics, and WebRender output in a `gtk4::GLArea`. On hosts
//! without GNOME SDK dev packages the default workspace still type-checks:
//! `run()` reports the missing shell and returns, so the `vixen` GUI binary
//! compiles everywhere (docs/PLAN.md Phase 0 gate: `cargo check --workspace`).
//!
//! Build the real shell with `just flatpak-build` (supported) or native
//! `cargo build -p vixen --features vixen-shell/gtk-shell` after installing
//! GTK/libadwaita development packages.
//!
//! ## Planned module layout (docs/ARCHITECTURE.md "Crate layout")
//!
//! ```text
//! app.rs             — top-level App component, root message enum
//! browser_window.rs  — window component (header bar, tab view, find bar slot)
//! tabs.rs            — FactoryVecDeque<TabModel> — dynamic tab list
//! tab.rs             — Tab component: presentation state and paint snapshots
//! location_entry.rs  — address/search component
//! find_bar.rs        — find-in-page component
//! engine_factory.rs  — creates EngineWorker + wraps gtk4::GLArea as GlAreaSurface
//! engine_worker.rs   — app-level Relm4 Worker: owns one shared BrowserCore
//! settings.rs        — GSettings wrapper
//! profile.rs         — app-ID scoped paths and host download services
//! config.rs          — APP_ID, VERSION
//! modals/            — about, preferences, shortcuts
//! ```

#![deny(unsafe_code)]

/// App ID constants (docs/ARCHITECTURE.md "App ID and profile paths").
pub mod config {
    /// Production app ID.
    pub const APP_ID: &str = "dev.adonm.vixen";
    /// Development app ID.
    pub const APP_ID_DEVEL: &str = "dev.adonm.vixen.Devel";
    /// Vixen version string (kept in sync with the workspace `Cargo.toml`).
    pub const VERSION: &str = env!("CARGO_PKG_VERSION");
}

/// App-ID scoped profile paths and host download services.
pub mod profile;

/// GTK-independent address-bar normalization.
pub mod address;

/// GTK-independent multi-context adapter over the production BrowserCore.
#[cfg(feature = "browser-core")]
pub mod browser_adapter;

#[cfg(feature = "gtk-shell")]
mod app;

#[cfg(feature = "gtk-shell")]
mod engine_worker;

#[cfg(feature = "gtk-shell")]
mod tab;

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
    app::run();
}
