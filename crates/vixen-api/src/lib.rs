//! Vixen public engine trait surface and DTOs.
//!
//! This crate defines the seams between the engine (`vixen-core`) and its
//! consumers (`vixen-shell` GUI, `vixen-headless` CLI, `vixen-wpt` harness).
//! It owns **no concrete engine dependencies** — only traits and data types
//! — so the trait shape compiles and tests run at zero build cost
//! (docs/ARCHITECTURE.md "Boundary rules", docs/PLAN.md Phase 0 gate).
//!
//! Trait signatures are the stable contract; consumers program against
//! `&dyn Engine` / `Box<dyn Engine>`, never concrete types.

#![forbid(unsafe_code)]

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Diagnostics (docs/SPEC.md "Diagnostics shape")
// ---------------------------------------------------------------------------

/// A single non-fatal engine diagnostic, surfaced in the shell status row
/// and consumed by the WPT `no-critical-diagnostics` check.
///
/// `code` is a **stable contract**: automation may match on it, so codes
/// must not be silently renamed (docs/SPEC.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineDiagnostic {
    pub category: EngineDiagnosticCategory,
    /// Stable dotted code, e.g. `"parse-dom.budget"`.
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineDiagnosticCategory {
    Network,
    ParseDom,
    ScriptRuntime,
    LayoutRender,
    StorageCache,
}

impl EngineDiagnostic {
    /// Convenience constructor.
    pub fn new(
        category: EngineDiagnosticCategory,
        code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            category,
            code,
            message: message.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Engine delegate callbacks (docs/ARCHITECTURE.md "EngineDelegate")
// ---------------------------------------------------------------------------

/// A single `Box<dyn EngineDelegate>` replaces N `Box<dyn Fn>` callbacks
/// (docs/ARCHITECTURE.md). The shell implements this; its implementation
/// posts each callback into the Relm4 message stream for the relevant tab
/// component (ADR-010). The trait stays GUI-agnostic so `vixen-core` does
/// not depend on `relm4`.
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

// ---------------------------------------------------------------------------
// Inspection surface (docs/ARCHITECTURE.md "EngineInspector")
// ---------------------------------------------------------------------------

/// Optional inspection surface used by the shell's right-click inspector,
/// the headless CDP server, and the WPT harness.
pub trait EngineInspector {
    /// Hit-test the rendered tree at viewport coordinates.
    fn inspect_element_at(&self, x: f64, y: f64) -> Option<ElementInfo>;
    /// Capture a coarse document snapshot for the given viewport.
    fn capture_snapshot(&self, vw: u32, vh: u32) -> PageSnapshot;
    /// Computed (resolved) style for a node id, as `(property, value)` pairs.
    fn computed_style_for_element(&self, node_id: usize) -> Vec<(String, String)>;
}

// ---------------------------------------------------------------------------
// Engine (docs/ARCHITECTURE.md "Engine")
// ---------------------------------------------------------------------------

/// The shell-facing engine interface. Each tab owns a Relm4 `EngineWorker`
/// that holds a `Box<dyn Engine>` on a background thread (ADR-010); the tab
/// component talks to the worker, never to the engine directly.
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

    // Callbacks — single delegate replaces N Box<dyn Fn>.
    fn set_delegate(&mut self, delegate: Box<dyn EngineDelegate>);

    // Snapshot/inspection — optional so headless/inspector can opt in.
    fn inspector(&self) -> Option<&dyn EngineInspector>;

    // Diagnostics
    fn diagnostics(&self) -> Vec<EngineDiagnostic>;
}

// ---------------------------------------------------------------------------
// Graphics context seam (docs/ARCHITECTURE.md "GlContext", ADR-006)
// ---------------------------------------------------------------------------

/// Minimal graphics-context abstraction so `vixen-core` can drive WebRender
/// without taking a GTK or EGL dependency. Exactly two implementations
/// exist: `GlAreaSurface` (wrapping `gtk4::GLArea`, in `vixen-shell`) and
/// `SurfacelessSurface` (wrapping an EGL surfaceless context, in
/// `vixen-headless`). Per ADR-006 there is one paint path and no
/// `PaintBackend` trait — this is the only seam that varies.
pub trait GlContext {
    /// Ensure this context is current on the calling thread. On the GUI
    /// path this is a no-op when called from inside `GLArea::render`, where
    /// GTK has already made the `gdk::GLContext` current.
    fn make_current(&self);
    /// GL function-pointer lookup; feeds WebRender's `gleam` loader.
    fn proc_address(&self, name: &str) -> *const std::ffi::c_void;
    /// Drawable size in physical pixels.
    fn drawable_size(&self) -> (u32, u32);
}

// ---------------------------------------------------------------------------
// Profile (docs/ARCHITECTURE.md "EngineProfile")
// ---------------------------------------------------------------------------

/// Configuration for instantiating an engine.
#[derive(Debug, Clone)]
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

impl Default for EngineProfile {
    fn default() -> Self {
        Self {
            start_url: "about:blank".to_owned(),
            restore_session: false,
            zoom: 1.0,
            data_dir: None,
            user_agent: None,
            enable_javascript: true,
            default_font_size: 16,
            hardware_acceleration: HardwareAccelerationMode::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardwareAccelerationMode {
    Auto,
    Enabled,
    Disabled,
}

// ---------------------------------------------------------------------------
// DTOs carried by delegate callbacks / inspector results
// ---------------------------------------------------------------------------

/// Download lifecycle events surfaced through `EngineDelegate`.
#[derive(Debug, Clone)]
pub enum DownloadEvent {
    Started {
        id: u64,
        filename: String,
        total_bytes: Option<u64>,
        mime: String,
    },
    Progress {
        id: u64,
        received_bytes: u64,
        total_bytes: Option<u64>,
    },
    Completed {
        id: u64,
    },
    Failed {
        id: u64,
        message: String,
    },
}

/// A permission request from a page, surfaced for the shell/user to decide.
#[derive(Debug, Clone)]
pub struct PermissionEvent {
    pub origin: String,
    pub permission: Permission,
}

/// The permission kinds a page may request at v1.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Geolocation,
    Notifications,
    Camera,
    Microphone,
    ClipboardRead,
    PersistentStorage,
}

/// Hit-test result returned by `EngineInspector::inspect_element_at`.
#[derive(Debug, Clone)]
pub struct ElementInfo {
    pub node_id: usize,
    pub tag: String,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub attributes: Vec<(String, String)>,
    pub text: String,
    /// Axis-aligned bounding box in physical viewport pixels: `(x, y, w, h)`.
    pub bbox: Option<(f64, f64, f64, f64)>,
}

/// Coarse document snapshot returned by `EngineInspector::capture_snapshot`.
#[derive(Debug, Clone)]
pub struct PageSnapshot {
    pub url: String,
    pub title: Option<String>,
    pub viewport: (u32, u32),
    pub text_content: String,
    pub element_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Phase 0 gate (docs/PLAN.md): "the trait shape compiles, basic DTO
    /// tests pass". This proves a concrete Engine/Delegate/Inspector
    /// implementation satisfies the traits.
    struct NullEngine;
    impl Engine for NullEngine {
        fn load_uri(&mut self, _uri: &str) {}
        fn reload(&mut self) {}
        fn stop(&mut self) {}
        fn go_back(&mut self) {}
        fn go_forward(&mut self) {}
        fn can_go_back(&self) -> bool {
            false
        }
        fn can_go_forward(&self) -> bool {
            false
        }
        fn current_uri(&self) -> Option<String> {
            None
        }
        fn current_title(&self) -> Option<String> {
            None
        }
        fn is_loading(&self) -> bool {
            false
        }
        fn estimated_load_progress(&self) -> f64 {
            0.0
        }
        fn find_text(&mut self, _q: &str, _case_sensitive: bool, _forward: bool) -> u32 {
            0
        }
        fn clear_find(&mut self) {}
        fn zoom_level(&self) -> f64 {
            1.0
        }
        fn set_zoom_level(&mut self, _z: f64) {}
        fn execute_javascript(&mut self, _src: &str) {}
        fn set_delegate(&mut self, _delegate: Box<dyn EngineDelegate>) {}
        fn inspector(&self) -> Option<&dyn EngineInspector> {
            None
        }
        fn diagnostics(&self) -> Vec<EngineDiagnostic> {
            Vec::new()
        }
    }

    #[derive(Default)]
    struct SinkDelegate {
        titles: Vec<String>,
    }
    impl EngineDelegate for SinkDelegate {
        fn uri_changed(&mut self, _uri: &str) {}
        fn title_changed(&mut self, title: &str) {
            self.titles.push(title.to_owned());
        }
        fn load_progress(&mut self, _progress: f64) {}
        fn load_finished(&mut self) {}
        fn load_failed(&mut self, _message: &str) {}
        fn download_event(&mut self, _event: DownloadEvent) {}
        fn permission_requested(&mut self, _event: PermissionEvent) {}
        fn context_menu(&mut self, _context: &str) {}
    }

    #[test]
    fn trait_shape_compiles_and_dispatches() {
        let mut engine = NullEngine;
        let mut sink = SinkDelegate::default();
        engine.set_delegate(Box::new(SinkDelegate::default()));
        sink.title_changed("Vixen");
        assert_eq!(sink.titles, vec!["Vixen".to_owned()]);
        assert_eq!(engine.diagnostics(), Vec::<EngineDiagnostic>::new());
    }

    #[test]
    fn diagnostic_shape_matches_spec() {
        // docs/SPEC.md "Diagnostics shape": code is a stable &'static str.
        let d = EngineDiagnostic::new(
            EngineDiagnosticCategory::ParseDom,
            "parse-dom.budget",
            "node budget exceeded",
        );
        assert_eq!(d.code, "parse-dom.budget");
        assert_eq!(d.category, EngineDiagnosticCategory::ParseDom);
        assert_eq!(d.message, "node budget exceeded");
    }

    #[test]
    fn profile_default_is_sane() {
        let p = EngineProfile::default();
        assert_eq!(p.zoom, 1.0);
        assert!(p.enable_javascript);
        assert_eq!(p.hardware_acceleration, HardwareAccelerationMode::Auto);
    }
}
