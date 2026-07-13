//! Browser-scoped commands, events, errors, and transport contracts.

use std::fmt;

use crate::{
    BrowserId, BrowsingContextId, DocumentId, DownloadEvent, DownloadId, EngineDiagnostic, FrameId,
    NavigationId, ProfileId, RequestId, RuntimeContextId,
};

/// Maximum number of document-order nodes in an accessibility snapshot.
pub const ACCESSIBILITY_MAX_NODES: usize = 1024;

/// Maximum UTF-8 bytes retained for each accessibility string field and for
/// the aggregate action strings on one node.
pub const ACCESSIBILITY_MAX_STRING_BYTES: usize = 512;
/// Maximum UTF-8 bytes accepted by one accessibility set-value action.
pub const ACCESSIBILITY_MAX_VALUE_BYTES: usize = 16 * 1024;

/// A bounded projection of the active document's current semantic hierarchy.
#[derive(Debug, Clone, PartialEq)]
pub struct AccessibilitySnapshot {
    pub context_id: BrowsingContextId,
    pub document_id: DocumentId,
    pub source_generation: u64,
    pub generation: u64,
    pub viewport: (u32, u32),
    pub nodes: Vec<AccessibilityNode>,
    pub truncated: bool,
}

impl AccessibilitySnapshot {
    /// Recompute the stable nonzero fingerprint for this exact projection.
    pub fn refresh_generation(&mut self) {
        let mut hash = AccessibilityHash::new();
        hash.u64(self.source_generation);
        hash.u64(u64::from(self.viewport.0));
        hash.u64(u64::from(self.viewport.1));
        hash.boolean(self.truncated);
        hash.u64(self.nodes.len() as u64);
        for node in &self.nodes {
            hash.u64(node.id as u64);
            match node.parent_id {
                Some(parent_id) => {
                    hash.byte(1);
                    hash.u64(parent_id as u64);
                }
                None => hash.byte(0),
            }
            hash.u64(node.controls_ids.len() as u64);
            for controls_id in &node.controls_ids {
                hash.u64(*controls_id as u64);
            }
            hash.u64(node.described_by_ids.len() as u64);
            for described_by_id in &node.described_by_ids {
                hash.u64(*described_by_id as u64);
            }
            hash.u64(node.details_ids.len() as u64);
            for details_id in &node.details_ids {
                hash.u64(*details_id as u64);
            }
            hash.u64(node.owns_ids.len() as u64);
            for owns_id in &node.owns_ids {
                hash.u64(*owns_id as u64);
            }
            hash.string(&node.role);
            hash.string(&node.label);
            hash.string(&node.description);
            hash.optional_string(node.value.as_deref());
            match node.text_selection {
                Some(selection) => {
                    hash.byte(1);
                    hash.u64(u64::from(selection.base_offset));
                    hash.u64(u64::from(selection.extent_offset));
                }
                None => hash.byte(0),
            }
            match node.range {
                Some(range) => {
                    hash.byte(1);
                    hash.u64(range.current.to_bits());
                    hash.u64(range.minimum.to_bits());
                    hash.u64(range.maximum.to_bits());
                    hash.u64(range.step.to_bits());
                }
                None => hash.byte(0),
            }
            match node.bbox {
                Some(bbox) => {
                    hash.byte(1);
                    hash.u64(bbox.x.to_bits());
                    hash.u64(bbox.y.to_bits());
                    hash.u64(bbox.width.to_bits());
                    hash.u64(bbox.height.to_bits());
                }
                None => hash.byte(0),
            }
            hash.boolean(node.focused);
            hash.boolean(node.disabled);
            hash.optional_bool(node.checked);
            hash.optional_bool(node.mixed);
            hash.boolean(node.selected);
            hash.optional_bool(node.expanded);
            match node.heading_level {
                Some(level) => {
                    hash.byte(1);
                    hash.byte(level);
                }
                None => hash.byte(0),
            }
            hash.boolean(node.hidden);
            hash.boolean(node.live_region);
            hash.boolean(node.focusable);
            hash.u64(node.actions.len() as u64);
            for action in &node.actions {
                hash.string(action);
            }
        }
        self.generation = hash.finish();
    }
}

struct AccessibilityHash(u64);

impl AccessibilityHash {
    const fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn byte(&mut self, byte: u8) {
        self.0 ^= u64::from(byte);
        self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
    }

    fn boolean(&mut self, value: bool) {
        self.byte(u8::from(value));
    }

    fn u64(&mut self, value: u64) {
        for byte in value.to_le_bytes() {
            self.byte(byte);
        }
    }

    fn string(&mut self, value: &str) {
        self.u64(value.len() as u64);
        for byte in value.bytes() {
            self.byte(byte);
        }
    }

    fn optional_string(&mut self, value: Option<&str>) {
        match value {
            Some(value) => {
                self.byte(1);
                self.string(value);
            }
            None => self.byte(0),
        }
    }

    fn optional_bool(&mut self, value: Option<bool>) {
        match value {
            Some(value) => {
                self.byte(1);
                self.boolean(value);
            }
            None => self.byte(0),
        }
    }

    fn finish(self) -> u64 {
        let value = self.0 & i64::MAX as u64;
        if value == 0 { 1 } else { value }
    }
}

/// One semantic element in stable DOM document order.
#[derive(Debug, Clone, PartialEq)]
pub struct AccessibilityNode {
    pub id: usize,
    /// Nearest emitted semantic DOM ancestor in this snapshot.
    pub parent_id: Option<usize>,
    /// Emitted semantic nodes referenced by this element's `aria-controls`.
    pub controls_ids: Vec<usize>,
    /// Emitted semantic nodes referenced by this element's `aria-describedby`.
    pub described_by_ids: Vec<usize>,
    /// Emitted semantic nodes referenced by this element's `aria-details`.
    pub details_ids: Vec<usize>,
    /// Emitted semantic nodes reparented by this element's `aria-owns`.
    pub owns_ids: Vec<usize>,
    pub role: String,
    pub label: String,
    pub description: String,
    pub value: Option<String>,
    pub text_selection: Option<AccessibilityTextSelection>,
    pub range: Option<AccessibilityRange>,
    pub bbox: Option<AccessibilityRect>,
    pub focused: bool,
    pub disabled: bool,
    pub checked: Option<bool>,
    pub mixed: Option<bool>,
    pub selected: bool,
    pub expanded: Option<bool>,
    pub heading_level: Option<u8>,
    pub hidden: bool,
    pub live_region: bool,
    pub focusable: bool,
    pub actions: Vec<String>,
}

/// UTF-16 offsets selected in a writable native text control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessibilityTextSelection {
    pub base_offset: u32,
    pub extent_offset: u32,
}

/// Numeric state for an adjustable native range control.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AccessibilityRange {
    pub current: f64,
    pub minimum: f64,
    pub maximum: f64,
    pub step: f64,
}

/// Physical viewport coordinates for a semantic element's layout border box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AccessibilityRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A semantic action implemented by the authoritative live document runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessibilityAction {
    Focus,
    SetValue(String),
    Increase,
    Decrease,
}

/// Host application state that affects document visibility and input routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostLifecycle {
    Resumed,
    Inactive,
    Hidden,
    Paused,
    Detached,
}

impl HostLifecycle {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Resumed => "resumed",
            Self::Inactive => "inactive",
            Self::Hidden => "hidden",
            Self::Paused => "paused",
            Self::Detached => "detached",
        }
    }
}

/// Monotonic presentation state supplied by a native browser host.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HostViewState {
    pub generation: u64,
    pub viewport: (u32, u32),
    pub scale_factor: f64,
    pub focused: bool,
    pub visible: bool,
    pub lifecycle: HostLifecycle,
}

impl Default for HostViewState {
    fn default() -> Self {
        Self {
            generation: 0,
            viewport: (800, 600),
            scale_factor: 1.0,
            focused: true,
            visible: true,
            lifecycle: HostLifecycle::Resumed,
        }
    }
}

impl HostViewState {
    pub const fn accepts_input(self) -> bool {
        self.focused && self.visible && matches!(self.lifecycle, HostLifecycle::Resumed)
    }
}

impl AccessibilityAction {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Focus => "focus",
            Self::SetValue(_) => "set_value",
            Self::Increase => "increase",
            Self::Decrease => "decrease",
        }
    }
}

/// Stable browser command/error codes consumed by adapters and automation.
pub mod error_codes {
    pub const INVALID_ARGUMENT: &str = "browser.invalid-argument";
    pub const UNKNOWN_CONTEXT: &str = "browser.unknown-context";
    pub const STALE_CONTEXT: &str = "browser.stale-context";
    pub const COMMAND_QUEUE_FULL: &str = "browser.command-queue-full";
    pub const ID_EXHAUSTED: &str = "browser.id-exhausted";
    pub const CONTEXT_LIMIT: &str = "browser.context-limit";
    pub const STALE_DOCUMENT: &str = "browser.stale-document";
    pub const STALE_RUNTIME: &str = "browser.stale-runtime";
    pub const STALE_ACCESSIBILITY: &str = "browser.stale-accessibility";
    pub const STALE_HOST_VIEW: &str = "browser.stale-host-view";
    pub const NAVIGATION_LOAD: &str = "navigation.load";
    pub const PROFILE: &str = "browser.profile";
    pub const EVENT_LAGGED: &str = "browser.event-lagged";
    pub const CLOSED: &str = "browser.closed";
}

/// Stable error returned at the frontend-to-core command boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserError {
    pub code: &'static str,
    pub message: String,
}

impl BrowserError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for BrowserError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for BrowserError {}

/// Browser-scoped lifecycle commands. Dispatch acknowledges acceptance only;
/// observable navigation completion is reported through [`BrowserEvent`].
#[derive(Debug, Clone, PartialEq)]
pub enum BrowserCommand {
    LoadProfileSession,
    /// Persist a session projection of BrowserCore's current context registry.
    SaveCurrentProfileSession,
    /// Transitional compatibility command for shells that still author state.
    SaveProfileSession {
        session: ProfileSessionState,
    },
    ClearProfileData {
        selection: ProfileDataSelection,
    },
    CreateBrowsingContext,
    CloseBrowsingContext {
        context_id: BrowsingContextId,
    },
    ActivateBrowsingContext {
        context_id: BrowsingContextId,
    },
    Navigate {
        context_id: BrowsingContextId,
        url: String,
    },
    Reload {
        context_id: BrowsingContextId,
    },
    Stop {
        context_id: BrowsingContextId,
    },
    TraverseHistory {
        context_id: BrowsingContextId,
        delta: i32,
    },
    GetBrowsingContextState {
        context_id: BrowsingContextId,
    },
    UpdateHostViewState {
        context_id: BrowsingContextId,
        state: HostViewState,
    },
    /// Capture the authoritative browser and context state in one owner-thread
    /// operation.
    GetBrowserSnapshot,
    ConfigureBrowsingContext {
        context_id: BrowsingContextId,
        config: BrowsingContextConfig,
    },
    GetNavigationHistory {
        context_id: BrowsingContextId,
    },
    ResetNavigationHistory {
        context_id: BrowsingContextId,
    },
    Evaluate {
        context_id: BrowsingContextId,
        document_id: DocumentId,
        runtime_context_id: RuntimeContextId,
        source: String,
    },
    EvaluateForAutomation {
        context_id: BrowsingContextId,
        document_id: DocumentId,
        runtime_context_id: RuntimeContextId,
        source: String,
    },
    DispatchMouseEvent {
        context_id: BrowsingContextId,
        document_id: DocumentId,
        runtime_context_id: RuntimeContextId,
        node_id: usize,
        event_type: String,
        event: MouseEventData,
    },
    DispatchKeyEvent {
        context_id: BrowsingContextId,
        document_id: DocumentId,
        runtime_context_id: RuntimeContextId,
        event_type: String,
        event: KeyEventData,
    },
    FindText {
        context_id: BrowsingContextId,
        document_id: DocumentId,
        query: String,
        case_sensitive: bool,
    },
    Snapshot {
        context_id: BrowsingContextId,
        document_id: DocumentId,
        viewport: (u32, u32),
    },
    AccessibilitySnapshot {
        context_id: BrowsingContextId,
        document_id: DocumentId,
        viewport: (u32, u32),
    },
    DispatchAccessibilityAction {
        context_id: BrowsingContextId,
        document_id: DocumentId,
        runtime_context_id: RuntimeContextId,
        viewport: (u32, u32),
        source_generation: u64,
        node_id: usize,
        action: AccessibilityAction,
    },
    QuerySelectorAll {
        context_id: BrowsingContextId,
        document_id: DocumentId,
        selector: String,
        viewport: (u32, u32),
    },
    ComputedStyle {
        context_id: BrowsingContextId,
        document_id: DocumentId,
        node_id: usize,
        viewport: (u32, u32),
    },
    DisplayListText {
        context_id: BrowsingContextId,
        document_id: DocumentId,
        viewport: (u32, u32),
    },
    Diagnostics {
        context_id: BrowsingContextId,
        document_id: DocumentId,
    },
    DocumentText {
        context_id: BrowsingContextId,
        document_id: DocumentId,
        viewport: (u32, u32),
        kind: DocumentTextKind,
    },
    HitTest {
        context_id: BrowsingContextId,
        document_id: DocumentId,
        viewport: (u32, u32),
        x: f64,
        y: f64,
    },
    FocusProjection {
        context_id: BrowsingContextId,
        document_id: DocumentId,
        element_id: String,
    },
    FormSubmission {
        context_id: BrowsingContextId,
        document_id: DocumentId,
        form_id: String,
    },
}

/// Immediate result after a command has been validated and accepted.
#[derive(Debug, Clone, PartialEq)]
pub enum BrowserCommandResult {
    Accepted,
    ProfileSession(ProfileSessionState),
    BrowserSnapshot(BrowserSnapshot),
    BrowsingContextCreated { context_id: BrowsingContextId },
    NavigationAccepted { navigation_id: NavigationId },
    BrowsingContextState(BrowsingContextState),
    NavigationHistory(NavigationHistorySnapshot),
    Evaluation(EvaluationResult),
    AutomationEvaluation(AutomationEvaluation),
    InputDispatched(InputDispatchResult),
    FindText(FindTextResult),
    Snapshot(crate::PageSnapshot),
    AccessibilitySnapshot(AccessibilitySnapshot),
    SelectorMatches(Vec<crate::ElementInfo>),
    ComputedStyle(Vec<(String, String)>),
    DisplayListText(String),
    Diagnostics(Vec<crate::EngineDiagnostic>),
    DocumentText(String),
    HitTest(Option<crate::ElementInfo>),
    FocusProjection(FocusProjection),
    FormSubmission(FormSubmissionInfo),
}

/// Bounded visible-text match count for browser chrome find UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FindTextResult {
    pub matches: u32,
}

/// Inspector/runtime settings applied by BrowserCore to exactly one browsing
/// context. Scripts are split so host/bootstrap scripts run before page init
/// scripts, and both run before author scripts on each new document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BrowsingContextConfig {
    pub extra_http_headers: Vec<(String, String)>,
    pub cache_disabled: bool,
    pub bypass_csp: bool,
    pub preload_scripts: Vec<String>,
    pub new_document_scripts: Vec<String>,
    pub permission_grants: Vec<RuntimePermissionGrant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePermissionGrant {
    pub origin: Option<String>,
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AutomationEvaluation {
    pub value: ScriptValue,
    pub effects: RuntimeEffects,
    /// Exact action order, including same-document changes interleaved with
    /// cross-document navigations that may already have been superseded.
    pub navigation_actions: Vec<NavigationActionOutcome>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EvaluationResult {
    pub value: ScriptValue,
    pub navigation_actions: Vec<NavigationActionOutcome>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InputDispatchResult {
    pub effects: RuntimeEffects,
    pub navigation_actions: Vec<NavigationActionOutcome>,
}

/// Ordered host outcome of one script-created navigation/history action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavigationActionOutcome {
    SameDocument {
        url: String,
    },
    CrossDocument {
        navigation_id: NavigationId,
        kind: CrossDocumentNavigationKind,
    },
}

/// Presentation semantics attached to an exact cross-document navigation ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrossDocumentNavigationKind {
    Regular,
    ContentReplacement { replaced_document_id: DocumentId },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RuntimeEffects {
    pub console: Vec<RuntimeConsoleEvent>,
    pub dialogs: Vec<RuntimeDialogEvent>,
    pub bindings: Vec<RuntimeBindingEvent>,
    pub network: Vec<RuntimeNetworkEvent>,
    pub exceptions: Vec<RuntimeExceptionEvent>,
}

impl RuntimeEffects {
    pub fn is_empty(&self) -> bool {
        self.console.is_empty()
            && self.dialogs.is_empty()
            && self.bindings.is_empty()
            && self.network.is_empty()
            && self.exceptions.is_empty()
    }

    pub fn extend(&mut self, mut other: Self) {
        self.console.append(&mut other.console);
        self.dialogs.append(&mut other.dialogs);
        self.bindings.append(&mut other.bindings);
        self.network.append(&mut other.network);
        self.exceptions.append(&mut other.exceptions);
    }
}

/// A script exception produced by a committed runtime generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeExceptionEvent {
    pub error: BrowserError,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeConsoleEvent {
    pub kind: String,
    pub args: Vec<RuntimeConsoleArg>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeConsoleArg {
    pub type_name: String,
    pub subtype: Option<String>,
    pub value: Option<RuntimeConsoleValue>,
    pub unserializable_value: Option<String>,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeConsoleValue {
    String(String),
    Number(f64),
    Bool(bool),
    Null,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeDialogEvent {
    pub kind: String,
    pub message: String,
    pub default_prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeBindingEvent {
    pub name: String,
    pub payload: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeNetworkEvent {
    Request {
        request_id: String,
        url: String,
        method: String,
    },
    Redirect {
        request_id: String,
        from: String,
        to: String,
        status: u16,
    },
    Response {
        request_id: String,
        url: String,
        status: u16,
    },
    Failure {
        request_id: String,
        url: String,
        error_text: String,
        blocked_reason: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct MouseEventData {
    pub x: f64,
    pub y: f64,
    pub button: i32,
    pub buttons: i64,
    pub detail: i64,
    pub related_node_id: Option<usize>,
    pub bubbles: bool,
    pub ctrl_key: bool,
    pub shift_key: bool,
    pub alt_key: bool,
    pub meta_key: bool,
    pub delta_x: f64,
    pub delta_y: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEventData {
    pub key: String,
    pub code: String,
    pub text: String,
    pub apply_text: bool,
    pub ctrl_key: bool,
    pub shift_key: bool,
    pub alt_key: bool,
    pub meta_key: bool,
    pub repeat: bool,
    pub location: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavigationHistorySnapshot {
    pub current_index: usize,
    pub entries: Vec<NavigationHistoryEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavigationHistoryEntry {
    pub url: String,
    pub title: Option<String>,
    pub same_document: bool,
}

/// Dependency-free shell/session restore state owned by the open profile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileSessionState {
    pub tabs: Vec<String>,
    pub active_index: usize,
}

/// Atomic projection of BrowserCore's current context registry.
///
/// Contexts are ordered by their monotonically allocated context ID, which is
/// creation order. If event consumption reports `browser.event-lagged`, pending
/// frontend operations are indeterminate and must be reconciled from a fresh
/// snapshot rather than assumed to have succeeded.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BrowserSnapshot {
    pub active_context_id: Option<BrowsingContextId>,
    pub contexts: Vec<BrowsingContextState>,
}

/// Profile-wide persisted data groups selected by clear-data UI or automation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProfileDataSelection {
    pub cookies: bool,
    pub fetch_cache: bool,
    pub history: bool,
    pub session: bool,
    pub web_storage: bool,
    pub downloads: bool,
    pub permissions: bool,
    pub security_state: bool,
}

impl ProfileDataSelection {
    pub const fn all() -> Self {
        Self {
            cookies: true,
            fetch_cache: true,
            history: true,
            session: true,
            web_storage: true,
            downloads: true,
            permissions: true,
            security_state: true,
        }
    }

    pub const fn browsing_data() -> Self {
        Self {
            session: false,
            ..Self::all()
        }
    }

    pub const fn is_empty(self) -> bool {
        !self.cookies
            && !self.fetch_cache
            && !self.history
            && !self.session
            && !self.web_storage
            && !self.downloads
            && !self.permissions
            && !self.security_state
    }
}

/// Stable text projections used by CLI and diagnostic adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentTextKind {
    Dom,
    TextContent,
    LayoutTree,
    Lines,
    PaintStats,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FocusProjection {
    pub target: crate::ElementInfo,
    pub events: Vec<FocusEventInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusEventInfo {
    pub event: String,
    pub target: Option<usize>,
    pub bubbles: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FormSubmissionInfo {
    pub form: crate::ElementInfo,
    pub action: String,
    pub method: String,
    pub enctype: String,
    pub content_type: String,
    pub entries: Vec<FormEntryInfo>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormEntryInfo {
    pub name: String,
    pub value: FormEntryValueInfo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormEntryValueInfo {
    Text(String),
    File {
        filename: String,
        content_type: String,
        body: Vec<u8>,
    },
}

/// Dependency-free scalar projection of a JavaScript evaluation result.
#[derive(Debug, Clone, PartialEq)]
pub enum ScriptValue {
    Int32(i32),
    Number(f64),
    String(String),
    Bool(bool),
    Null,
    Undefined,
    Object,
}

impl ScriptValue {
    /// Stable scalar formatting used by text-based frontend adapters.
    pub fn to_display(&self) -> String {
        match self {
            Self::Int32(value) => value.to_string(),
            Self::Number(value) if value.fract() == 0.0 && value.abs() < 1e21 => {
                format!("{}", *value as i64)
            }
            Self::Number(value) => value.to_string(),
            Self::String(value) => value.clone(),
            Self::Bool(value) => value.to_string(),
            Self::Null => "null".to_owned(),
            Self::Undefined => "undefined".to_owned(),
            Self::Object => "[object]".to_owned(),
        }
    }
}

/// Current bounded presentation state for one top-level browsing context.
#[derive(Debug, Clone, PartialEq)]
pub struct BrowsingContextState {
    pub context_id: BrowsingContextId,
    pub main_frame_id: FrameId,
    pub document_id: DocumentId,
    pub runtime_context_id: Option<RuntimeContextId>,
    pub active_navigation_id: Option<NavigationId>,
    pub url: String,
    pub title: Option<String>,
    pub history_length: usize,
    pub history_index: usize,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub is_loading: bool,
    pub load_progress: f64,
}

/// Lifecycle phase for a navigation generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationPhase {
    Intent,
    Policy,
    Request,
    Response,
    Commit,
    Parse,
    ScriptsAndSubresources,
    DomContentLoaded,
    Load,
    Settled,
    Failed,
    Cancelled,
}

impl NavigationPhase {
    /// Whether this phase is the single terminal outcome for a navigation.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Settled | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationCancellationReason {
    Stopped,
    Superseded,
    ContextClosed,
    BrowserShutdown,
}

/// Typed scope attached to diagnostics so stale/cross-target work is visible.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiagnosticScope {
    pub profile_id: Option<ProfileId>,
    pub browser_id: Option<BrowserId>,
    pub context_id: Option<BrowsingContextId>,
    pub frame_id: Option<FrameId>,
    pub navigation_id: Option<NavigationId>,
    pub document_id: Option<DocumentId>,
    pub request_id: Option<RequestId>,
    pub runtime_context_id: Option<RuntimeContextId>,
    pub download_id: Option<DownloadId>,
}

/// Ordered lifecycle/state event emitted by the authoritative browser core.
#[derive(Debug, Clone, PartialEq)]
pub enum BrowserEvent {
    BrowsingContextCreated {
        state: BrowsingContextState,
    },
    BrowsingContextClosed {
        context_id: BrowsingContextId,
    },
    ActiveBrowsingContextChanged {
        context_id: Option<BrowsingContextId>,
    },
    NavigationRequested {
        context_id: BrowsingContextId,
        frame_id: FrameId,
        navigation_id: NavigationId,
        predecessor_navigation_id: Option<NavigationId>,
        kind: CrossDocumentNavigationKind,
        url: String,
    },
    NavigationStarted {
        context_id: BrowsingContextId,
        frame_id: FrameId,
        navigation_id: NavigationId,
        request_id: RequestId,
        url: String,
    },
    NavigationRedirected {
        context_id: BrowsingContextId,
        frame_id: FrameId,
        navigation_id: NavigationId,
        request_id: RequestId,
        next_request_id: RequestId,
        from_url: String,
        to_url: String,
        status: u16,
    },
    NavigationPhaseChanged {
        context_id: BrowsingContextId,
        frame_id: FrameId,
        navigation_id: NavigationId,
        phase: NavigationPhase,
    },
    RuntimeContextDestroyed {
        context_id: BrowsingContextId,
        frame_id: FrameId,
        document_id: DocumentId,
        runtime_context_id: RuntimeContextId,
    },
    DocumentDiscarded {
        context_id: BrowsingContextId,
        frame_id: FrameId,
        document_id: DocumentId,
        replaced_by: Option<NavigationId>,
    },
    NavigationCommitted {
        context_id: BrowsingContextId,
        frame_id: FrameId,
        navigation_id: NavigationId,
        request_id: Option<RequestId>,
        document_id: DocumentId,
        runtime_context_id: Option<RuntimeContextId>,
        url: String,
    },
    RuntimeContextCreated {
        context_id: BrowsingContextId,
        frame_id: FrameId,
        document_id: DocumentId,
        runtime_context_id: RuntimeContextId,
    },
    RuntimeEffects {
        context_id: BrowsingContextId,
        frame_id: FrameId,
        document_id: DocumentId,
        runtime_context_id: RuntimeContextId,
        url: String,
        effects: RuntimeEffects,
    },
    DomContentLoaded {
        context_id: BrowsingContextId,
        frame_id: FrameId,
        navigation_id: NavigationId,
        document_id: DocumentId,
    },
    DocumentLoadCompleted {
        context_id: BrowsingContextId,
        frame_id: FrameId,
        navigation_id: NavigationId,
        document_id: DocumentId,
    },
    NavigationCancelled {
        context_id: BrowsingContextId,
        frame_id: FrameId,
        navigation_id: NavigationId,
        request_id: Option<RequestId>,
        reason: NavigationCancellationReason,
    },
    NavigationFailed {
        context_id: BrowsingContextId,
        frame_id: FrameId,
        navigation_id: NavigationId,
        request_id: Option<RequestId>,
        error: BrowserError,
    },
    BrowsingContextStateChanged {
        state: BrowsingContextState,
    },
    Download {
        source_context_id: Option<BrowsingContextId>,
        source_document_id: Option<DocumentId>,
        event: DownloadEvent,
    },
    Diagnostic {
        scope: DiagnosticScope,
        diagnostic: EngineDiagnostic,
    },
}

/// Thread-safe transport handle. Implementations validate and enqueue quickly;
/// they never wait for network, script, layout, or render completion.
pub trait BrowserHandle: Send {
    fn dispatch(&mut self, command: BrowserCommand) -> Result<BrowserCommandResult, BrowserError>;

    /// Return the next ordered event without blocking.
    ///
    /// On `browser.event-lagged`, pending frontend operations are indeterminate;
    /// query [`BrowserCommand::GetBrowserSnapshot`] and reconcile instead of
    /// assuming those operations succeeded.
    fn try_next_event(&mut self) -> Result<Option<BrowserEvent>, BrowserError>;
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    struct FakeHandle {
        events: VecDeque<BrowserEvent>,
    }

    impl BrowserHandle for FakeHandle {
        fn dispatch(
            &mut self,
            command: BrowserCommand,
        ) -> Result<BrowserCommandResult, BrowserError> {
            match command {
                BrowserCommand::CreateBrowsingContext => {
                    Ok(BrowserCommandResult::BrowsingContextCreated {
                        context_id: BrowsingContextId::new(1).unwrap(),
                    })
                }
                _ => Ok(BrowserCommandResult::Accepted),
            }
        }

        fn try_next_event(&mut self) -> Result<Option<BrowserEvent>, BrowserError> {
            Ok(self.events.pop_front())
        }
    }

    #[test]
    fn browser_handle_is_object_safe_and_preserves_event_order() {
        let context_id = BrowsingContextId::new(1).unwrap();
        let mut handle: Box<dyn BrowserHandle> = Box::new(FakeHandle {
            events: VecDeque::from([
                BrowserEvent::ActiveBrowsingContextChanged {
                    context_id: Some(context_id),
                },
                BrowserEvent::BrowsingContextClosed { context_id },
            ]),
        });

        assert_eq!(
            handle
                .dispatch(BrowserCommand::CreateBrowsingContext)
                .unwrap(),
            BrowserCommandResult::BrowsingContextCreated { context_id }
        );
        assert!(matches!(
            handle.try_next_event().unwrap(),
            Some(BrowserEvent::ActiveBrowsingContextChanged { .. })
        ));
        assert!(matches!(
            handle.try_next_event().unwrap(),
            Some(BrowserEvent::BrowsingContextClosed { .. })
        ));
        assert_eq!(handle.try_next_event().unwrap(), None);
    }

    #[test]
    fn browser_error_has_stable_display_shape() {
        let error = BrowserError::new(error_codes::UNKNOWN_CONTEXT, "context 9 is unknown");
        assert_eq!(error.code, "browser.unknown-context");
        assert_eq!(
            error.to_string(),
            "browser.unknown-context: context 9 is unknown"
        );
    }

    #[test]
    fn accessibility_generation_is_nonzero_stable_and_content_sensitive() {
        let mut snapshot = AccessibilitySnapshot {
            context_id: BrowsingContextId::new(1).unwrap(),
            document_id: DocumentId::new(1).unwrap(),
            source_generation: 1,
            generation: 0,
            viewport: (800, 600),
            nodes: vec![AccessibilityNode {
                id: 1,
                parent_id: None,
                controls_ids: vec![],
                described_by_ids: vec![],
                details_ids: vec![],
                owns_ids: vec![],
                role: "button".to_owned(),
                label: "Before".to_owned(),
                description: String::new(),
                value: None,
                text_selection: None,
                range: None,
                bbox: None,
                focused: false,
                disabled: false,
                checked: None,
                mixed: None,
                selected: false,
                expanded: None,
                heading_level: None,
                hidden: false,
                live_region: false,
                focusable: true,
                actions: vec!["tap".to_owned()],
            }],
            truncated: false,
        };
        snapshot.refresh_generation();
        let first = snapshot.generation;
        assert_ne!(first, 0);
        assert!(first <= i64::MAX as u64);
        snapshot.refresh_generation();
        assert_eq!(snapshot.generation, first);
        snapshot.nodes[0].disabled = true;
        snapshot.refresh_generation();
        assert_ne!(snapshot.generation, first);
        let without_parent = snapshot.generation;
        snapshot.nodes[0].parent_id = Some(7);
        snapshot.refresh_generation();
        assert_ne!(snapshot.generation, without_parent);
        let without_controls = snapshot.generation;
        snapshot.nodes[0].controls_ids.push(9);
        snapshot.refresh_generation();
        assert_ne!(snapshot.generation, without_controls);
        let without_description = snapshot.generation;
        snapshot.nodes[0].description = "More context".to_owned();
        snapshot.refresh_generation();
        assert_ne!(snapshot.generation, without_description);
        let without_range = snapshot.generation;
        snapshot.nodes[0].range = Some(AccessibilityRange {
            current: 4.0,
            minimum: 0.0,
            maximum: 10.0,
            step: 2.0,
        });
        snapshot.refresh_generation();
        assert_ne!(snapshot.generation, without_range);
    }

    #[test]
    fn runtime_effects_retain_script_exceptions() {
        let mut effects = RuntimeEffects::default();
        effects.extend(RuntimeEffects {
            exceptions: vec![RuntimeExceptionEvent {
                error: BrowserError::new("script.eval", "author script failed"),
            }],
            ..RuntimeEffects::default()
        });

        assert!(!effects.is_empty());
        assert_eq!(effects.exceptions[0].error.code, "script.eval");
    }

    #[test]
    fn script_value_display_matches_runtime_scalar_formatting() {
        assert_eq!(ScriptValue::Int32(3).to_display(), "3");
        assert_eq!(ScriptValue::Number(2.5).to_display(), "2.5");
        assert_eq!(ScriptValue::Number(2.0).to_display(), "2");
        assert_eq!(
            ScriptValue::String("vixen".to_owned()).to_display(),
            "vixen"
        );
        assert_eq!(ScriptValue::Bool(true).to_display(), "true");
        assert_eq!(ScriptValue::Null.to_display(), "null");
        assert_eq!(ScriptValue::Undefined.to_display(), "undefined");
        assert_eq!(ScriptValue::Object.to_display(), "[object]");
    }

    #[test]
    fn profile_data_presets_are_explicit() {
        assert!(ProfileDataSelection::default().is_empty());
        assert!(ProfileDataSelection::all().session);
        assert!(!ProfileDataSelection::browsing_data().session);
        assert!(ProfileDataSelection::browsing_data().cookies);
    }

    #[test]
    fn navigation_terminal_phases_are_explicit() {
        assert!(NavigationPhase::Settled.is_terminal());
        assert!(NavigationPhase::Failed.is_terminal());
        assert!(NavigationPhase::Cancelled.is_terminal());
        assert!(!NavigationPhase::Load.is_terminal());
        assert!(!NavigationPhase::Commit.is_terminal());
    }
}
