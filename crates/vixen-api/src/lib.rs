//! Vixen public browser contracts and DTOs.
//!
//! This crate defines the seams between the engine (`vixen-engine`) and its
//! consumers (`vixen-ffi` Flutter bridge, `vixen-headless` CLI, and `vixen-wpt`
//! harness). It owns **no concrete engine dependencies** — only contracts and
//! data types — so the API compiles and tests run at zero build cost
//! (docs/ARCHITECTURE.md "Dependency direction", docs/PLAN.md Phase 0 gate).
//!
//! ADR-017 places production ownership behind one browser-scoped command/event
//! seam. This crate is the implementation-free home for those frontend
//! contracts.

#![forbid(unsafe_code)]

mod browser;
mod ids;

pub use browser::{
    ACCESSIBILITY_MAX_NODES, ACCESSIBILITY_MAX_STRING_BYTES, ACCESSIBILITY_MAX_VALUE_BYTES,
    AccessibilityAction, AccessibilityNode, AccessibilityRange, AccessibilityRect,
    AccessibilitySnapshot, AccessibilityTextInputAction, AccessibilityTextInputType,
    AccessibilityTextSelection, AutomationEvaluation, BrowserCommand, BrowserCommandResult,
    BrowserError, BrowserEvent, BrowserHandle, BrowserSnapshot, BrowsingContextConfig,
    BrowsingContextState, CrossDocumentNavigationKind, DiagnosticScope, DocumentTextKind,
    EvaluationResult, FindTextResult, FocusEventInfo, FocusProjection, FormEntryInfo,
    FormEntryValueInfo, FormSubmissionInfo, HostLifecycle, HostViewState, InputDispatchResult,
    KeyEventData, MouseEventData, NavigationActionOutcome, NavigationCancellationReason,
    NavigationHistoryEntry, NavigationHistorySnapshot, NavigationPhase, ProfileDataSelection,
    ProfileSessionState, RuntimeBindingEvent, RuntimeConsoleArg, RuntimeConsoleEvent,
    RuntimeConsoleValue, RuntimeDialogEvent, RuntimeEffects, RuntimeExceptionEvent,
    RuntimeNetworkEvent, RuntimePermissionGrant, ScriptValue, TextInputState,
    error_codes as browser_error_codes,
};
pub use ids::{
    BrowserId, BrowsingContextId, DocumentId, DownloadId, FrameId, InvalidId, NavigationId,
    ProfileId, RequestId, RuntimeContextId,
};

// ---------------------------------------------------------------------------
// Diagnostics (docs/SPEC.md "Diagnostics shape")
// ---------------------------------------------------------------------------

/// A single non-fatal engine diagnostic surfaced to frontends and consumed by
/// the WPT `no-critical-diagnostics` check.
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
// Graphics context seam (docs/ARCHITECTURE.md "Style, layout, paint, and inspection")
// ---------------------------------------------------------------------------

/// Minimal graphics-context abstraction so `vixen-engine` can drive WebRender
/// without taking a platform-window or EGL dependency. GUI frames render
/// through the BrowserCore capture path used by `vixen-ffi`; headless uses
/// `SurfacelessSurface` around an EGL surfaceless context. Per ADR-006 there is
/// one paint path and no `PaintBackend` trait — this is the only seam that varies.
pub trait GlContext {
    /// Ensure this context is current on the calling thread.
    fn make_current(&self);
    /// GL function-pointer lookup; feeds WebRender's `gleam` loader.
    fn proc_address(&self, name: &str) -> *const std::ffi::c_void;
    /// Drawable size in physical pixels.
    fn drawable_size(&self) -> (u32, u32);
}

/// Download lifecycle events surfaced through BrowserCore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadEvent {
    Started {
        id: DownloadId,
        filename: String,
        total_bytes: Option<u64>,
        mime: String,
    },
    Progress {
        id: DownloadId,
        received_bytes: u64,
        total_bytes: Option<u64>,
    },
    Completed {
        id: DownloadId,
    },
    Cancelled {
        id: DownloadId,
    },
    Failed {
        id: DownloadId,
        message: String,
    },
}

/// Hit-test result returned by BrowserCore inspection commands.
#[derive(Debug, Clone, PartialEq)]
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

/// Coarse document snapshot returned by BrowserCore snapshot commands.
#[derive(Debug, Clone, Default, PartialEq)]
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

    #[test]
    fn diagnostic_shape_matches_spec() {
        let diagnostic = EngineDiagnostic::new(
            EngineDiagnosticCategory::ParseDom,
            "parse-dom.budget",
            "node budget exceeded",
        );
        assert_eq!(diagnostic.code, "parse-dom.budget");
        assert_eq!(diagnostic.category, EngineDiagnosticCategory::ParseDom);
        assert_eq!(diagnostic.message, "node budget exceeded");
    }
}
