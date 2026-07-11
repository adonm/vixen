//! Chrome DevTools Protocol (CDP) server — Phase 8 step 1 (docs/PLAN.md).
//!
//! Implements the v1.0-required CDP method surface (docs/SPEC.md "CDP
//! methods required") over a tokio + tokio-tungstenite WebSocket server:
//!
//! - `Browser.getVersion`
//! - `Target.createTarget`, `Target.closeTarget`, `Target.attachToTarget`, `Target.getTargets`
//! - `Page.enable`, `Page.navigate`, `Page.loadEventFired`
//! - `Page.captureScreenshot`, `Page.getLayoutMetrics`
//! - `Runtime.enable`, `Runtime.evaluate`, `Runtime.awaitPromise`, `Runtime.addBinding`, `Runtime.consoleAPICalled`
//! - `Input.dispatchMouseEvent`, `Input.dispatchKeyEvent`
//!
//! Architecture: [`CdpState`] owns protocol presentation state while BrowserCore
//! owns browser state. Direct dispatcher calls remain synchronous; the WebSocket
//! loop polls BrowserCore while navigation-producing requests are pending so the
//! same connection can stop them. Most tests drive the dispatcher directly, with a
//! focused real-socket race test for asynchronous delivery.
//!
//! Phase 8 covers the contract surface; full DOM/inspector backing comes
//! with the cascade (Phase 3 step 3) and host hooks (Phase 6).

#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use vixen_api::{
    BrowserCommand, BrowserCommandResult, BrowserError, BrowserEvent, BrowsingContextConfig,
    BrowsingContextId, BrowsingContextState, CrossDocumentNavigationKind, DocumentId,
    DocumentTextKind, FrameId, KeyEventData, MouseEventData, NavigationActionOutcome, NavigationId,
    NavigationPhase, RuntimeBindingEvent, RuntimeConsoleArg, RuntimeConsoleEvent,
    RuntimeConsoleValue, RuntimeContextId, RuntimeDialogEvent, RuntimeEffects, RuntimeNetworkEvent,
    RuntimePermissionGrant, ScriptValue,
};
use vixen_engine::browser::{BrowserConfig, EngineBrowserHandle, spawn_browser};
use vixen_engine::engine_error::codes;
use vixen_engine::headers::{validate_header_name, validate_header_value};
use vixen_engine::script::JsRuntime;

use crate::browser_adapter::BrowserProfile;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const DEFAULT_CAPTURE_VIEWPORT: (u32, u32) = (800, 600);
const CDP_DOCUMENT_NODE_ID: usize = 1_000_000_000;
const MAX_REMOTE_HANDLES: usize = 1_024;
const MAX_PENDING_CORE_NAVIGATIONS: usize = 1_024;
const MAX_PENDING_SOCKET_NAVIGATIONS: usize = 64;
const NAVIGATION_WAIT_TIMEOUT: Duration = Duration::from_secs(35);
const CORE_EVENT_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// CDP server entry point. Binds `127.0.0.1:{port}` and serves the
/// WebSocket CDP protocol until the process is killed.
///
/// BrowserCore owns its dedicated engine thread. The socket adapter stays on a
/// local task so requests against the shared protocol state remain ordered.
pub async fn serve(port: u16) -> std::io::Result<()> {
    serve_with_initial_url(port, None).await
}

/// CDP server entry point with an already requested page URL. The URL is loaded
/// through the same headless trust boundary as CLI page actions before clients
/// connect, so early `Runtime.evaluate` DOM probes see the requested page.
pub async fn serve_with_initial_url(port: u16, initial_url: Option<String>) -> std::io::Result<()> {
    serve_with_initial_url_and_profile(port, initial_url, None).await
}

/// CDP server entry point with an optional persistent BrowserCore profile root.
pub async fn serve_with_initial_url_and_profile(
    port: u16,
    initial_url: Option<String>,
    profile_dir: Option<PathBuf>,
) -> std::io::Result<()> {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let listener = TcpListener::bind(addr).await?;
    let mut initial_state =
        CdpState::new_with_profile(profile_dir.as_deref()).map_err(std::io::Error::other)?;
    if let Some(url) = initial_url
        && let Err(e) = initial_state.seed_initial_target(url)
    {
        eprintln!("vixen-headless: initial CDP load failed: {e}");
    }
    let state: Rc<RefCell<CdpState>> = Rc::new(RefCell::new(initial_state));
    eprintln!("vixen-headless: CDP listening on ws://127.0.0.1:{port}");

    loop {
        let (stream, _) = listener.accept().await?;
        let state = Rc::clone(&state);
        // `spawn_local` because `state` is `!Send`.
        tokio::task::spawn_local(async move {
            if let Err(e) = handle_connection(stream, state).await {
                eprintln!("vixen-headless: CDP connection error: {e}");
            }
        })
        .await
        .ok();
    }
}

async fn handle_connection(
    stream: TcpStream,
    state: Rc<RefCell<CdpState>>,
) -> Result<(), BoxError> {
    let ws = tokio_tungstenite::accept_async(stream).await?;
    let (mut write, mut read) = ws.split();
    let mut pending_operations = VecDeque::new();
    let mut event_poll = tokio::time::interval(CORE_EVENT_POLL_INTERVAL);
    event_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let result = async {
        loop {
            tokio::select! {
                message = read.next() => {
                    let Some(message) = message else {
                        break;
                    };
                    let message = message?;
                    if message.is_text() || message.is_binary() {
                        let text = message.into_text().unwrap_or_default();
                        let parsed = parse_cdp_request(&text);
                        match parsed {
                            Ok(request) if is_async_navigation_method(&request.method) => {
                                let started = Instant::now();
                                let result = state.borrow_mut().start_socket_operation_with_capacity(
                                    &request,
                                    pending_operations.len() >= MAX_PENDING_SOCKET_NAVIGATIONS,
                                );
                                match result {
                                    Ok(Some(operation)) => pending_operations.push_back(PendingCdpOperation {
                                        request,
                                        started,
                                        deadline: Instant::now() + NAVIGATION_WAIT_TIMEOUT,
                                        operation,
                                    }),
                                    Ok(None) => {}
                                    Err(outcome) => {
                                        let lines = state
                                            .borrow_mut()
                                            .render_dispatch(&request, started, outcome);
                                        for line in lines {
                                            write.send(Message::text(line)).await?;
                                        }
                                    }
                                }
                            }
                            Ok(request) => {
                                let started = Instant::now();
                                let outcome = state.borrow_mut().dispatch(&request);
                                let lines = state
                                    .borrow_mut()
                                    .render_dispatch(&request, started, outcome);
                                for line in lines {
                                    write.send(Message::text(line)).await?;
                                }
                            }
                            Err(line) => write.send(Message::text(line)).await?,
                        }
                    } else if message.is_close() {
                        break;
                    }
                }
                _ = event_poll.tick(), if !pending_operations.is_empty() => {}
            }

            let completed = state
                .borrow_mut()
                .poll_socket_operations(&mut pending_operations);
            for lines in completed {
                for line in lines {
                    write.send(Message::text(line)).await?;
                }
            }
        }
        Ok::<(), BoxError>(())
    }
    .await;

    let mut state = state.borrow_mut();
    let _ = state.pump_core_events();
    for mut pending in pending_operations {
        let _ = state.adopt_active_socket_successor(&mut pending.operation);
        state.cancel_and_consume_socket_operation(&pending.operation);
    }
    let _ = state.pump_core_events();
    state.discard_pending_runtime_effects();
    result
}

/// Protocol presentation state. BrowserCore exclusively owns pages, runtimes,
/// histories, loading, and typed document/runtime generations.
pub struct CdpState {
    browser: EngineBrowserHandle,
    _profile: BrowserProfile,
    next_session_id: u64,
    targets: Vec<Target>,
    attached_sessions: Vec<TargetSession>,
    dispatch_context: Option<BrowsingContextId>,
    active_presentation_context: Option<BrowsingContextId>,
    context_presentations: HashMap<BrowsingContextId, ContextPresentation>,
    pending_core_navigations: HashMap<NavigationId, PendingCoreNavigation>,
    command_navigation_actions: Vec<NavigationActionOutcome>,
    defer_navigation_notifications: bool,
    pending_effects: VecDeque<PendingRuntimeEffects>,
    runtime_enabled: bool,
    network_enabled: bool,
    page_enabled: bool,
    lifecycle_events_enabled: bool,
    bypass_csp: bool,
    extra_http_headers: Vec<(String, String)>,
    cache_disabled: bool,
    log_enabled: bool,
    last_mouse_down: Option<MouseDownTarget>,
    last_mouse_over_node_id: Option<usize>,
    last_key_down_text: Option<String>,
    next_object_id: u64,
    remote_handles: VecDeque<RemoteHandle>,
    emulated_viewport: Option<(u32, u32)>,
    emulated_media: EmulatedMedia,
    next_new_document_script_id: u64,
    new_document_scripts: Vec<NewDocumentScript>,
    runtime_bindings: Vec<String>,
    isolated_world_name: Option<String>,
    download_behavior: DownloadBehavior,
    permission_grants: Vec<PermissionGrant>,
    tracing: TraceState,
    io_streams: HashMap<String, IoStream>,
    next_io_stream_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PermissionGrant {
    origin: Option<String>,
    runtime_permissions: Vec<String>,
}

const MAX_TRACE_EVENTS: usize = 4_096;
const TRACE_READ_CHUNK_BYTES: usize = 64 * 1024;

#[derive(Debug, Default)]
struct TraceState {
    active: bool,
    session_id: Option<String>,
    events: Vec<Value>,
    data_loss_occurred: bool,
}

#[derive(Debug)]
struct IoStream {
    bytes: Vec<u8>,
    offset: usize,
}

struct Target {
    context_id: BrowsingContextId,
    session_id: u64,
}

struct TargetSession {
    session_id: u64,
    context_id: BrowsingContextId,
}

#[derive(Clone, Default)]
struct ContextPresentation {
    pending_effects: VecDeque<PendingRuntimeEffects>,
    runtime_enabled: bool,
    network_enabled: bool,
    page_enabled: bool,
    lifecycle_events_enabled: bool,
    bypass_csp: bool,
    extra_http_headers: Vec<(String, String)>,
    cache_disabled: bool,
    log_enabled: bool,
    last_mouse_down: Option<MouseDownTarget>,
    last_mouse_over_node_id: Option<usize>,
    last_key_down_text: Option<String>,
    next_object_id: u64,
    remote_handles: VecDeque<RemoteHandle>,
    emulated_viewport: Option<(u32, u32)>,
    emulated_media: EmulatedMedia,
    next_new_document_script_id: u64,
    new_document_scripts: Vec<NewDocumentScript>,
    runtime_bindings: Vec<String>,
    isolated_world_name: Option<String>,
}

#[derive(Debug, Clone)]
struct NewDocumentScript {
    identifier: String,
    source: String,
}

#[derive(Clone)]
struct RemoteHandle {
    object_id: String,
    object_group: Option<String>,
}

#[derive(Clone)]
struct PendingRuntimeEffects {
    context_id: BrowsingContextId,
    frame_id: FrameId,
    document_id: DocumentId,
    runtime_context_id: RuntimeContextId,
    url: String,
    effects: RuntimeEffects,
}

struct PendingCoreNavigation {
    context_id: BrowsingContextId,
    predecessor_navigation_id: Option<NavigationId>,
    kind: CrossDocumentNavigationKind,
    committed: bool,
    terminal: Option<Result<(), String>>,
    wire_claimed: bool,
    abandoned: bool,
}

#[derive(Debug, Clone, Copy)]
enum PendingPageMethod {
    Navigate,
    Reload,
}

#[derive(Debug, Clone, Copy)]
struct StartedPageNavigation {
    method: PendingPageMethod,
    context_id: BrowsingContextId,
    navigation_id: NavigationId,
}

struct StartedHistoryTraversal {
    context_id: BrowsingContextId,
    navigation_id: Option<NavigationId>,
    before: BrowsingContextState,
}

struct PendingCdpOperation {
    request: CdpRequest,
    started: Instant,
    deadline: Instant,
    operation: StartedSocketOperation,
}

struct StartedSocketOperation {
    context_id: BrowsingContextId,
    navigation_actions: Vec<NavigationActionOutcome>,
    completion: SocketCompletion,
}

enum SocketCompletion {
    Page(StartedPageNavigation),
    TargetCreate,
    History { before: BrowsingContextState },
    Dispatch(CdpDispatch),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DownloadBehavior {
    policy: DownloadPolicy,
    download_path: Option<String>,
    events_enabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum DownloadPolicy {
    Deny,
    Allow,
    AllowAndName,
    #[default]
    Default,
}

#[derive(Debug, Clone)]
struct MouseDownTarget {
    node_id: usize,
    button: MouseButton,
}

struct RuntimeContextNotification<'a> {
    id: u64,
    name: &'a str,
    unique_prefix: &'a str,
    is_default: bool,
    context_type: &'a str,
    frame_id: &'a str,
    origin: &'a str,
    loader_id: u64,
    session_id: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
struct MouseDomEvent {
    node_id: usize,
    event_type: &'static str,
    related_node_id: Option<usize>,
    bubbles: bool,
}

#[derive(Debug, Clone, Default)]
struct EmulatedMedia {
    media_type: Option<String>,
    color_scheme: Option<String>,
}

#[derive(Deserialize)]
struct CdpDomRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseButton {
    None,
    Left,
    Middle,
    Right,
}

impl CdpState {
    pub fn new() -> Result<Self, String> {
        Self::new_with_profile(None)
    }

    pub fn with_profile_dir(profile_dir: &Path) -> Result<Self, String> {
        Self::new_with_profile(Some(profile_dir))
    }

    fn new_with_profile(profile_dir: Option<&Path>) -> Result<Self, String> {
        let profile = BrowserProfile::open(profile_dir, "vixen-cdp-", "CDP")?;
        let config = BrowserConfig::new(profile.database_path());
        Self::with_config_and_profile(config, profile)
    }

    fn with_config_and_profile(
        config: BrowserConfig,
        profile: BrowserProfile,
    ) -> Result<Self, String> {
        let browser = spawn_browser(config).map_err(|error| error.to_string())?;
        let mut state = Self {
            browser,
            _profile: profile,
            next_session_id: 0,
            targets: Vec::new(),
            attached_sessions: Vec::new(),
            dispatch_context: None,
            active_presentation_context: None,
            context_presentations: HashMap::new(),
            pending_core_navigations: HashMap::new(),
            command_navigation_actions: Vec::new(),
            defer_navigation_notifications: false,
            pending_effects: VecDeque::new(),
            runtime_enabled: false,
            network_enabled: false,
            page_enabled: false,
            lifecycle_events_enabled: false,
            bypass_csp: false,
            extra_http_headers: Vec::new(),
            cache_disabled: false,
            log_enabled: false,
            last_mouse_down: None,
            last_mouse_over_node_id: None,
            last_key_down_text: None,
            next_object_id: 0,
            remote_handles: VecDeque::new(),
            emulated_viewport: None,
            emulated_media: EmulatedMedia::default(),
            next_new_document_script_id: 0,
            new_document_scripts: Vec::new(),
            runtime_bindings: Vec::new(),
            isolated_world_name: None,
            download_behavior: DownloadBehavior::default(),
            permission_grants: Vec::new(),
            tracing: TraceState::default(),
            io_streams: HashMap::new(),
            next_io_stream_id: 0,
        };
        state.push_loaded_target("about:blank".to_owned())?;
        Ok(state)
    }

    /// Dispatch a single JSON request (synchronous — no await while state
    /// is borrowed). Returns outgoing lines: exactly one response followed by
    /// zero or more notifications caused by that response.
    pub fn handle_text_sync(&mut self, raw: &str) -> Vec<String> {
        let req = match parse_cdp_request(raw) {
            Ok(request) => request,
            Err(line) => return vec![line],
        };
        let started = Instant::now();
        let outcome = self.dispatch(&req);
        self.render_dispatch(&req, started, outcome)
    }

    fn render_dispatch(
        &mut self,
        req: &CdpRequest,
        started: Instant,
        outcome: CdpDispatch,
    ) -> Vec<String> {
        let succeeded = outcome.response.is_ok();
        self.record_trace_event(&req.method, req.session_id.as_deref(), started, succeeded);
        let resp = match outcome.response {
            Ok(result) => CdpResponse {
                id: req.id,
                session_id: req.session_id.clone(),
                result: Some(result),
                error: None,
            },
            Err(error) => CdpResponse {
                id: req.id,
                session_id: req.session_id.clone(),
                result: None,
                error: Some(error),
            },
        };
        let mut out = Vec::with_capacity(
            outcome.pre_response_notifications.len() + 1 + outcome.notifications.len(),
        );
        out.extend(outcome.pre_response_notifications);
        match serde_json::to_string(&resp) {
            Ok(s) => out.push(s),
            Err(e) => out.push(error_response(
                Some(req.id),
                CdpError::new(-32603, e.to_string()),
            )),
        }
        out.extend(outcome.notifications);
        out
    }

    /// Pure dispatch on the method name.
    fn dispatch(&mut self, req: &CdpRequest) -> CdpDispatch {
        if let Err(outcome) = self.prepare_dispatch(req) {
            return outcome;
        }
        let outcome = match req.method.as_str() {
            "Browser.getVersion" => CdpDispatch::ok(self.browser_get_version()),
            "Browser.close" => CdpDispatch::ok(json!({})),
            "Browser.setDownloadBehavior" => self.browser_set_download_behavior(req),
            "Browser.grantPermissions" => self.browser_grant_permissions(req),
            "Browser.resetPermissions" => self.browser_reset_permissions(req),
            "Target.createTarget" => self.target_create(req),
            "Target.closeTarget" => self.target_close(req),
            "Target.attachToTarget" => self.target_attach(req),
            "Target.detachFromTarget" => self.target_detach(req),
            "Target.attachToBrowserTarget" => self.target_attach_to_browser_target(),
            "Target.getTargets" => CdpDispatch::ok(self.target_get_targets()),
            "Target.getTargetInfo" => CdpDispatch::ok(self.target_get_target_info(req)),
            "Target.setDiscoverTargets" => self.target_set_discover_targets(req),
            "Target.setAutoAttach" => self.target_set_auto_attach(req),
            "Page.enable" => self.page_enable(req),
            "Page.disable" => {
                self.page_enabled = false;
                CdpDispatch::ok(json!({}))
            }
            "Page.navigate" => self.page_navigate(req),
            "Page.loadEventFired" => CdpDispatch::ok(json!({})),
            "Page.stopLoading" => self.page_stop_loading(req),
            "Page.reload" => self.page_reload(req),
            "Page.getNavigationHistory" => self.page_get_navigation_history(),
            "Page.resetNavigationHistory" => self.page_reset_navigation_history(req),
            "Page.navigateToHistoryEntry" => self.page_navigate_to_history_entry(req),
            "Page.captureScreenshot" => self.page_capture_screenshot(req),
            "Page.getLayoutMetrics" => {
                CdpDispatch::ok(page_get_layout_metrics(self.current_viewport()))
            }
            "Page.getFrameTree" => CdpDispatch::ok(self.page_get_frame_tree(req)),
            "Page.getResourceTree" => self.page_get_resource_tree(req),
            "Page.getResourceContent" => self.page_get_resource_content(req),
            "Page.addScriptToEvaluateOnNewDocument" => {
                self.page_add_script_to_evaluate_on_new_document(req)
            }
            "Page.removeScriptToEvaluateOnNewDocument" => {
                self.page_remove_script_to_evaluate_on_new_document(req)
            }
            "Page.handleJavaScriptDialog" => self.page_handle_javascript_dialog(req),
            "Page.createIsolatedWorld" => self.page_create_isolated_world(req),
            "Page.setLifecycleEventsEnabled" => self.page_set_lifecycle_events_enabled(req),
            "Page.setBypassCSP" => self.page_set_bypass_csp(req),
            "Page.bringToFront" => CdpDispatch::ok(json!({})),
            "Runtime.enable" => self.runtime_enable(req),
            "Runtime.disable" => {
                self.runtime_enabled = false;
                CdpDispatch::ok(json!({}))
            }
            "Runtime.evaluate" => self.runtime_evaluate(req),
            "Runtime.callFunctionOn" => self.runtime_call_function_on(req),
            "Runtime.getProperties" => self.runtime_get_properties(req),
            "Runtime.awaitPromise" => self.runtime_await_promise(req),
            "Runtime.addBinding" => self.runtime_add_binding(req),
            "Runtime.releaseObject" => self.runtime_release_object(req),
            "Runtime.releaseObjectGroup" => self.runtime_release_object_group(req),
            "Runtime.runIfWaitingForDebugger" => CdpDispatch::ok(json!({})),
            "Log.enable" => {
                self.log_enabled = true;
                CdpDispatch::ok(json!({}))
            }
            "Log.disable" => {
                self.log_enabled = false;
                CdpDispatch::ok(json!({}))
            }
            "Network.enable" => {
                self.network_enabled = true;
                CdpDispatch::ok(json!({}))
            }
            "Network.disable" => {
                self.network_enabled = false;
                CdpDispatch::ok(json!({}))
            }
            "Network.setCacheDisabled" => self.network_set_cache_disabled(req),
            "Network.setBypassServiceWorker" => CdpDispatch::ok(json!({})),
            "Network.setExtraHTTPHeaders" => self.network_set_extra_http_headers(req),
            "Performance.enable" | "Performance.disable" => CdpDispatch::ok(json!({})),
            "Performance.getMetrics" => self.performance_get_metrics(req),
            "Security.enable" | "Security.disable" => CdpDispatch::ok(json!({})),
            "Security.getSecurityState" => self.security_get_state(req),
            "Tracing.start" => self.tracing_start(req),
            "Tracing.end" => self.tracing_end(req),
            "Tracing.getCategories" => CdpDispatch::ok(json!({
                "categories": ["vixen.cdp", "devtools.timeline"]
            })),
            "IO.read" => self.io_read(req),
            "IO.close" => self.io_close(req),
            "DOM.enable"
            | "DOM.disable"
            | "Emulation.setTouchEmulationEnabled"
            | "Emulation.setFocusEmulationEnabled" => CdpDispatch::ok(json!({})),
            "Emulation.setDeviceMetricsOverride" => self.emulation_set_device_metrics_override(req),
            "Emulation.setEmulatedMedia" => self.emulation_set_emulated_media(req),
            "Emulation.clearDeviceMetricsOverride" => {
                self.emulated_viewport = None;
                match self.configure_current_context() {
                    Ok(()) => CdpDispatch::ok(json!({})),
                    Err(error) => CdpDispatch::error(-32603, error),
                }
            }
            "DOM.scrollIntoViewIfNeeded" => CdpDispatch::ok(json!({})),
            "DOM.getDocument" => self.dom_get_document(req),
            "DOM.querySelector" => self.dom_query_selector(req),
            "DOM.querySelectorAll" => self.dom_query_selector_all(req),
            "DOM.describeNode" => self.dom_describe_node(req),
            "DOM.resolveNode" => self.dom_resolve_node(req),
            "DOM.getAttributes" => self.dom_get_attributes(req),
            "DOM.getOuterHTML" => self.dom_get_outer_html(req),
            "DOM.setAttributeValue" => self.dom_set_attribute_value(req),
            "DOM.removeAttribute" => self.dom_remove_attribute(req),
            "DOM.getContentQuads" => self.dom_get_content_quads(req),
            "DOM.getBoxModel" => self.dom_get_box_model(req),
            "Input.dispatchMouseEvent" => self.input_dispatch_mouse_event(req),
            "Input.dispatchKeyEvent" => self.input_dispatch_key_event(req),
            "Input.insertText" => self.input_insert_text(req),
            _ => CdpDispatch::error(-32601, format!("method not found: {}", req.method)),
        };
        self.store_active_presentation();
        outcome
    }

    fn prepare_dispatch(&mut self, req: &CdpRequest) -> Result<(), CdpDispatch> {
        self.command_navigation_actions.clear();
        if req
            .session_id
            .as_deref()
            .is_some_and(|session_id| !self.is_known_session(session_id))
        {
            return Err(CdpDispatch::error(
                -32001,
                format!(
                    "{}: unknown or detached session `{}`",
                    req.method,
                    req.session_id.as_deref().unwrap_or_default()
                ),
            ));
        }
        self.dispatch_context = self
            .target_for_session(req.session_id.as_deref())
            .map(|target| target.context_id);
        if let Some(context_id) = self.dispatch_context {
            self.load_presentation(context_id);
        }
        Ok(())
    }

    fn start_socket_operation(
        &mut self,
        req: &CdpRequest,
    ) -> Result<Option<StartedSocketOperation>, CdpDispatch> {
        let operation = match req.method.as_str() {
            "Page.navigate" | "Page.reload" => {
                self.prepare_dispatch(req)?;
                let navigation = if req.method == "Page.navigate" {
                    self.start_page_navigate(req)?
                } else {
                    self.start_page_reload(req)?
                };
                StartedSocketOperation {
                    context_id: navigation.context_id,
                    navigation_actions: vec![NavigationActionOutcome::CrossDocument {
                        navigation_id: navigation.navigation_id,
                        kind: CrossDocumentNavigationKind::Regular,
                    }],
                    completion: SocketCompletion::Page(navigation),
                }
            }
            "Target.createTarget" => {
                self.prepare_dispatch(req)?;
                let (context_id, navigation_id) = self.start_target_create(req)?;
                let Some(navigation_id) = navigation_id else {
                    let dispatch = self.finish_target_create(req, context_id, Ok(false));
                    self.store_active_presentation();
                    return Err(dispatch);
                };
                StartedSocketOperation {
                    context_id,
                    navigation_actions: vec![NavigationActionOutcome::CrossDocument {
                        navigation_id,
                        kind: CrossDocumentNavigationKind::Regular,
                    }],
                    completion: SocketCompletion::TargetCreate,
                }
            }
            "Page.navigateToHistoryEntry" => {
                self.prepare_dispatch(req)?;
                let traversal = self.start_history_traversal(req)?;
                let Some(navigation_id) = traversal.navigation_id else {
                    let dispatch = self.finish_history_traversal(req, traversal.before, Ok(false));
                    self.store_active_presentation();
                    return Err(dispatch);
                };
                StartedSocketOperation {
                    context_id: traversal.context_id,
                    navigation_actions: vec![NavigationActionOutcome::CrossDocument {
                        navigation_id,
                        kind: CrossDocumentNavigationKind::Regular,
                    }],
                    completion: SocketCompletion::History {
                        before: traversal.before,
                    },
                }
            }
            _ => {
                self.defer_navigation_notifications = true;
                let mut dispatch = self.dispatch(req);
                self.defer_navigation_notifications = false;
                dispatch
                    .notifications
                    .append(&mut self.drain_side_effect_notifications(req));
                let navigation_actions = std::mem::take(&mut self.command_navigation_actions);
                let context_id = self.dispatch_context;
                if let Some(context_id) = context_id {
                    for action in &navigation_actions {
                        if let NavigationActionOutcome::SameDocument { url } = action {
                            dispatch
                                .notifications
                                .extend(self.same_document_notifications(
                                    context_id,
                                    url,
                                    req.session_id.as_deref(),
                                ));
                        }
                    }
                }
                let Some(navigation_id) = final_cross_document_id(&navigation_actions) else {
                    return Err(dispatch);
                };
                let context_id = self
                    .pending_core_navigations
                    .get(&navigation_id)
                    .map(|pending| pending.context_id)
                    .or(self.dispatch_context)
                    .ok_or_else(|| CdpDispatch::error(-32000, "navigation has no page target"))?;
                StartedSocketOperation {
                    context_id,
                    navigation_actions,
                    completion: SocketCompletion::Dispatch(dispatch),
                }
            }
        };
        for (navigation_id, kind) in cross_document_actions(&operation.navigation_actions) {
            self.pending_core_navigations
                .entry(navigation_id)
                .or_insert(PendingCoreNavigation {
                    context_id: operation.context_id,
                    predecessor_navigation_id: None,
                    kind,
                    committed: false,
                    terminal: None,
                    wire_claimed: true,
                    abandoned: false,
                })
                .wire_claimed = true;
        }
        self.prune_pending_core_navigations();
        self.store_active_presentation();
        Ok(Some(operation))
    }

    fn start_socket_operation_with_capacity(
        &mut self,
        req: &CdpRequest,
        at_capacity: bool,
    ) -> Result<Option<StartedSocketOperation>, CdpDispatch> {
        let operation = self.start_socket_operation(req)?;
        if at_capacity && let Some(operation) = operation {
            self.cancel_socket_operation(&operation);
            return Err(CdpDispatch::error(
                -32000,
                "too many pending navigation-producing requests",
            ));
        }
        Ok(operation)
    }

    fn poll_socket_operations(
        &mut self,
        pending_operations: &mut VecDeque<PendingCdpOperation>,
    ) -> Vec<Vec<String>> {
        let pump_error = self.pump_core_events().err();
        let now = Instant::now();
        let mut completed = Vec::new();
        for _ in 0..pending_operations.len() {
            let mut pending = pending_operations
                .pop_front()
                .expect("pending operation length is stable");
            let mut operation_error = self
                .adopt_active_socket_successor(&mut pending.operation)
                .err();
            let mut navigation_ids = cross_document_ids(&pending.operation.navigation_actions);
            if pump_error.is_none()
                && operation_error.is_none()
                && self.socket_navigation_ids_are_terminal(&navigation_ids)
            {
                operation_error = self
                    .context_state(pending.operation.context_id)
                    .and_then(|_| self.pump_core_events())
                    .and_then(|_| self.adopt_active_socket_successor(&mut pending.operation))
                    .err();
                navigation_ids = cross_document_ids(&pending.operation.navigation_actions);
            }
            let outcome = if let Some(error) = pump_error.as_ref() {
                self.cancel_and_consume_socket_operation(&pending.operation);
                Some(Err(error.clone()))
            } else if let Some(error) = operation_error {
                self.cancel_and_consume_socket_operation(&pending.operation);
                Some(Err(error))
            } else if let Some((committed, terminal)) =
                self.take_socket_operation_outcome(&navigation_ids)
            {
                Some(terminal.map(|()| committed))
            } else if now >= pending.deadline {
                self.cancel_and_consume_socket_operation(&pending.operation);
                Some(Err(format!(
                    "timed out waiting for navigation {} in context {}",
                    navigation_ids.last().expect("non-empty navigation IDs"),
                    pending.operation.context_id
                )))
            } else {
                None
            };

            let Some(outcome) = outcome else {
                pending_operations.push_back(pending);
                continue;
            };
            if self
                .context_presentations
                .contains_key(&pending.operation.context_id)
            {
                self.dispatch_context = Some(pending.operation.context_id);
                self.load_presentation(pending.operation.context_id);
            }
            let final_navigation_kind =
                final_cross_document_kind(&pending.operation.navigation_actions)
                    .unwrap_or(CrossDocumentNavigationKind::Regular);
            let mut dispatch = match pending.operation.completion {
                SocketCompletion::Page(navigation) => match navigation.method {
                    PendingPageMethod::Navigate => {
                        self.finish_page_navigate(&pending.request, navigation, outcome)
                    }
                    PendingPageMethod::Reload => {
                        self.finish_page_reload(&pending.request, navigation.context_id, outcome)
                    }
                },
                SocketCompletion::TargetCreate => self.finish_target_create(
                    &pending.request,
                    pending.operation.context_id,
                    outcome,
                ),
                SocketCompletion::History { before } => {
                    self.finish_history_traversal(&pending.request, before, outcome)
                }
                SocketCompletion::Dispatch(mut dispatch) => match outcome {
                    Ok(committed) => match self.navigation_notifications_after_completion(
                        pending.operation.context_id,
                        &pending.request,
                        committed,
                        final_navigation_kind,
                    ) {
                        Ok(mut notifications) => {
                            dispatch.notifications.append(&mut notifications);
                            dispatch
                        }
                        Err(error) => {
                            dispatch.response = Err(CdpError::new(-32603, error));
                            dispatch
                        }
                    },
                    Err(error) => {
                        dispatch.response = Err(CdpError::new(-32603, error));
                        dispatch
                    }
                },
            };
            dispatch
                .notifications
                .append(&mut self.drain_side_effect_notifications_for_context(
                    pending.operation.context_id,
                    &pending.request,
                ));
            self.store_active_presentation();
            completed.push(self.render_dispatch(&pending.request, pending.started, dispatch));
        }
        completed
    }

    fn socket_navigation_ids_are_terminal(&self, navigation_ids: &[NavigationId]) -> bool {
        !navigation_ids.is_empty()
            && navigation_ids.iter().all(|navigation_id| {
                self.pending_core_navigations
                    .get(navigation_id)
                    .is_some_and(|pending| pending.wire_claimed && pending.terminal.is_some())
            })
    }

    fn abandon_navigation_ids(&mut self, navigation_ids: &[NavigationId]) {
        for navigation_id in navigation_ids {
            if let Some(pending) = self.pending_core_navigations.get_mut(navigation_id)
                && pending.wire_claimed
            {
                pending.abandoned = true;
            }
        }
    }

    fn cancel_socket_operation(&mut self, operation: &StartedSocketOperation) {
        self.cancel_and_consume_socket_operation(operation);
    }

    fn cancel_and_consume_socket_operation(&mut self, operation: &StartedSocketOperation) {
        let _ = self.browser.dispatch(BrowserCommand::Stop {
            context_id: operation.context_id,
        });
        let _ = self.pump_core_events();
        let mut navigation_ids = cross_document_ids(&operation.navigation_actions);
        while let Some(successor_navigation_id) = self
            .pending_core_navigations
            .iter()
            .filter(|(_, pending)| {
                pending.context_id == operation.context_id
                    && pending
                        .predecessor_navigation_id
                        .is_some_and(|predecessor| navigation_ids.contains(&predecessor))
                    && !pending.wire_claimed
            })
            .map(|(navigation_id, _)| *navigation_id)
            .min()
        {
            self.pending_core_navigations
                .get_mut(&successor_navigation_id)
                .expect("successor exists")
                .wire_claimed = true;
            navigation_ids.push(successor_navigation_id);
        }
        for navigation_id in navigation_ids {
            let terminal = self
                .pending_core_navigations
                .get(&navigation_id)
                .is_some_and(|pending| pending.terminal.is_some());
            if terminal {
                self.pending_core_navigations.remove(&navigation_id);
            } else {
                self.abandon_navigation_ids(&[navigation_id]);
            }
        }
        if matches!(&operation.completion, SocketCompletion::TargetCreate) {
            self.remove_failed_target(operation.context_id);
        }
        self.prune_pending_core_navigations();
    }

    fn adopt_active_socket_successor(
        &mut self,
        operation: &mut StartedSocketOperation,
    ) -> Result<(), String> {
        loop {
            let navigation_ids = cross_document_ids(&operation.navigation_actions);
            if navigation_ids.iter().any(|navigation_id| {
                self.pending_core_navigations
                    .get(navigation_id)
                    .is_none_or(|pending| pending.terminal.is_none())
            }) {
                return Ok(());
            }
            let Some(predecessor_navigation_id) = navigation_ids.last().copied() else {
                return Ok(());
            };
            let Some(successor_navigation_id) = self
                .pending_core_navigations
                .iter()
                .filter(|(_, pending)| {
                    pending.context_id == operation.context_id
                        && pending.predecessor_navigation_id == Some(predecessor_navigation_id)
                })
                .map(|(navigation_id, _)| *navigation_id)
                .min()
            else {
                return Ok(());
            };
            if self.pending_core_navigations[&successor_navigation_id].wire_claimed {
                return Ok(());
            }
            if navigation_ids.len() >= MAX_PENDING_CORE_NAVIGATIONS {
                return Err("navigation successor limit exceeded".to_owned());
            }
            let Some(pending) = self
                .pending_core_navigations
                .get_mut(&successor_navigation_id)
                .filter(|pending| !pending.wire_claimed)
            else {
                return Ok(());
            };
            pending.wire_claimed = true;
            let kind = pending.kind;
            let terminal = pending.terminal.is_some();
            operation
                .navigation_actions
                .push(NavigationActionOutcome::CrossDocument {
                    navigation_id: successor_navigation_id,
                    kind,
                });
            if !terminal {
                // A successor created while lifecycle work settles is released
                // when BrowserCore handles its next command.
                let _ = self.context_state(operation.context_id);
                return Ok(());
            }
        }
    }

    // --- Method handlers ------------------------------------------------

    fn target_for_session(&self, session_id: Option<&str>) -> Option<&Target> {
        if let Some(session_id) = session_id {
            if let Some(target) = self.target_by_primary_session_id(session_id) {
                return Some(target);
            }
            if let Some(target) = self.target_by_attached_session_id(session_id) {
                return Some(target);
            }
        }
        self.targets.first()
    }

    fn is_known_session(&self, session_id: &str) -> bool {
        self.targets.is_empty()
            || session_id == "browser-session"
            || self.target_by_primary_session_id(session_id).is_some()
            || self.target_by_attached_session_id(session_id).is_some()
    }

    fn target_by_primary_session_id(&self, session_id: &str) -> Option<&Target> {
        let target_session_id = session_id.strip_prefix("sess-")?.parse::<u64>().ok()?;
        self.targets
            .iter()
            .find(|target| target.session_id == target_session_id)
    }

    fn target_by_attached_session_id(&self, session_id: &str) -> Option<&Target> {
        let target_session_id = session_id.strip_prefix("sess-")?.parse::<u64>().ok()?;
        let target_id = self
            .attached_sessions
            .iter()
            .find(|session| session.session_id == target_session_id)?
            .context_id;
        self.targets
            .iter()
            .find(|target| target.context_id == target_id)
    }

    fn context_for_session(&self, session_id: Option<&str>) -> Option<BrowsingContextId> {
        self.target_for_session(session_id)
            .map(|target| target.context_id)
    }

    fn notification_sessions(
        &self,
        context_id: BrowsingContextId,
        requested_session: Option<&str>,
    ) -> Vec<Option<String>> {
        if requested_session.is_none() {
            return vec![None];
        }
        let Some(target) = self
            .targets
            .iter()
            .find(|target| target.context_id == context_id)
        else {
            return Vec::new();
        };
        let mut sessions = vec![Some(format!("sess-{}", target.session_id))];
        sessions.extend(
            self.attached_sessions
                .iter()
                .filter(|session| session.context_id == context_id)
                .map(|session| Some(format!("sess-{}", session.session_id))),
        );
        if let Some(requested_session) = requested_session
            && !sessions
                .iter()
                .any(|session| session.as_deref() == Some(requested_session))
        {
            sessions.push(Some(requested_session.to_owned()));
        }
        sessions
    }

    fn store_active_presentation(&mut self) {
        let Some(context_id) = self.active_presentation_context else {
            return;
        };
        self.context_presentations.insert(
            context_id,
            ContextPresentation {
                runtime_enabled: self.runtime_enabled,
                pending_effects: self.pending_effects.clone(),
                network_enabled: self.network_enabled,
                page_enabled: self.page_enabled,
                lifecycle_events_enabled: self.lifecycle_events_enabled,
                bypass_csp: self.bypass_csp,
                extra_http_headers: self.extra_http_headers.clone(),
                cache_disabled: self.cache_disabled,
                log_enabled: self.log_enabled,
                last_mouse_down: self.last_mouse_down.clone(),
                last_mouse_over_node_id: self.last_mouse_over_node_id,
                last_key_down_text: self.last_key_down_text.clone(),
                next_object_id: self.next_object_id,
                remote_handles: self.remote_handles.clone(),
                emulated_viewport: self.emulated_viewport,
                emulated_media: self.emulated_media.clone(),
                next_new_document_script_id: self.next_new_document_script_id,
                new_document_scripts: self.new_document_scripts.clone(),
                runtime_bindings: self.runtime_bindings.clone(),
                isolated_world_name: self.isolated_world_name.clone(),
            },
        );
    }

    fn load_presentation(&mut self, context_id: BrowsingContextId) {
        self.store_active_presentation();
        let presentation = self
            .context_presentations
            .entry(context_id)
            .or_default()
            .clone();
        self.runtime_enabled = presentation.runtime_enabled;
        self.pending_effects = presentation.pending_effects;
        self.network_enabled = presentation.network_enabled;
        self.page_enabled = presentation.page_enabled;
        self.lifecycle_events_enabled = presentation.lifecycle_events_enabled;
        self.bypass_csp = presentation.bypass_csp;
        self.extra_http_headers = presentation.extra_http_headers;
        self.cache_disabled = presentation.cache_disabled;
        self.log_enabled = presentation.log_enabled;
        self.last_mouse_down = presentation.last_mouse_down;
        self.last_mouse_over_node_id = presentation.last_mouse_over_node_id;
        self.last_key_down_text = presentation.last_key_down_text;
        self.next_object_id = presentation.next_object_id;
        self.remote_handles = presentation.remote_handles;
        self.emulated_viewport = presentation.emulated_viewport;
        self.emulated_media = presentation.emulated_media;
        self.next_new_document_script_id = presentation.next_new_document_script_id;
        self.new_document_scripts = presentation.new_document_scripts;
        self.runtime_bindings = presentation.runtime_bindings;
        self.isolated_world_name = presentation.isolated_world_name;
        self.active_presentation_context = Some(context_id);
    }

    fn current_context(&self) -> Result<BrowsingContextId, String> {
        self.dispatch_context
            .or_else(|| self.targets.first().map(|target| target.context_id))
            .ok_or_else(|| "no page target".to_owned())
    }

    fn context_state(
        &mut self,
        context_id: BrowsingContextId,
    ) -> Result<BrowsingContextState, String> {
        match self
            .browser
            .dispatch(BrowserCommand::GetBrowsingContextState { context_id })
            .map_err(|error| error.to_string())?
        {
            BrowserCommandResult::BrowsingContextState(state) => Ok(state),
            result => Err(format!("unexpected context-state result: {result:?}")),
        }
    }

    fn current_state(&mut self) -> Result<BrowsingContextState, String> {
        let context_id = self.current_context()?;
        self.context_state(context_id)
    }

    fn query_selector_all(
        &mut self,
        selector: &str,
        viewport: (u32, u32),
    ) -> Result<Vec<vixen_api::ElementInfo>, String> {
        let state = self.current_state()?;
        match self
            .browser
            .dispatch(BrowserCommand::QuerySelectorAll {
                context_id: state.context_id,
                document_id: state.document_id,
                selector: selector.to_owned(),
                viewport,
            })
            .map_err(|error| error.to_string())?
        {
            BrowserCommandResult::SelectorMatches(elements) => Ok(elements),
            result => Err(format!("unexpected selector result: {result:?}")),
        }
    }

    fn element_for_node_id(
        &mut self,
        node_id: usize,
    ) -> Result<Option<vixen_api::ElementInfo>, String> {
        Ok(self
            .query_selector_all("*", self.current_viewport())?
            .into_iter()
            .find(|element| element.node_id == node_id))
    }

    fn configure_current_context(&mut self) -> Result<(), String> {
        let context_id = self.current_context()?;
        self.configure_context(context_id)
    }

    fn configure_context(&mut self, context_id: BrowsingContextId) -> Result<(), String> {
        self.store_active_presentation();
        let presentation = self
            .context_presentations
            .get(&context_id)
            .cloned()
            .unwrap_or_default();
        let viewport = presentation
            .emulated_viewport
            .unwrap_or(DEFAULT_CAPTURE_VIEWPORT);
        let preload_scripts = runtime_binding_install_scripts(&presentation.runtime_bindings)
            .into_iter()
            .chain(std::iter::once(emulation_override_script(
                viewport,
                &presentation.emulated_media,
            )))
            .collect();
        let config = BrowsingContextConfig {
            extra_http_headers: presentation.extra_http_headers,
            cache_disabled: presentation.cache_disabled,
            bypass_csp: presentation.bypass_csp,
            preload_scripts,
            new_document_scripts: presentation
                .new_document_scripts
                .iter()
                .map(|script| script.source.clone())
                .collect(),
            permission_grants: self
                .permission_grants
                .iter()
                .map(|grant| RuntimePermissionGrant {
                    origin: grant.origin.clone(),
                    permissions: grant.runtime_permissions.clone(),
                })
                .collect(),
        };
        match self
            .browser
            .dispatch(BrowserCommand::ConfigureBrowsingContext { context_id, config })
            .map_err(|error| error.to_string())?
        {
            BrowserCommandResult::Accepted => Ok(()),
            result => Err(format!("unexpected configure-context result: {result:?}")),
        }
    }

    fn drain_core_events(&mut self) {
        let _ = self.pump_core_events();
    }

    fn pump_core_events(&mut self) -> Result<(), String> {
        loop {
            match self.browser.try_next_event() {
                Ok(Some(event)) => self.record_core_event(event),
                Ok(None) => break,
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(())
    }

    fn wait_for_navigation(
        &mut self,
        context_id: BrowsingContextId,
        navigation_id: NavigationId,
    ) -> Result<bool, String> {
        let deadline = Instant::now() + NAVIGATION_WAIT_TIMEOUT;
        loop {
            if let Some((committed, terminal)) = self.take_navigation_outcome(navigation_id) {
                terminal?;
                return Ok(committed);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let Some(event) = self
                .browser
                .wait_next_event(remaining)
                .map_err(|error| error.to_string())?
            else {
                return Err(format!(
                    "timed out waiting for navigation {navigation_id} in context {context_id}"
                ));
            };
            self.record_core_event(event);
        }
    }

    fn record_core_event(&mut self, event: BrowserEvent) {
        match event {
            BrowserEvent::NavigationRequested {
                context_id,
                navigation_id,
                predecessor_navigation_id,
                kind,
                ..
            } => {
                let pending = self
                    .pending_core_navigations
                    .entry(navigation_id)
                    .or_insert(PendingCoreNavigation {
                        context_id,
                        predecessor_navigation_id,
                        kind,
                        committed: false,
                        terminal: None,
                        wire_claimed: false,
                        abandoned: false,
                    });
                pending.kind = kind;
                pending.predecessor_navigation_id = predecessor_navigation_id;
            }
            BrowserEvent::NavigationCommitted {
                context_id,
                navigation_id,
                ..
            } => {
                let pending = self
                    .pending_core_navigations
                    .entry(navigation_id)
                    .or_insert(PendingCoreNavigation {
                        context_id,
                        predecessor_navigation_id: None,
                        kind: CrossDocumentNavigationKind::Regular,
                        committed: false,
                        terminal: None,
                        wire_claimed: false,
                        abandoned: false,
                    });
                pending.committed = true;
            }
            BrowserEvent::NavigationPhaseChanged {
                context_id,
                navigation_id,
                phase: NavigationPhase::Settled,
                ..
            } => {
                self.pending_core_navigations
                    .entry(navigation_id)
                    .or_insert(PendingCoreNavigation {
                        context_id,
                        predecessor_navigation_id: None,
                        kind: CrossDocumentNavigationKind::Regular,
                        committed: false,
                        terminal: None,
                        wire_claimed: false,
                        abandoned: false,
                    })
                    .terminal = Some(Ok(()));
            }
            BrowserEvent::NavigationFailed {
                context_id,
                navigation_id,
                error,
                ..
            } => {
                self.pending_core_navigations
                    .entry(navigation_id)
                    .or_insert(PendingCoreNavigation {
                        context_id,
                        predecessor_navigation_id: None,
                        kind: CrossDocumentNavigationKind::Regular,
                        committed: false,
                        terminal: None,
                        wire_claimed: false,
                        abandoned: false,
                    })
                    .terminal = Some(Err(error.to_string()));
            }
            BrowserEvent::NavigationCancelled {
                context_id,
                navigation_id,
                reason,
                ..
            } => {
                self.pending_core_navigations
                    .entry(navigation_id)
                    .or_insert(PendingCoreNavigation {
                        context_id,
                        predecessor_navigation_id: None,
                        kind: CrossDocumentNavigationKind::Regular,
                        committed: false,
                        terminal: None,
                        wire_claimed: false,
                        abandoned: false,
                    })
                    .terminal = Some(Err(format!(
                    "navigation {navigation_id} was cancelled: {reason:?}"
                )));
            }
            BrowserEvent::RuntimeEffects {
                context_id,
                frame_id,
                document_id,
                runtime_context_id,
                url,
                effects,
            } => self.queue_runtime_effects_for_context(PendingRuntimeEffects {
                context_id,
                frame_id,
                document_id,
                runtime_context_id,
                url,
                effects,
            }),
            _ => {}
        }
        self.prune_pending_core_navigations();
    }

    fn prune_pending_core_navigations(&mut self) {
        while self.pending_core_navigations.len() > MAX_PENDING_CORE_NAVIGATIONS {
            let Some(oldest_terminal) = self
                .pending_core_navigations
                .iter()
                .filter(|(_, pending)| {
                    pending.terminal.is_some() && (!pending.wire_claimed || pending.abandoned)
                })
                .map(|(navigation_id, _)| *navigation_id)
                .min()
            else {
                break;
            };
            self.pending_core_navigations.remove(&oldest_terminal);
        }
    }

    fn take_navigation_outcome(
        &mut self,
        navigation_id: NavigationId,
    ) -> Option<(bool, Result<(), String>)> {
        let pending = self.pending_core_navigations.get(&navigation_id)?;
        if pending.wire_claimed {
            return None;
        }
        pending.terminal.as_ref()?;
        self.remove_navigation_outcome(navigation_id)
    }

    fn take_socket_navigation_outcome(
        &mut self,
        navigation_id: NavigationId,
    ) -> Option<(bool, Result<(), String>)> {
        let pending = self.pending_core_navigations.get(&navigation_id)?;
        if !pending.wire_claimed {
            return None;
        }
        pending.terminal.as_ref()?;
        self.remove_navigation_outcome(navigation_id)
    }

    fn take_socket_operation_outcome(
        &mut self,
        navigation_ids: &[NavigationId],
    ) -> Option<(bool, Result<(), String>)> {
        let (&final_navigation_id, superseded) = navigation_ids.split_last()?;
        if !navigation_ids.iter().all(|navigation_id| {
            self.pending_core_navigations
                .get(navigation_id)
                .is_some_and(|pending| pending.wire_claimed && pending.terminal.is_some())
        }) {
            return None;
        }
        for navigation_id in superseded {
            self.remove_navigation_outcome(*navigation_id);
        }
        self.take_socket_navigation_outcome(final_navigation_id)
    }

    fn remove_navigation_outcome(
        &mut self,
        navigation_id: NavigationId,
    ) -> Option<(bool, Result<(), String>)> {
        let pending = self
            .pending_core_navigations
            .remove(&navigation_id)
            .expect("pending navigation exists");
        Some((
            pending.committed,
            pending.terminal.expect("terminal outcome exists"),
        ))
    }

    fn queue_runtime_effects(
        &mut self,
        runtime_context_id: RuntimeContextId,
        effects: RuntimeEffects,
    ) {
        if effects.is_empty() {
            return;
        }
        let Some(context_id) = self
            .active_presentation_context
            .or(self.dispatch_context)
            .or_else(|| self.targets.first().map(|target| target.context_id))
        else {
            return;
        };
        let Ok(state) = self.context_state(context_id) else {
            return;
        };
        self.queue_runtime_effects_for_context(PendingRuntimeEffects {
            context_id: state.context_id,
            frame_id: state.main_frame_id,
            document_id: state.document_id,
            runtime_context_id,
            url: state.url,
            effects,
        });
    }

    fn queue_runtime_effects_for_context(&mut self, queued: PendingRuntimeEffects) {
        if queued.effects.is_empty() {
            return;
        }
        let context_id = queued.context_id;
        let same_generation = |pending: &PendingRuntimeEffects| {
            pending.context_id == queued.context_id
                && pending.frame_id == queued.frame_id
                && pending.document_id == queued.document_id
                && pending.runtime_context_id == queued.runtime_context_id
                && pending.url == queued.url
        };
        if self.active_presentation_context == Some(context_id) {
            if let Some(pending) = self.pending_effects.back_mut()
                && same_generation(pending)
            {
                pending.effects.extend(queued.effects);
            } else {
                self.pending_effects.push_back(queued);
            }
            return;
        }
        let pending = &mut self
            .context_presentations
            .entry(context_id)
            .or_default()
            .pending_effects;
        if let Some(previous) = pending.back_mut()
            && same_generation(previous)
        {
            previous.effects.extend(queued.effects);
        } else {
            pending.push_back(queued);
        }
    }

    fn discard_pending_runtime_effects(&mut self) {
        self.pending_effects.clear();
        for presentation in self.context_presentations.values_mut() {
            presentation.pending_effects.clear();
        }
    }

    fn browser_get_version(&self) -> Value {
        // Stable product string per CDP schema.
        json!({
            "protocolVersion": "1.3",
            "product": format!("Vixen/{}", env!("CARGO_PKG_VERSION")),
            "revision": "@vixen-headless@",
            "userAgent": format!("Vixen/{}", env!("CARGO_PKG_VERSION")),
            "jsVersion": "V8 (deno_core)"
        })
    }

    fn browser_set_download_behavior(&mut self, req: &CdpRequest) -> CdpDispatch {
        let Some(raw_behavior) = req.params.get("behavior").and_then(Value::as_str) else {
            return CdpDispatch::error(-32602, "Browser.setDownloadBehavior: missing `behavior`");
        };
        let policy = match raw_behavior {
            "deny" => DownloadPolicy::Deny,
            "allow" => DownloadPolicy::Allow,
            "allowAndName" => DownloadPolicy::AllowAndName,
            "default" => DownloadPolicy::Default,
            other => {
                return CdpDispatch::error(
                    -32602,
                    format!("Browser.setDownloadBehavior: unsupported behavior `{other}`"),
                );
            }
        };
        let download_path = req
            .params
            .get("downloadPath")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(ToOwned::to_owned);
        if matches!(policy, DownloadPolicy::Allow | DownloadPolicy::AllowAndName)
            && download_path.is_none()
        {
            return CdpDispatch::error(
                -32602,
                "Browser.setDownloadBehavior: `downloadPath` is required for allow behavior",
            );
        }
        let events_enabled = req
            .params
            .get("eventsEnabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.download_behavior = DownloadBehavior {
            policy,
            download_path,
            events_enabled,
        };
        CdpDispatch::ok(json!({}))
    }

    fn browser_grant_permissions(&mut self, req: &CdpRequest) -> CdpDispatch {
        if let Some(context_id) = req.params.get("browserContextId").and_then(Value::as_str)
            && context_id != "default"
        {
            return CdpDispatch::error(
                -32000,
                "Browser.grantPermissions: only the default browser context is supported",
            );
        }
        let Some(permissions) = req.params.get("permissions").and_then(Value::as_array) else {
            return CdpDispatch::error(-32602, "Browser.grantPermissions: missing `permissions`");
        };
        let origin = match req.params.get("origin").and_then(Value::as_str) {
            Some(origin) => match canonical_permission_origin(origin) {
                Ok(origin) => Some(origin),
                Err(message) => return CdpDispatch::error(-32602, message),
            },
            None => None,
        };
        let mut runtime_permissions = Vec::new();
        for permission in permissions {
            let Some(permission) = permission.as_str() else {
                return CdpDispatch::error(
                    -32602,
                    "Browser.grantPermissions: permission names must be strings",
                );
            };
            let Some(mapped) = runtime_permission_name(permission) else {
                if is_supported_cdp_permission(permission) {
                    continue;
                }
                return CdpDispatch::error(
                    -32602,
                    format!("Browser.grantPermissions: unsupported permission `{permission}`"),
                );
            };
            if !runtime_permissions.iter().any(|entry| entry == mapped) {
                runtime_permissions.push(mapped.to_owned());
            }
        }

        let grant = PermissionGrant {
            origin: origin.clone(),
            runtime_permissions: runtime_permissions.clone(),
        };
        if let Some(existing) = self
            .permission_grants
            .iter_mut()
            .find(|entry| entry.origin == origin)
        {
            *existing = grant;
        } else {
            self.permission_grants.push(grant);
        }
        let contexts = self
            .targets
            .iter()
            .map(|target| target.context_id)
            .collect::<Vec<_>>();
        for context_id in contexts {
            if let Err(error) = self.configure_context(context_id) {
                return CdpDispatch::error(-32603, error);
            }
        }
        CdpDispatch::ok(json!({}))
    }

    fn browser_reset_permissions(&mut self, req: &CdpRequest) -> CdpDispatch {
        if let Some(context_id) = req.params.get("browserContextId").and_then(Value::as_str)
            && context_id != "default"
        {
            return CdpDispatch::error(
                -32000,
                "Browser.resetPermissions: only the default browser context is supported",
            );
        }
        self.permission_grants.clear();
        let contexts = self
            .targets
            .iter()
            .map(|target| target.context_id)
            .collect::<Vec<_>>();
        for context_id in contexts {
            if let Err(error) = self.configure_context(context_id) {
                return CdpDispatch::error(-32603, error);
            }
        }
        CdpDispatch::ok(json!({}))
    }

    fn tracing_start(&mut self, req: &CdpRequest) -> CdpDispatch {
        if self.tracing.active {
            return CdpDispatch::error(-32000, "Tracing.start: tracing is already active");
        }
        let transfer_mode = req
            .params
            .get("transferMode")
            .and_then(Value::as_str)
            .unwrap_or("ReturnAsStream");
        if transfer_mode != "ReturnAsStream" {
            return CdpDispatch::error(-32000, "Tracing.start: only ReturnAsStream is supported");
        }
        if req
            .params
            .get("streamFormat")
            .and_then(Value::as_str)
            .is_some_and(|format| format != "json")
            || req
                .params
                .get("streamCompression")
                .and_then(Value::as_str)
                .is_some_and(|compression| compression != "none")
        {
            return CdpDispatch::error(
                -32000,
                "Tracing.start: only uncompressed JSON traces are supported",
            );
        }
        self.tracing = TraceState {
            active: true,
            session_id: req.session_id.clone(),
            events: Vec::new(),
            data_loss_occurred: false,
        };
        CdpDispatch::ok(json!({}))
    }

    fn tracing_end(&mut self, _req: &CdpRequest) -> CdpDispatch {
        if !self.tracing.active {
            return CdpDispatch::error(-32000, "Tracing.end: tracing is not active");
        }
        self.tracing.active = false;
        self.next_io_stream_id = self.next_io_stream_id.saturating_add(1);
        let handle = format!("vixen-trace-{}", self.next_io_stream_id);
        let payload = json!({
            "traceEvents": self.tracing.events,
            "metadata": {
                "product": format!("Vixen/{}", env!("CARGO_PKG_VERSION")),
            }
        });
        let bytes =
            serde_json::to_vec(&payload).unwrap_or_else(|_| b"{\"traceEvents\":[]}".to_vec());
        self.io_streams
            .insert(handle.clone(), IoStream { bytes, offset: 0 });
        let notification_session = self.tracing.session_id.clone();
        let data_loss_occurred = self.tracing.data_loss_occurred;
        self.tracing.events.clear();
        CdpDispatch::ok_with_notifications(
            json!({}),
            vec![notification(
                "Tracing.tracingComplete",
                json!({
                    "dataLossOccurred": data_loss_occurred,
                    "stream": handle,
                    "traceFormat": "json",
                    "streamCompression": "none",
                }),
                notification_session.as_deref(),
            )],
        )
    }

    fn io_read(&mut self, req: &CdpRequest) -> CdpDispatch {
        let Some(handle) = req.params.get("handle").and_then(Value::as_str) else {
            return CdpDispatch::error(-32602, "IO.read: missing `handle`");
        };
        let Some(stream) = self.io_streams.get_mut(handle) else {
            return CdpDispatch::error(-32000, "IO.read: unknown stream handle");
        };
        let requested = req
            .params
            .get("size")
            .and_then(Value::as_u64)
            .and_then(|size| usize::try_from(size).ok())
            .filter(|size| *size > 0)
            .unwrap_or(TRACE_READ_CHUNK_BYTES)
            .min(TRACE_READ_CHUNK_BYTES);
        let end = stream
            .offset
            .saturating_add(requested)
            .min(stream.bytes.len());
        let data = BASE64_STANDARD.encode(&stream.bytes[stream.offset..end]);
        stream.offset = end;
        CdpDispatch::ok(json!({
            "base64Encoded": true,
            "data": data,
            "eof": stream.offset >= stream.bytes.len(),
        }))
    }

    fn io_close(&mut self, req: &CdpRequest) -> CdpDispatch {
        let Some(handle) = req.params.get("handle").and_then(Value::as_str) else {
            return CdpDispatch::error(-32602, "IO.close: missing `handle`");
        };
        if self.io_streams.remove(handle).is_none() {
            return CdpDispatch::error(-32000, "IO.close: unknown stream handle");
        }
        CdpDispatch::ok(json!({}))
    }

    fn page_stop_loading(&mut self, req: &CdpRequest) -> CdpDispatch {
        let Some(context_id) = self.context_for_session(req.session_id.as_deref()) else {
            return CdpDispatch::error(-32000, "Page.stopLoading: no page target");
        };
        match self.browser.dispatch(BrowserCommand::Stop { context_id }) {
            Ok(BrowserCommandResult::Accepted) => CdpDispatch::ok(json!({})),
            Ok(result) => CdpDispatch::error(
                -32603,
                format!("unexpected stop-loading result: {result:?}"),
            ),
            Err(error) => CdpDispatch::error(-32603, error.to_string()),
        }
    }

    fn target_create(&mut self, req: &CdpRequest) -> CdpDispatch {
        let (context_id, navigation_id) = match self.start_target_create(req) {
            Ok(started) => started,
            Err(error) => return error,
        };
        let outcome = navigation_id
            .map(|navigation_id| self.wait_for_navigation(context_id, navigation_id))
            .unwrap_or(Ok(false));
        self.finish_target_create(req, context_id, outcome)
    }

    fn start_target_create(
        &mut self,
        req: &CdpRequest,
    ) -> Result<(BrowsingContextId, Option<NavigationId>), CdpDispatch> {
        let url = req
            .params
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("about:blank")
            .to_owned();
        self.push_target(url)
            .map_err(|error| CdpDispatch::error(-32602, error))
    }

    fn finish_target_create(
        &mut self,
        req: &CdpRequest,
        context_id: BrowsingContextId,
        outcome: Result<bool, String>,
    ) -> CdpDispatch {
        if let Err(error) = outcome {
            self.remove_failed_target(context_id);
            return CdpDispatch::error(-32603, error);
        }
        let target = self
            .targets
            .iter()
            .find(|target| target.context_id == context_id)
            .expect("created target is stored");
        let session_id = target.session_id;
        let state = match self.context_state(context_id) {
            Ok(state) => state,
            Err(error) => {
                self.remove_failed_target(context_id);
                return CdpDispatch::error(-32603, error);
            }
        };
        CdpDispatch::ok_with_pre_response_notifications(
            json!({ "targetId": cdp_target_id(context_id) }),
            vec![target_attached_notification(
                context_id,
                session_id,
                &state,
                req.session_id.as_deref(),
            )],
        )
    }

    fn target_close(&mut self, req: &CdpRequest) -> CdpDispatch {
        let Some(target_id) = req.params.get("targetId").and_then(Value::as_str) else {
            return CdpDispatch::error(-32602, "Target.closeTarget: missing `targetId`");
        };
        let Some(index) = self
            .targets
            .iter()
            .position(|target| cdp_target_id(target.context_id) == target_id)
        else {
            return CdpDispatch::ok(json!({ "success": false }));
        };
        let context_id = self.targets[index].context_id;
        if let Err(error) = self
            .browser
            .dispatch(BrowserCommand::CloseBrowsingContext { context_id })
        {
            return CdpDispatch::error(-32603, error.to_string());
        }
        self.drain_core_events();
        let target = self.targets.remove(index);
        self.context_presentations.remove(&context_id);
        if self.active_presentation_context == Some(context_id) {
            self.active_presentation_context = None;
        }
        self.attached_sessions
            .retain(|session| session.context_id != target.context_id);
        if index == 0 {
            self.reset_input_for_navigation();
        }
        CdpDispatch::ok_with_notifications(
            json!({ "success": true }),
            vec![target_detached_notification(
                &target,
                req.session_id.as_deref(),
            )],
        )
    }

    fn target_attach(&mut self, req: &CdpRequest) -> CdpDispatch {
        let Some(target_id) = req.params.get("targetId").and_then(Value::as_str) else {
            return CdpDispatch::error(-32602, "Target.attachToTarget: missing `targetId`");
        };
        let Some(target_id) = self
            .targets
            .iter()
            .find(|target| cdp_target_id(target.context_id) == target_id)
            .map(|target| target.context_id)
        else {
            return CdpDispatch::error(-32602, "Target.attachToTarget: unknown `targetId`");
        };
        self.next_session_id += 1;
        let session_id = self.next_session_id;
        self.attached_sessions.push(TargetSession {
            session_id,
            context_id: target_id,
        });
        CdpDispatch::ok(json!({ "sessionId": format!("sess-{session_id}") }))
    }

    fn target_detach(&mut self, req: &CdpRequest) -> CdpDispatch {
        let Some(session_id) = req
            .params
            .get("sessionId")
            .and_then(Value::as_str)
            .or(req.session_id.as_deref())
        else {
            return CdpDispatch::error(-32602, "Target.detachFromTarget: missing `sessionId`");
        };
        if session_id == "browser-session" {
            return CdpDispatch::ok(json!({}));
        }
        let Some(detached_session_id) = session_id
            .strip_prefix("sess-")
            .and_then(|session_id| session_id.parse::<u64>().ok())
        else {
            return CdpDispatch::error(-32602, "Target.detachFromTarget: unknown `sessionId`");
        };
        let Some(target) = self.target_for_session(Some(session_id)) else {
            return CdpDispatch::error(-32602, "Target.detachFromTarget: unknown `sessionId`");
        };
        let target_id = target.context_id;
        let primary_session_id = target.session_id;
        if detached_session_id != primary_session_id {
            self.attached_sessions
                .retain(|session| session.session_id != detached_session_id);
        }
        let Some(target) = self
            .targets
            .iter()
            .find(|target| target.context_id == target_id)
        else {
            return CdpDispatch::error(-32602, "Target.detachFromTarget: unknown `sessionId`");
        };
        CdpDispatch::ok_with_notifications(
            json!({}),
            vec![target_detached_notification_for_session(
                target,
                detached_session_id,
                req.session_id.as_deref(),
            )],
        )
    }

    fn target_attach_to_browser_target(&self) -> CdpDispatch {
        CdpDispatch::ok(json!({ "sessionId": "browser-session" }))
    }

    fn target_get_targets(&mut self) -> Value {
        let contexts = self
            .targets
            .iter()
            .map(|target| target.context_id)
            .collect::<Vec<_>>();
        let target_infos = contexts
            .into_iter()
            .filter_map(|context_id| {
                self.context_state(context_id)
                    .ok()
                    .map(|state| target_info_json(context_id, &state))
            })
            .collect::<Vec<_>>();
        json!({ "targetInfos": target_infos })
    }

    fn target_get_target_info(&mut self, req: &CdpRequest) -> Value {
        let requested = req.params.get("targetId").and_then(Value::as_str);
        let target = requested
            .and_then(|target_id| {
                self.targets
                    .iter()
                    .find(|target| cdp_target_id(target.context_id) == target_id)
            })
            .or_else(|| self.targets.first())
            .map(|target| target.context_id);
        json!({
            "targetInfo": target.and_then(|context_id| self.context_state(context_id).ok().map(|state| target_info_json(context_id, &state))).unwrap_or_else(|| json!({
                "targetId": "tab-0",
                "type": "page",
                "title": "",
                "url": "about:blank",
                "attached": false,
                "canAccessOpener": false,
                "browserContextId": "default",
            }))
        })
    }

    fn target_set_discover_targets(&mut self, req: &CdpRequest) -> CdpDispatch {
        let discover = req
            .params
            .get("discover")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !discover {
            return CdpDispatch::ok(json!({}));
        }
        let contexts = self
            .targets
            .iter()
            .map(|target| target.context_id)
            .collect::<Vec<_>>();
        let notifications = contexts
            .into_iter()
            .filter_map(|context_id| {
                self.context_state(context_id).ok().map(|state| {
                    notification(
                        "Target.targetCreated",
                        json!({ "targetInfo": target_info_json(context_id, &state) }),
                        None,
                    )
                })
            })
            .collect();
        CdpDispatch::ok_with_notifications(json!({}), notifications)
    }

    fn target_set_auto_attach(&mut self, req: &CdpRequest) -> CdpDispatch {
        if req.session_id.is_some() {
            return CdpDispatch::ok(json!({}));
        }
        let auto_attach = req
            .params
            .get("autoAttach")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !auto_attach {
            return CdpDispatch::ok(json!({}));
        }
        let targets = self
            .targets
            .iter()
            .map(|target| (target.context_id, target.session_id))
            .collect::<Vec<_>>();
        let notifications = targets
            .into_iter()
            .filter_map(|(context_id, session_id)| {
                self.context_state(context_id).ok().map(|state| {
                    target_attached_notification(
                        context_id,
                        session_id,
                        &state,
                        req.session_id.as_deref(),
                    )
                })
            })
            .collect();
        CdpDispatch::ok_with_notifications(json!({}), notifications)
    }

    fn page_enable(&mut self, req: &CdpRequest) -> CdpDispatch {
        if self.page_enabled {
            return CdpDispatch::ok(json!({}));
        }
        self.page_enabled = true;
        let Some(context_id) = self.context_for_session(req.session_id.as_deref()) else {
            return CdpDispatch::ok(json!({}));
        };
        let state = match self.context_state(context_id) {
            Ok(state) => state,
            Err(error) => return CdpDispatch::error(-32603, error),
        };
        let frame_id = frame_id(&state);
        CdpDispatch::ok_with_notifications(
            json!({}),
            vec![
                notification(
                    "Page.frameStartedLoading",
                    json!({ "frameId": frame_id }),
                    req.session_id.as_deref(),
                ),
                notification(
                    "Page.frameStoppedLoading",
                    json!({ "frameId": &frame_id }),
                    req.session_id.as_deref(),
                ),
                notification(
                    "Page.frameNavigated",
                    json!({ "frame": frame_json(&state) }),
                    req.session_id.as_deref(),
                ),
            ],
        )
    }

    fn page_set_lifecycle_events_enabled(&mut self, req: &CdpRequest) -> CdpDispatch {
        self.lifecycle_events_enabled = req
            .params
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        CdpDispatch::ok(json!({}))
    }

    fn page_set_bypass_csp(&mut self, req: &CdpRequest) -> CdpDispatch {
        self.bypass_csp = req
            .params
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        match self.configure_current_context() {
            Ok(()) => CdpDispatch::ok(json!({})),
            Err(error) => CdpDispatch::error(-32603, error),
        }
    }

    fn network_set_extra_http_headers(&mut self, req: &CdpRequest) -> CdpDispatch {
        let Some(headers) = req.params.get("headers").and_then(Value::as_object) else {
            return CdpDispatch::error(-32602, "Network.setExtraHTTPHeaders: missing `headers`");
        };
        let mut parsed = Vec::with_capacity(headers.len());
        for (name, value) in headers {
            let Some(value) = value.as_str() else {
                return CdpDispatch::error(
                    -32602,
                    format!("Network.setExtraHTTPHeaders: header `{name}` must be a string"),
                );
            };
            let name = match validate_header_name(name) {
                Ok(name) => name,
                Err(err) => {
                    return CdpDispatch::error(
                        -32602,
                        format!("Network.setExtraHTTPHeaders: {err}"),
                    );
                }
            };
            let value = match validate_header_value(value) {
                Ok(value) => value,
                Err(err) => {
                    return CdpDispatch::error(
                        -32602,
                        format!("Network.setExtraHTTPHeaders: {err}"),
                    );
                }
            };
            parsed.push((name, value));
        }
        self.extra_http_headers = parsed.clone();
        if let Err(error) = self.configure_current_context() {
            return CdpDispatch::error(-32603, error);
        }
        CdpDispatch::ok(json!({}))
    }

    fn network_set_cache_disabled(&mut self, req: &CdpRequest) -> CdpDispatch {
        self.cache_disabled = req
            .params
            .get("cacheDisabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if let Err(error) = self.configure_current_context() {
            return CdpDispatch::error(-32603, error);
        }
        CdpDispatch::ok(json!({}))
    }

    fn page_navigate(&mut self, req: &CdpRequest) -> CdpDispatch {
        let navigation = match self.start_page_navigate(req) {
            Ok(navigation) => navigation,
            Err(outcome) => return outcome,
        };
        let outcome = self.wait_for_navigation(navigation.context_id, navigation.navigation_id);
        self.finish_page_navigate(req, navigation, outcome)
    }

    fn start_page_navigate(
        &mut self,
        req: &CdpRequest,
    ) -> Result<StartedPageNavigation, CdpDispatch> {
        let Some(url) = req.params.get("url").and_then(Value::as_str) else {
            return Err(CdpDispatch::error(-32602, "Page.navigate: missing `url`"));
        };
        let Some(context_id) = self.context_for_session(req.session_id.as_deref()) else {
            return Err(CdpDispatch::error(-32000, "Page.navigate: no page target"));
        };
        if let Err(error) = self.configure_current_context() {
            return Err(CdpDispatch::error(-32603, error));
        }
        self.drain_core_events();
        let navigation_id = match self.browser.dispatch(BrowserCommand::Navigate {
            context_id,
            url: url.to_owned(),
        }) {
            Ok(BrowserCommandResult::NavigationAccepted { navigation_id }) => navigation_id,
            Ok(result) => {
                return Err(CdpDispatch::error(
                    -32603,
                    format!("unexpected navigation result: {result:?}"),
                ));
            }
            Err(error) => return Err(CdpDispatch::error(-32602, error.to_string())),
        };
        Ok(StartedPageNavigation {
            method: PendingPageMethod::Navigate,
            context_id,
            navigation_id,
        })
    }

    fn finish_page_navigate(
        &mut self,
        req: &CdpRequest,
        navigation: StartedPageNavigation,
        outcome: Result<bool, String>,
    ) -> CdpDispatch {
        if let Err(error) = outcome {
            return CdpDispatch::error(-32603, error);
        }
        self.reset_input_for_navigation();
        let state = match self.context_state(navigation.context_id) {
            Ok(state) => state,
            Err(error) => return CdpDispatch::error(-32603, error),
        };
        let mut notifications =
            self.current_page_load_notifications(navigation.context_id, req.session_id.as_deref());
        notifications.extend(self.drain_side_effect_notifications(req));
        CdpDispatch::ok_with_notifications(
            json!({ "frameId": frame_id(&state), "loaderId": loader_id(&state), "navigationId": navigation.navigation_id.to_string() }),
            notifications,
        )
    }

    fn page_capture_screenshot(&mut self, req: &CdpRequest) -> CdpDispatch {
        if let Some(format) = req.params.get("format").and_then(Value::as_str)
            && !format.eq_ignore_ascii_case("png")
        {
            return CdpDispatch::error(-32602, "Page.captureScreenshot: only png is supported");
        }
        let viewport = match capture_viewport(&req.params, self.current_viewport()) {
            Ok(viewport) => viewport,
            Err(err) => return CdpDispatch::error(-32602, err),
        };
        let state = match self.current_state() {
            Ok(state) => state,
            Err(error) => {
                return CdpDispatch::error(-32000, format!("Page.captureScreenshot: {error}"));
            }
        };
        let paint =
            match self
                .browser
                .capture_paint_snapshot(state.context_id, state.document_id, viewport)
            {
                Ok(paint) => paint,
                Err(error) => return CdpDispatch::error(-32603, error.to_string()),
            };
        match crate::capture_commands_png(&paint.commands, viewport) {
            Ok(png) => CdpDispatch::ok(json!({ "data": BASE64_STANDARD.encode(png) })),
            Err(err) => {
                CdpDispatch::error(-32603, format!("{}: {err}", codes::UNSUPPORTED_SCREENSHOT))
            }
        }
    }

    fn page_reload(&mut self, req: &CdpRequest) -> CdpDispatch {
        let navigation = match self.start_page_reload(req) {
            Ok(navigation) => navigation,
            Err(outcome) => return outcome,
        };
        let outcome = self.wait_for_navigation(navigation.context_id, navigation.navigation_id);
        self.finish_page_reload(req, navigation.context_id, outcome)
    }

    fn start_page_reload(
        &mut self,
        req: &CdpRequest,
    ) -> Result<StartedPageNavigation, CdpDispatch> {
        let Some(context_id) = self.context_for_session(req.session_id.as_deref()) else {
            return Err(CdpDispatch::error(-32000, "Page.reload: no page loaded"));
        };
        if let Err(error) = self.configure_current_context() {
            return Err(CdpDispatch::error(-32603, error));
        }
        self.drain_core_events();
        let navigation_id = match self.browser.dispatch(BrowserCommand::Reload { context_id }) {
            Ok(BrowserCommandResult::NavigationAccepted { navigation_id }) => navigation_id,
            Ok(result) => {
                return Err(CdpDispatch::error(
                    -32603,
                    format!("unexpected reload result: {result:?}"),
                ));
            }
            Err(error) => return Err(CdpDispatch::error(-32603, error.to_string())),
        };
        Ok(StartedPageNavigation {
            method: PendingPageMethod::Reload,
            context_id,
            navigation_id,
        })
    }

    fn finish_page_reload(
        &mut self,
        req: &CdpRequest,
        context_id: BrowsingContextId,
        outcome: Result<bool, String>,
    ) -> CdpDispatch {
        if let Err(error) = outcome {
            return CdpDispatch::error(-32603, error);
        }
        self.reset_input_for_navigation();
        CdpDispatch::ok_with_notifications(
            json!({}),
            self.current_page_load_notifications(context_id, req.session_id.as_deref()),
        )
    }

    fn page_get_navigation_history(&mut self) -> CdpDispatch {
        let Ok(context_id) = self.current_context() else {
            return CdpDispatch::ok(json!({ "currentIndex": 0, "entries": [] }));
        };
        let history = match self
            .browser
            .dispatch(BrowserCommand::GetNavigationHistory { context_id })
        {
            Ok(BrowserCommandResult::NavigationHistory(history)) => history,
            Ok(result) => {
                return CdpDispatch::error(
                    -32603,
                    format!("unexpected history result: {result:?}"),
                );
            }
            Err(error) => return CdpDispatch::error(-32603, error.to_string()),
        };
        let entries = history
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                json!({
                    "id": index + 1,
                    "url": entry.url.clone(),
                    "userTypedURL": entry.url.clone(),
                    "title": entry.title.clone().unwrap_or_default(),
                    "transitionType": "typed",
                })
            })
            .collect::<Vec<_>>();
        CdpDispatch::ok(json!({
            "currentIndex": history.current_index,
            "entries": entries,
        }))
    }

    fn page_reset_navigation_history(&mut self, req: &CdpRequest) -> CdpDispatch {
        let Some(context_id) = self.context_for_session(req.session_id.as_deref()) else {
            return CdpDispatch::error(-32000, "Page.resetNavigationHistory: no page loaded");
        };
        match self
            .browser
            .dispatch(BrowserCommand::ResetNavigationHistory { context_id })
        {
            Ok(BrowserCommandResult::Accepted) => CdpDispatch::ok(json!({})),
            Ok(result) => CdpDispatch::error(
                -32603,
                format!("unexpected reset-history result: {result:?}"),
            ),
            Err(error) => CdpDispatch::error(-32603, error.to_string()),
        }
    }

    fn page_navigate_to_history_entry(&mut self, req: &CdpRequest) -> CdpDispatch {
        let traversal = match self.start_history_traversal(req) {
            Ok(traversal) => traversal,
            Err(error) => return error,
        };
        let outcome = traversal
            .navigation_id
            .map(|navigation_id| self.wait_for_navigation(traversal.context_id, navigation_id))
            .unwrap_or(Ok(false));
        self.finish_history_traversal(req, traversal.before, outcome)
    }

    fn start_history_traversal(
        &mut self,
        req: &CdpRequest,
    ) -> Result<StartedHistoryTraversal, CdpDispatch> {
        let entry_id = match req.params.get("entryId").and_then(Value::as_u64) {
            Some(entry_id) if entry_id > 0 => entry_id as usize,
            _ => {
                return Err(CdpDispatch::error(
                    -32602,
                    "Page.navigateToHistoryEntry: missing `entryId`",
                ));
            }
        };
        let Some(context_id) = self.context_for_session(req.session_id.as_deref()) else {
            return Err(CdpDispatch::error(
                -32000,
                "Page.navigateToHistoryEntry: no page loaded",
            ));
        };
        let history = match self
            .browser
            .dispatch(BrowserCommand::GetNavigationHistory { context_id })
        {
            Ok(BrowserCommandResult::NavigationHistory(history)) => history,
            Ok(result) => {
                return Err(CdpDispatch::error(
                    -32603,
                    format!("unexpected history result: {result:?}"),
                ));
            }
            Err(error) => return Err(CdpDispatch::error(-32603, error.to_string())),
        };
        if entry_id > history.entries.len() {
            return Err(CdpDispatch::error(
                -32602,
                "Page.navigateToHistoryEntry: unknown `entryId`",
            ));
        }
        let target_index = entry_id - 1;
        let delta = target_index as i32 - history.current_index as i32;
        let before = self
            .context_state(context_id)
            .map_err(|error| CdpDispatch::error(-32603, error))?;
        if delta == 0 {
            return Ok(StartedHistoryTraversal {
                context_id,
                navigation_id: None,
                before,
            });
        }
        self.drain_core_events();
        let navigation_id = match self
            .browser
            .dispatch(BrowserCommand::TraverseHistory { context_id, delta })
        {
            Ok(BrowserCommandResult::Accepted) => None,
            Ok(BrowserCommandResult::NavigationAccepted { navigation_id }) => Some(navigation_id),
            Ok(result) => {
                return Err(CdpDispatch::error(
                    -32603,
                    format!("unexpected history-traversal result: {result:?}"),
                ));
            }
            Err(error) => return Err(CdpDispatch::error(-32603, error.to_string())),
        };
        Ok(StartedHistoryTraversal {
            context_id,
            navigation_id,
            before,
        })
    }

    fn finish_history_traversal(
        &mut self,
        req: &CdpRequest,
        before: BrowsingContextState,
        outcome: Result<bool, String>,
    ) -> CdpDispatch {
        let committed = match outcome {
            Ok(committed) => committed,
            Err(error) => return CdpDispatch::error(-32603, error),
        };
        let after = match self.context_state(before.context_id) {
            Ok(state) => state,
            Err(error) => return CdpDispatch::error(-32603, error),
        };
        let notifications = if committed {
            self.reset_input_for_navigation();
            self.current_page_load_notifications(before.context_id, req.session_id.as_deref())
        } else if after.url != before.url {
            self.same_document_notifications(
                before.context_id,
                &after.url,
                req.session_id.as_deref(),
            )
        } else {
            Vec::new()
        };
        CdpDispatch::ok_with_notifications(json!({}), notifications)
    }

    fn page_get_frame_tree(&mut self, req: &CdpRequest) -> Value {
        let frame = self
            .context_for_session(req.session_id.as_deref())
            .and_then(|context_id| self.context_state(context_id).ok())
            .map(|state| frame_json(&state))
            .unwrap_or_else(|| {
                json!({
                    "id": "tab-0",
                    "loaderId": "loader-0",
                    "url": "about:blank",
                    "securityOrigin": "://",
                    "mimeType": "text/html",
                })
            });
        json!({ "frameTree": { "frame": frame } })
    }

    fn page_get_resource_tree(&mut self, req: &CdpRequest) -> CdpDispatch {
        let Some(context_id) = self.context_for_session(req.session_id.as_deref()) else {
            return CdpDispatch::error(-32000, "Page.getResourceTree: no page loaded");
        };
        let state = match self.context_state(context_id) {
            Ok(state) => state,
            Err(error) => return CdpDispatch::error(-32603, error),
        };
        CdpDispatch::ok(json!({
            "frameTree": {
                "frame": frame_json(&state),
                "resources": [],
            }
        }))
    }

    fn page_get_resource_content(&mut self, req: &CdpRequest) -> CdpDispatch {
        let requested_url = req.params.get("url").and_then(Value::as_str);
        let Some(context_id) = self.context_for_session(req.session_id.as_deref()) else {
            return CdpDispatch::error(-32000, "Page.getResourceContent: no page loaded");
        };
        let state = match self.context_state(context_id) {
            Ok(state) => state,
            Err(error) => return CdpDispatch::error(-32603, error),
        };
        if let Some(requested_url) = requested_url
            && requested_url != state.url
        {
            return CdpDispatch::error(-32000, "Page.getResourceContent: unknown resource URL");
        }
        let content = match self.browser.dispatch(BrowserCommand::DocumentText {
            context_id,
            document_id: state.document_id,
            viewport: self.current_viewport(),
            kind: DocumentTextKind::Dom,
        }) {
            Ok(BrowserCommandResult::DocumentText(content)) => content,
            Ok(result) => {
                return CdpDispatch::error(
                    -32603,
                    format!("unexpected document result: {result:?}"),
                );
            }
            Err(error) => return CdpDispatch::error(-32603, error.to_string()),
        };
        CdpDispatch::ok(json!({
            "content": content,
            "base64Encoded": false,
        }))
    }

    fn page_add_script_to_evaluate_on_new_document(&mut self, req: &CdpRequest) -> CdpDispatch {
        let Some(source) = req.params.get("source").and_then(Value::as_str) else {
            return CdpDispatch::error(
                -32602,
                "Page.addScriptToEvaluateOnNewDocument: missing `source`",
            );
        };
        self.next_new_document_script_id += 1;
        let identifier = format!("vixen-init-script-{}", self.next_new_document_script_id);
        self.new_document_scripts.push(NewDocumentScript {
            identifier: identifier.clone(),
            source: source.to_owned(),
        });
        if let Err(error) = self.configure_current_context() {
            return CdpDispatch::error(-32603, error);
        }
        CdpDispatch::ok(json!({ "identifier": identifier }))
    }

    fn page_remove_script_to_evaluate_on_new_document(&mut self, req: &CdpRequest) -> CdpDispatch {
        let Some(identifier) = req.params.get("identifier").and_then(Value::as_str) else {
            return CdpDispatch::error(
                -32602,
                "Page.removeScriptToEvaluateOnNewDocument: missing `identifier`",
            );
        };
        self.new_document_scripts
            .retain(|script| script.identifier != identifier);
        if let Err(error) = self.configure_current_context() {
            return CdpDispatch::error(-32603, error);
        }
        CdpDispatch::ok(json!({}))
    }

    fn page_handle_javascript_dialog(&self, req: &CdpRequest) -> CdpDispatch {
        let accepted = req
            .params
            .get("accept")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        CdpDispatch::ok_with_notifications(
            json!({}),
            vec![notification(
                "Page.javascriptDialogClosed",
                json!({ "result": accepted, "userInput": req.params.get("promptText").and_then(Value::as_str).unwrap_or("") }),
                req.session_id.as_deref(),
            )],
        )
    }

    fn page_create_isolated_world(&mut self, req: &CdpRequest) -> CdpDispatch {
        let state = match self.current_state() {
            Ok(state) => state,
            Err(error) => return CdpDispatch::error(-32000, error),
        };
        let state_frame_id = frame_id(&state);
        let requested_frame_id = req
            .params
            .get("frameId")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !requested_frame_id.is_empty() && requested_frame_id != state_frame_id {
            return CdpDispatch::error(-32602, "Page.createIsolatedWorld: unknown frameId");
        }
        let world_name = req
            .params
            .get("worldName")
            .and_then(Value::as_str)
            .unwrap_or("Vixen utility world")
            .to_owned();
        let origin = origin_for_url(&state.url);
        self.isolated_world_name = Some(world_name.clone());
        let execution_context_id = utility_context_id(&state);
        let notifications = self.runtime_enabled.then(|| {
            self.runtime_context_created_notification(RuntimeContextNotification {
                id: execution_context_id,
                name: &world_name,
                unique_prefix: "vixen-utility-context",
                is_default: false,
                context_type: "isolated",
                frame_id: &state_frame_id,
                origin: &origin,
                loader_id: state.document_id.get(),
                session_id: req.session_id.as_deref(),
            })
        });
        CdpDispatch::ok_with_notifications(
            json!({ "executionContextId": execution_context_id }),
            notifications.into_iter().collect(),
        )
    }

    fn emulation_set_device_metrics_override(&mut self, req: &CdpRequest) -> CdpDispatch {
        let width =
            match positive_u32_param(&req.params, "width", "Emulation.setDeviceMetricsOverride") {
                Ok(width) => width,
                Err(err) => return CdpDispatch::error(-32602, err),
            };
        let height =
            match positive_u32_param(&req.params, "height", "Emulation.setDeviceMetricsOverride") {
                Ok(height) => height,
                Err(err) => return CdpDispatch::error(-32602, err),
            };
        self.emulated_viewport = Some((width, height));
        if let Err(error) = self.configure_current_context() {
            return CdpDispatch::error(-32603, error);
        }
        CdpDispatch::ok(json!({}))
    }

    fn emulation_set_emulated_media(&mut self, req: &CdpRequest) -> CdpDispatch {
        let media_type = req
            .params
            .get("media")
            .and_then(Value::as_str)
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty());
        let color_scheme = req
            .params
            .get("features")
            .and_then(Value::as_array)
            .and_then(|features| {
                features.iter().find_map(|feature| {
                    let name = feature.get("name")?.as_str()?.to_ascii_lowercase();
                    if name != "prefers-color-scheme" {
                        return None;
                    }
                    let value = feature.get("value")?.as_str()?.to_ascii_lowercase();
                    matches!(value.as_str(), "dark" | "light" | "no-preference").then_some(value)
                })
            });
        self.emulated_media = EmulatedMedia {
            media_type,
            color_scheme,
        };
        if let Err(error) = self.configure_current_context() {
            return CdpDispatch::error(-32603, error);
        }
        CdpDispatch::ok(json!({}))
    }

    fn performance_get_metrics(&mut self, req: &CdpRequest) -> CdpDispatch {
        let snapshot = self
            .context_for_session(req.session_id.as_deref())
            .and_then(|context_id| {
                let state = self.context_state(context_id).ok()?;
                match self
                    .browser
                    .dispatch(BrowserCommand::Snapshot {
                        context_id,
                        document_id: state.document_id,
                        viewport: self.current_viewport(),
                    })
                    .ok()?
                {
                    BrowserCommandResult::Snapshot(snapshot) => Some(snapshot),
                    _ => None,
                }
            })
            .unwrap_or_default();
        CdpDispatch::ok(json!({
            "metrics": [
                { "name": "Timestamp", "value": (now_ms() as f64) / 1000.0 },
                { "name": "Documents", "value": if snapshot.url.is_empty() { 0.0 } else { 1.0 } },
                { "name": "Nodes", "value": snapshot.element_count as f64 },
                { "name": "LayoutCount", "value": 1.0 },
                { "name": "RecalcStyleCount", "value": 1.0 },
                { "name": "JSHeapUsedSize", "value": 0.0 },
                { "name": "JSHeapTotalSize", "value": 0.0 },
            ]
        }))
    }

    fn security_get_state(&mut self, req: &CdpRequest) -> CdpDispatch {
        let url = self
            .context_for_session(req.session_id.as_deref())
            .and_then(|context_id| self.context_state(context_id).ok())
            .map(|state| state.url)
            .unwrap_or_else(|| "about:blank".to_owned());
        let scheme = url::Url::parse(&url)
            .map(|url| url.scheme().to_owned())
            .unwrap_or_else(|_| "about".to_owned());
        let state = match scheme.as_str() {
            "https" => "secure",
            "http" => "insecure",
            _ => "neutral",
        };
        CdpDispatch::ok(json!({
            "securityState": state,
            "schemeIsCryptographic": scheme == "https",
            "explanations": [],
            "insecureContentStatus": {
                "ranMixedContent": false,
                "displayedMixedContent": false,
                "containedMixedForm": false,
                "ranContentWithCertErrors": false,
                "displayedContentWithCertErrors": false,
                "ranInsecureContentStyle": "unknown",
                "displayedInsecureContentStyle": "unknown"
            },
            "summary": "Vixen CDP security state is derived from the current document URL scheme"
        }))
    }

    fn runtime_enable(&mut self, req: &CdpRequest) -> CdpDispatch {
        self.runtime_enabled = true;
        CdpDispatch::ok_with_notifications(
            json!({}),
            vec![self.runtime_main_context_created_notification(req.session_id.as_deref())],
        )
    }

    fn dom_get_document(&mut self, req: &CdpRequest) -> CdpDispatch {
        let Some(context_id) = self.context_for_session(req.session_id.as_deref()) else {
            return CdpDispatch::error(-32000, "DOM.getDocument: no page loaded");
        };
        let state = match self.context_state(context_id) {
            Ok(state) => state,
            Err(error) => return CdpDispatch::error(-32603, error),
        };
        let elements = match self.query_selector_all("*", self.current_viewport()) {
            Ok(elements) => elements,
            Err(error) => return CdpDispatch::error(-32603, error),
        };
        let depth = req.params.get("depth").and_then(Value::as_i64).unwrap_or(1);
        CdpDispatch::ok(json!({ "root": cdp_document_node(&state, &elements, depth) }))
    }

    fn dom_query_selector(&mut self, req: &CdpRequest) -> CdpDispatch {
        let selector = match req.params.get("selector").and_then(Value::as_str) {
            Some(selector) if !selector.is_empty() => selector,
            _ => return CdpDispatch::error(-32602, "DOM.querySelector: missing `selector`"),
        };
        if let Err(err) = cdp_query_root_node_id(&req.params, "DOM.querySelector") {
            return CdpDispatch::error(-32602, err);
        }
        match self.query_selector_all(selector, self.current_viewport()) {
            Ok(elements) => CdpDispatch::ok(json!({
                "nodeId": elements.first().map(|element| element.node_id).unwrap_or(0),
            })),
            Err(err) => CdpDispatch::error(-32602, format!("DOM.querySelector: {err}")),
        }
    }

    fn dom_query_selector_all(&mut self, req: &CdpRequest) -> CdpDispatch {
        let selector = match req.params.get("selector").and_then(Value::as_str) {
            Some(selector) if !selector.is_empty() => selector,
            _ => return CdpDispatch::error(-32602, "DOM.querySelectorAll: missing `selector`"),
        };
        if let Err(err) = cdp_query_root_node_id(&req.params, "DOM.querySelectorAll") {
            return CdpDispatch::error(-32602, err);
        }
        match self.query_selector_all(selector, self.current_viewport()) {
            Ok(elements) => CdpDispatch::ok(json!({
                "nodeIds": elements.into_iter().map(|element| element.node_id).collect::<Vec<_>>(),
            })),
            Err(err) => CdpDispatch::error(-32602, format!("DOM.querySelectorAll: {err}")),
        }
    }

    fn dom_describe_node(&mut self, req: &CdpRequest) -> CdpDispatch {
        let node_id = match self.dom_node_id_from_params(&req.params, "DOM.describeNode") {
            Ok(node_id) => node_id,
            Err(err) => return CdpDispatch::error(-32602, err),
        };
        let Some(context_id) = self.context_for_session(req.session_id.as_deref()) else {
            return CdpDispatch::error(-32000, "DOM.describeNode: no page loaded");
        };
        let state = match self.context_state(context_id) {
            Ok(state) => state,
            Err(error) => return CdpDispatch::error(-32603, error),
        };
        if node_id == CDP_DOCUMENT_NODE_ID {
            let elements = match self.query_selector_all("*", self.current_viewport()) {
                Ok(elements) => elements,
                Err(error) => return CdpDispatch::error(-32603, error),
            };
            return CdpDispatch::ok(json!({ "node": cdp_document_node(&state, &elements, 1) }));
        }
        let element = match self.element_for_node_id(node_id) {
            Ok(Some(element)) => element,
            Ok(None) => return CdpDispatch::error(-32000, "DOM.describeNode: node not found"),
            Err(err) => return CdpDispatch::error(-32603, err),
        };
        CdpDispatch::ok(json!({ "node": cdp_node_from_element(&element) }))
    }

    fn dom_resolve_node(&mut self, req: &CdpRequest) -> CdpDispatch {
        let node_id = match self.dom_node_id_from_params(&req.params, "DOM.resolveNode") {
            Ok(node_id) => node_id,
            Err(err) => return CdpDispatch::error(-32602, err),
        };
        let object_group = req
            .params
            .get("objectGroup")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let object_id = self.next_remote_object_id();
        let object_id_json = serde_json::to_string(&object_id).unwrap_or_else(|_| "\"\"".into());
        let store_expr = if node_id == CDP_DOCUMENT_NODE_ID {
            format!(
                r#"(() => {{
                globalThis.__vixenCdpObjects = globalThis.__vixenCdpObjects || Object.create(null);
                globalThis.__vixenCdpObjects[{object_id_json}] = document;
                return document;
            }})()"#
            )
        } else {
            format!(
                r#"(() => {{
                globalThis.__vixenCdpObjects = globalThis.__vixenCdpObjects || Object.create(null);
                const __nodeId = {node_id};
                const __nodes = document.querySelectorAll('*');
                for (const __node of __nodes) {{
                    if (__node && __node.__vixenNodeId === __nodeId) {{
                        globalThis.__vixenCdpObjects[{object_id_json}] = __node;
                        return __node;
                    }}
                }}
                throw new Error('DOM.resolveNode: node not found');
            }})()"#
            )
        };
        match self.evaluate_js(&store_expr) {
            Ok(value) => {
                self.register_remote_object_id(object_id.clone(), object_group);
                CdpDispatch::ok(json!({
                    "object": self.remote_object_from_js_value(&value, Some(&object_id)),
                }))
            }
            Err(err) => CdpDispatch::error(-32603, err.to_string()),
        }
    }

    fn dom_get_attributes(&mut self, req: &CdpRequest) -> CdpDispatch {
        let object_expr = match self.dom_object_expr_from_params(&req.params, "DOM.getAttributes") {
            Ok(expr) => expr,
            Err(err) => return CdpDispatch::error(-32602, err),
        };
        let expr = format!(
            r#"(() => {{
                const __node = {object_expr};
                if (!__node || !__node.attributes) throw new Error('DOM.getAttributes: node not found');
                return JSON.stringify(Array.from(__node.attributes).flatMap((attr) => [attr.name, attr.value]));
            }})()"#
        );
        match self.evaluate_js(&expr) {
            Ok(ScriptValue::String(attrs)) => match serde_json::from_str::<Value>(&attrs) {
                Ok(attributes) => CdpDispatch::ok(json!({ "attributes": attributes })),
                Err(err) => CdpDispatch::error(-32603, format!("DOM.getAttributes: {err}")),
            },
            Ok(_) => CdpDispatch::error(-32603, "DOM.getAttributes: runtime returned non-string"),
            Err(err) => CdpDispatch::error(-32603, err.to_string()),
        }
    }

    fn dom_get_outer_html(&mut self, req: &CdpRequest) -> CdpDispatch {
        let object_expr = match self.dom_object_expr_from_params(&req.params, "DOM.getOuterHTML") {
            Ok(expr) => expr,
            Err(err) => return CdpDispatch::error(-32602, err),
        };
        let expr = format!(
            r#"(() => {{
                const __node = {object_expr};
                if (__node === document) return document.documentElement ? document.documentElement.outerHTML : '';
                if (!__node || typeof __node.outerHTML !== 'string') throw new Error('DOM.getOuterHTML: node not found');
                return __node.outerHTML;
            }})()"#
        );
        match self.evaluate_js(&expr) {
            Ok(ScriptValue::String(outer_html)) => {
                CdpDispatch::ok(json!({ "outerHTML": outer_html }))
            }
            Ok(value) => CdpDispatch::ok(json!({ "outerHTML": value.to_display() })),
            Err(err) => CdpDispatch::error(-32603, err.to_string()),
        }
    }

    fn dom_set_attribute_value(&mut self, req: &CdpRequest) -> CdpDispatch {
        let object_expr =
            match self.dom_object_expr_from_params(&req.params, "DOM.setAttributeValue") {
                Ok(expr) => expr,
                Err(err) => return CdpDispatch::error(-32602, err),
            };
        let Some(name) = req.params.get("name").and_then(Value::as_str) else {
            return CdpDispatch::error(-32602, "DOM.setAttributeValue: missing `name`");
        };
        let Some(value) = req.params.get("value").and_then(Value::as_str) else {
            return CdpDispatch::error(-32602, "DOM.setAttributeValue: missing `value`");
        };
        let name = serde_json::to_string(name).unwrap_or_else(|_| "\"\"".into());
        let value = serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into());
        let expr = format!(
            r#"(() => {{
                const __node = {object_expr};
                if (!__node || typeof __node.setAttribute !== 'function') throw new Error('DOM.setAttributeValue: node not found');
                __node.setAttribute({name}, {value});
                return undefined;
            }})()"#
        );
        match self.evaluate_js(&expr) {
            Ok(_) => CdpDispatch::ok(json!({})),
            Err(err) => CdpDispatch::error(-32603, err.to_string()),
        }
    }

    fn dom_remove_attribute(&mut self, req: &CdpRequest) -> CdpDispatch {
        let object_expr = match self.dom_object_expr_from_params(&req.params, "DOM.removeAttribute")
        {
            Ok(expr) => expr,
            Err(err) => return CdpDispatch::error(-32602, err),
        };
        let Some(name) = req.params.get("name").and_then(Value::as_str) else {
            return CdpDispatch::error(-32602, "DOM.removeAttribute: missing `name`");
        };
        let name = serde_json::to_string(name).unwrap_or_else(|_| "\"\"".into());
        let expr = format!(
            r#"(() => {{
                const __node = {object_expr};
                if (!__node || typeof __node.removeAttribute !== 'function') throw new Error('DOM.removeAttribute: node not found');
                __node.removeAttribute({name});
                return undefined;
            }})()"#
        );
        match self.evaluate_js(&expr) {
            Ok(_) => CdpDispatch::ok(json!({})),
            Err(err) => CdpDispatch::error(-32603, err.to_string()),
        }
    }

    fn dom_get_content_quads(&mut self, req: &CdpRequest) -> CdpDispatch {
        if let Some(object_id) = req.params.get("objectId").and_then(Value::as_str) {
            match self.dom_bbox_from_object(object_id, "DOM.getContentQuads") {
                Ok(Some(bbox)) => {
                    return CdpDispatch::ok(json!({ "quads": [quad_from_bbox(bbox)] }));
                }
                Ok(None) => {}
                Err(err) => return CdpDispatch::error(-32602, err),
            }
        }
        let node_id = match self.dom_node_id_from_params(&req.params, "DOM.getContentQuads") {
            Ok(node_id) => node_id,
            Err(err) => return CdpDispatch::error(-32602, err),
        };
        let bbox = match self
            .element_for_node_id(node_id)
            .map(|element| element.and_then(element_cdp_bbox))
        {
            Ok(Some(bbox)) => bbox,
            Ok(None) => return CdpDispatch::ok(json!({ "quads": [] })),
            Err(err) => return CdpDispatch::error(-32603, err),
        };
        CdpDispatch::ok(json!({ "quads": [quad_from_bbox(bbox)] }))
    }

    fn dom_get_box_model(&mut self, req: &CdpRequest) -> CdpDispatch {
        if let Some(object_id) = req.params.get("objectId").and_then(Value::as_str) {
            match self.dom_bbox_from_object(object_id, "DOM.getBoxModel") {
                Ok(Some(bbox)) => return CdpDispatch::ok(box_model_from_bbox(bbox)),
                Ok(None) => {}
                Err(err) => return CdpDispatch::error(-32602, err),
            }
        }
        let node_id = match self.dom_node_id_from_params(&req.params, "DOM.getBoxModel") {
            Ok(node_id) => node_id,
            Err(err) => return CdpDispatch::error(-32602, err),
        };
        let bbox = match self
            .element_for_node_id(node_id)
            .map(|element| element.and_then(element_cdp_bbox))
        {
            Ok(Some(bbox)) => bbox,
            Ok(None) => return CdpDispatch::error(-32000, "DOM.getBoxModel: node has no box"),
            Err(err) => return CdpDispatch::error(-32603, err),
        };
        CdpDispatch::ok(box_model_from_bbox(bbox))
    }

    fn dom_object_expr_from_params(
        &mut self,
        params: &Value,
        method: &str,
    ) -> Result<String, String> {
        if let Some(object_id) = params.get("objectId").and_then(Value::as_str) {
            self.validate_remote_object_id(object_id, method)?;
            return Ok(cdp_object_expr(object_id));
        }
        let node_id = self.dom_node_id_from_params(params, method)?;
        if node_id == CDP_DOCUMENT_NODE_ID {
            return Ok("document".to_owned());
        }
        Ok(format!(
            r#"(() => {{
                const __nodeId = {node_id};
                const __nodes = document.querySelectorAll('*');
                for (const __node of __nodes) {{
                    if (__node && __node.__vixenNodeId === __nodeId) return __node;
                }}
                return null;
            }})()"#
        ))
    }

    fn dom_node_id_from_params(&mut self, params: &Value, method: &str) -> Result<usize, String> {
        for name in ["nodeId", "backendNodeId"] {
            if let Some(value) = params.get(name) {
                let id = value
                    .as_u64()
                    .ok_or_else(|| format!("{method}: `{name}` must be a positive integer"))?;
                let id =
                    usize::try_from(id).map_err(|_| format!("{method}: `{name}` is too large"))?;
                if id == 0 {
                    return Err(format!("{method}: `{name}` must be a positive integer"));
                }
                return Ok(id);
            }
        }
        let object_id = params
            .get("objectId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                format!("{method}: one of `nodeId`, `backendNodeId`, or `objectId` is required")
            })?;
        self.dom_node_id_from_object(object_id, method)
    }

    fn dom_node_id_from_object(&mut self, object_id: &str, method: &str) -> Result<usize, String> {
        self.validate_remote_object_id(object_id, method)?;
        let object_expr = cdp_object_expr(object_id);
        let probe = format!(
            "(() => {{ const __o = {object_expr}; const __id = __o && __o.__vixenNodeId; return Number.isInteger(__id) && __id > 0 ? __id : null; }})()"
        );
        match self
            .evaluate_js(&probe)
            .map_err(|err| format!("{method}: {err}"))?
        {
            ScriptValue::Int32(id) if id > 0 => Ok(id as usize),
            ScriptValue::Number(id)
                if id.is_finite() && id.fract() == 0.0 && id > 0.0 && id <= usize::MAX as f64 =>
            {
                Ok(id as usize)
            }
            _ => Err(format!(
                "{method}: objectId does not reference a Vixen Element"
            )),
        }
    }

    fn dom_bbox_from_object(
        &mut self,
        object_id: &str,
        method: &str,
    ) -> Result<Option<(f64, f64, f64, f64)>, String> {
        self.validate_remote_object_id(object_id, method)?;
        let object_expr = cdp_object_expr(object_id);
        let probe = format!(
            r#"(() => {{
                const __o = {object_expr};
                if (!__o || typeof __o.getBoundingClientRect !== 'function') return null;
                const __r = __o.getBoundingClientRect();
                const __number = (value) => {{
                    const n = Number(value);
                    return Number.isFinite(n) ? n : 0;
                }};
                return JSON.stringify({{
                    x: __number(__r.x ?? __r.left),
                    y: __number(__r.y ?? __r.top),
                    width: Math.max(0, __number(__r.width)),
                    height: Math.max(0, __number(__r.height)),
                }});
            }})()"#
        );
        match self
            .evaluate_js(&probe)
            .map_err(|err| format!("{method}: {err}"))?
        {
            ScriptValue::String(json) => {
                let rect: CdpDomRect = serde_json::from_str(&json)
                    .map_err(|err| format!("{method}: invalid object rect: {err}"))?;
                Ok(Some((rect.x, rect.y, rect.width, rect.height)))
            }
            ScriptValue::Null | ScriptValue::Undefined => Ok(None),
            _ => Err(format!("{method}: object rect probe returned a non-string")),
        }
    }

    fn dom_node_id_at_point_from_runtime(
        &mut self,
        x: f64,
        y: f64,
    ) -> Result<Option<usize>, String> {
        let x = serde_json::to_string(&x).map_err(|err| err.to_string())?;
        let y = serde_json::to_string(&y).map_err(|err| err.to_string())?;
        let probe = format!(
            "(() => {{ const __e = document.elementFromPoint({x}, {y}); const __id = __e && __e.__vixenNodeId; return Number.isInteger(__id) && __id > 0 ? __id : null; }})()"
        );
        match self
            .evaluate_js(&probe)
            .map_err(|err| format!("Input.dispatchMouseEvent: {err}"))?
        {
            ScriptValue::Int32(id) if id > 0 => Ok(Some(id as usize)),
            ScriptValue::Number(id)
                if id.is_finite() && id.fract() == 0.0 && id > 0.0 && id <= usize::MAX as f64 =>
            {
                Ok(Some(id as usize))
            }
            ScriptValue::Null | ScriptValue::Undefined => Ok(None),
            _ => Err("Input.dispatchMouseEvent: point probe returned a non-node id".to_owned()),
        }
    }

    fn dispatch_key_to_core(
        &mut self,
        event_type: &str,
        event: KeyEventData,
    ) -> Result<Vec<NavigationActionOutcome>, String> {
        let state = self.current_state()?;
        let runtime_context_id = state
            .runtime_context_id
            .ok_or_else(|| "Input.dispatchKeyEvent: no runtime".to_owned())?;
        match self
            .browser
            .dispatch(BrowserCommand::DispatchKeyEvent {
                context_id: state.context_id,
                document_id: state.document_id,
                runtime_context_id,
                event_type: event_type.to_owned(),
                event,
            })
            .map_err(|error| error.to_string())?
        {
            BrowserCommandResult::InputDispatched(result) => {
                self.queue_runtime_effects(runtime_context_id, result.effects);
                Ok(result.navigation_actions)
            }
            result => Err(format!("unexpected key-input result: {result:?}")),
        }
    }

    fn input_dispatch_mouse_event(&mut self, req: &CdpRequest) -> CdpDispatch {
        let event_type = match req.params.get("type").and_then(Value::as_str) {
            Some(event_type) => event_type,
            None => return CdpDispatch::error(-32602, "Input.dispatchMouseEvent: missing `type`"),
        };
        let x = match finite_param(&req.params, "x") {
            Ok(x) => x,
            Err(err) => return CdpDispatch::error(-32602, err),
        };
        let y = match finite_param(&req.params, "y") {
            Ok(y) => y,
            Err(err) => return CdpDispatch::error(-32602, err),
        };
        let button = match mouse_button(&req.params) {
            Ok(button) => button,
            Err(err) => return CdpDispatch::error(-32602, err),
        };
        let buttons = req
            .params
            .get("buttons")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let click_count = req
            .params
            .get("clickCount")
            .and_then(Value::as_i64)
            .unwrap_or(1)
            .max(0);
        let delta_x = match optional_finite_param(&req.params, "deltaX") {
            Ok(delta_x) => delta_x,
            Err(err) => return CdpDispatch::error(-32602, err),
        };
        let delta_y = match optional_finite_param(&req.params, "deltaY") {
            Ok(delta_y) => delta_y,
            Err(err) => return CdpDispatch::error(-32602, err),
        };

        let viewport = self.current_viewport();
        let state = match self.current_state() {
            Ok(state) => state,
            Err(error) => return CdpDispatch::error(-32000, error),
        };
        let target_node_id = self
            .dom_node_id_at_point_from_runtime(x, y)
            .ok()
            .flatten()
            .or_else(|| {
                match self.browser.dispatch(BrowserCommand::HitTest {
                    context_id: state.context_id,
                    document_id: state.document_id,
                    viewport,
                    x,
                    y,
                }) {
                    Ok(BrowserCommandResult::HitTest(target)) => {
                        target.map(|target| target.node_id)
                    }
                    _ => None,
                }
            });
        let mut dom_events = Vec::new();
        let mut next_mouse_down = self.last_mouse_down.clone();
        let mut next_mouse_over_node_id = self.last_mouse_over_node_id;

        match event_type {
            "mouseMoved" => {
                if self.last_mouse_over_node_id != target_node_id {
                    if let Some(node_id) = self.last_mouse_over_node_id {
                        dom_events.push(MouseDomEvent {
                            node_id,
                            event_type: "mouseout",
                            related_node_id: target_node_id,
                            bubbles: true,
                        });
                        dom_events.push(MouseDomEvent {
                            node_id,
                            event_type: "mouseleave",
                            related_node_id: target_node_id,
                            bubbles: false,
                        });
                    }
                    if let Some(node_id) = target_node_id {
                        dom_events.push(MouseDomEvent {
                            node_id,
                            event_type: "mouseover",
                            related_node_id: self.last_mouse_over_node_id,
                            bubbles: true,
                        });
                        dom_events.push(MouseDomEvent {
                            node_id,
                            event_type: "mouseenter",
                            related_node_id: self.last_mouse_over_node_id,
                            bubbles: false,
                        });
                    }
                    next_mouse_over_node_id = target_node_id;
                }
                if let Some(node_id) = target_node_id {
                    dom_events.push(MouseDomEvent {
                        node_id,
                        event_type: "mousemove",
                        related_node_id: None,
                        bubbles: true,
                    });
                }
            }
            "mousePressed" => {
                if let Some(node_id) = target_node_id {
                    dom_events.push(MouseDomEvent {
                        node_id,
                        event_type: "mousedown",
                        related_node_id: None,
                        bubbles: true,
                    });
                    next_mouse_down = Some(MouseDownTarget { node_id, button });
                } else {
                    next_mouse_down = None;
                }
            }
            "mouseReleased" => {
                if let Some(node_id) = target_node_id {
                    dom_events.push(MouseDomEvent {
                        node_id,
                        event_type: "mouseup",
                        related_node_id: None,
                        bubbles: true,
                    });
                    if let Some(down) = self.last_mouse_down.as_ref()
                        && down.node_id == node_id
                        && down.button == button
                    {
                        if button == MouseButton::Left {
                            dom_events.push(MouseDomEvent {
                                node_id,
                                event_type: "click",
                                related_node_id: None,
                                bubbles: true,
                            });
                            if click_count >= 2 {
                                dom_events.push(MouseDomEvent {
                                    node_id,
                                    event_type: "dblclick",
                                    related_node_id: None,
                                    bubbles: true,
                                });
                            }
                        } else if button == MouseButton::Right {
                            dom_events.push(MouseDomEvent {
                                node_id,
                                event_type: "contextmenu",
                                related_node_id: None,
                                bubbles: true,
                            });
                        }
                    }
                }
                next_mouse_down = None;
            }
            "mouseWheel" => {
                if let Some(node_id) = target_node_id {
                    dom_events.push(MouseDomEvent {
                        node_id,
                        event_type: "wheel",
                        related_node_id: None,
                        bubbles: true,
                    });
                }
            }
            _ => {
                return CdpDispatch::error(
                    -32602,
                    "Input.dispatchMouseEvent: unsupported mouse event type",
                );
            }
        }

        let mut navigation_actions = Vec::new();
        for dom_event in dom_events {
            let state = match self.current_state() {
                Ok(state) => state,
                Err(error) => return CdpDispatch::error(-32000, error),
            };
            let Some(runtime_context_id) = state.runtime_context_id else {
                return CdpDispatch::error(-32000, "Input.dispatchMouseEvent: no runtime");
            };
            let modifiers = req
                .params
                .get("modifiers")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            match self.browser.dispatch(BrowserCommand::DispatchMouseEvent {
                context_id: state.context_id,
                document_id: state.document_id,
                runtime_context_id,
                node_id: dom_event.node_id,
                event_type: dom_event.event_type.to_owned(),
                event: MouseEventData {
                    x,
                    y,
                    button: button.dom_button_code(),
                    buttons,
                    detail: click_count,
                    related_node_id: dom_event.related_node_id,
                    bubbles: dom_event.bubbles,
                    ctrl_key: modifiers & 2 != 0,
                    shift_key: modifiers & 8 != 0,
                    alt_key: modifiers & 1 != 0,
                    meta_key: modifiers & 4 != 0,
                    delta_x,
                    delta_y,
                },
            }) {
                Ok(BrowserCommandResult::InputDispatched(result)) => {
                    self.queue_runtime_effects(runtime_context_id, result.effects);
                    navigation_actions.extend(result.navigation_actions);
                }
                Ok(result) => {
                    return CdpDispatch::error(
                        -32603,
                        format!("unexpected mouse-input result: {result:?}"),
                    );
                }
                Err(error) => return CdpDispatch::error(-32603, error.to_string()),
            }
        }
        self.last_mouse_down = next_mouse_down;
        self.last_mouse_over_node_id = next_mouse_over_node_id;
        self.command_navigation_actions.extend(navigation_actions);
        let mut notifications = self.drain_side_effect_notifications(req);
        match self.drain_navigation_notifications(req) {
            Ok(mut navigation_notifications) => notifications.append(&mut navigation_notifications),
            Err(err) => return CdpDispatch::error(-32603, err),
        }
        CdpDispatch::ok_with_notifications(json!({}), notifications)
    }

    fn input_dispatch_key_event(&mut self, req: &CdpRequest) -> CdpDispatch {
        let event_type = match req.params.get("type").and_then(Value::as_str) {
            Some(event_type) => event_type,
            None => return CdpDispatch::error(-32602, "Input.dispatchKeyEvent: missing `type`"),
        };
        if !matches!(event_type, "rawKeyDown" | "keyDown" | "keyUp" | "char") {
            return CdpDispatch::error(
                -32602,
                "Input.dispatchKeyEvent: unsupported key event type",
            );
        }

        let text = req
            .params
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let key = req
            .params
            .get("key")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| {
                if text.is_empty() {
                    String::new()
                } else {
                    text.clone()
                }
            });
        let code = req
            .params
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let modifiers = req
            .params
            .get("modifiers")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let apply_text = match event_type {
            "char" => !text.is_empty() && self.last_key_down_text.as_deref() != Some(text.as_str()),
            "keyUp" => false,
            _ => !text.is_empty(),
        };
        if matches!(event_type, "rawKeyDown" | "keyDown") {
            self.last_key_down_text = (!text.is_empty()).then(|| text.clone());
        } else if event_type == "keyUp" {
            self.last_key_down_text = None;
        }

        let navigation_actions = match self.dispatch_key_to_core(
            event_type,
            KeyEventData {
                key,
                code,
                text,
                apply_text,
                ctrl_key: modifiers & 2 != 0,
                shift_key: modifiers & 8 != 0,
                alt_key: modifiers & 1 != 0,
                meta_key: modifiers & 4 != 0,
                repeat: req
                    .params
                    .get("autoRepeat")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                location: req
                    .params
                    .get("location")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
            },
        ) {
            Ok(navigation_actions) => navigation_actions,
            Err(err) => return CdpDispatch::error(-32603, err),
        };
        self.command_navigation_actions.extend(navigation_actions);

        let mut notifications = self.drain_side_effect_notifications(req);
        match self.drain_navigation_notifications(req) {
            Ok(mut navigation_notifications) => notifications.append(&mut navigation_notifications),
            Err(err) => return CdpDispatch::error(-32603, err),
        }
        CdpDispatch::ok_with_notifications(json!({}), notifications)
    }

    fn input_insert_text(&mut self, req: &CdpRequest) -> CdpDispatch {
        let text = match req.params.get("text").and_then(Value::as_str) {
            Some(text) => text.to_owned(),
            None => return CdpDispatch::error(-32602, "Input.insertText: missing `text`"),
        };

        self.last_key_down_text = None;
        let navigation_actions = match self.dispatch_key_to_core(
            "char",
            KeyEventData {
                key: String::new(),
                code: String::new(),
                text,
                apply_text: true,
                ctrl_key: false,
                shift_key: false,
                alt_key: false,
                meta_key: false,
                repeat: false,
                location: 0,
            },
        ) {
            Ok(navigation_actions) => navigation_actions,
            Err(err) => {
                return CdpDispatch::error(
                    -32603,
                    err.replace("Input.dispatchKeyEvent", "Input.insertText"),
                );
            }
        };
        self.command_navigation_actions.extend(navigation_actions);

        let mut notifications = self.drain_side_effect_notifications(req);
        match self.drain_navigation_notifications(req) {
            Ok(mut navigation_notifications) => notifications.append(&mut navigation_notifications),
            Err(err) => return CdpDispatch::error(-32603, err),
        }
        CdpDispatch::ok_with_notifications(json!({}), notifications)
    }

    fn runtime_evaluate(&mut self, req: &CdpRequest) -> CdpDispatch {
        let expr = match req.params.get("expression").and_then(Value::as_str) {
            Some(e) => e.to_owned(),
            None => {
                return CdpDispatch::error(-32602, "Runtime.evaluate: missing `expression`");
            }
        };
        let return_by_value = req
            .params
            .get("returnByValue")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let object_group = req
            .params
            .get("objectGroup")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let result = if return_by_value {
            self.evaluate_serialized_value(&format!(
                "globalThis.eval({})",
                serde_json::to_string(&expr).unwrap_or_else(|_| "\"undefined\"".to_owned())
            ))
            .map(|value| (serialized_remote_object(&value), None))
        } else {
            let object_id = self.next_remote_object_id();
            let object_id_json =
                serde_json::to_string(&object_id).unwrap_or_else(|_| "\"\"".to_owned());
            let expr_json =
                serde_json::to_string(&expr).unwrap_or_else(|_| "\"undefined\"".to_owned());
            let store_expr = format!(
                "(() => {{ const __v = globalThis.eval({expr_json}); if (__v !== null && (typeof __v === 'object' || typeof __v === 'function')) {{ globalThis.__vixenCdpObjects = globalThis.__vixenCdpObjects || Object.create(null); globalThis.__vixenCdpObjects[{object_id_json}] = __v; }} return __v; }})()"
            );
            self.evaluate_js(&store_expr).map(|value| {
                let object_id = if matches!(value, ScriptValue::Object) {
                    self.register_remote_object_id(object_id.clone(), object_group);
                    Some(object_id)
                } else {
                    None
                };
                let remote = self.remote_object_from_js_value(&value, object_id.as_deref());
                (remote, object_id)
            })
        };
        match result {
            Ok((remote_object, _object_id)) => {
                let mut notifications = self.drain_side_effect_notifications(req);
                match self.drain_navigation_notifications(req) {
                    Ok(mut navigation_notifications) => {
                        notifications.append(&mut navigation_notifications);
                    }
                    Err(err) => return CdpDispatch::error(-32603, err),
                }
                CdpDispatch::ok_with_notifications(
                    json!({ "result": remote_object }),
                    notifications,
                )
            }
            Err(e) => self.runtime_exception_result(e, req),
        }
    }

    fn runtime_call_function_on(&mut self, req: &CdpRequest) -> CdpDispatch {
        let function_declaration = match req
            .params
            .get("functionDeclaration")
            .and_then(Value::as_str)
        {
            Some(declaration) => declaration,
            None => {
                return CdpDispatch::error(
                    -32602,
                    "Runtime.callFunctionOn: missing `functionDeclaration`",
                );
            }
        };
        let return_by_value = req
            .params
            .get("returnByValue")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if let Some(object_id) = req.params.get("objectId").and_then(Value::as_str)
            && let Err(error) = self.validate_remote_object_id(object_id, "Runtime.callFunctionOn")
        {
            return CdpDispatch::error(-32000, error);
        }
        if let Some(arguments) = req.params.get("arguments").and_then(Value::as_array) {
            for argument in arguments {
                if let Some(object_id) = argument.get("objectId").and_then(Value::as_str)
                    && let Err(error) =
                        self.validate_remote_object_id(object_id, "Runtime.callFunctionOn")
                {
                    return CdpDispatch::error(-32000, error);
                }
            }
        }
        let object_expr = req
            .params
            .get("objectId")
            .and_then(Value::as_str)
            .map(cdp_object_expr)
            .unwrap_or_else(|| "globalThis".to_owned());
        let args = req
            .params
            .get("arguments")
            .and_then(Value::as_array)
            .map(|args| {
                args.iter()
                    .map(cdp_call_argument_expr)
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        let call_args = if args.is_empty() {
            String::new()
        } else {
            format!(", {args}")
        };
        let declaration = serde_json::to_string(function_declaration)
            .unwrap_or_else(|_| "\"() => undefined\"".to_owned());
        let call_expr = format!(
            "(() => {{ const __fn = (0, eval)({declaration}); const __recv = {object_expr}; return __fn.call(__recv{call_args}); }})()"
        );
        let object_group = req
            .params
            .get("objectGroup")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                req.params
                    .get("objectId")
                    .and_then(Value::as_str)
                    .and_then(|object_id| self.remote_object_group(object_id))
            });

        let result = if return_by_value {
            self.evaluate_serialized_value(&call_expr)
                .map(|value| serialized_remote_object(&value))
        } else {
            let object_id = self.next_remote_object_id();
            let object_id_json =
                serde_json::to_string(&object_id).unwrap_or_else(|_| "\"\"".into());
            let store_expr = format!(
                "(async () => {{ const __v = await ({call_expr}); if (__v !== null && (typeof __v === 'object' || typeof __v === 'function')) {{ globalThis.__vixenCdpObjects = globalThis.__vixenCdpObjects || Object.create(null); globalThis.__vixenCdpObjects[{object_id_json}] = __v; }} return __v; }})()"
            );
            self.evaluate_js(&store_expr).map(|value| {
                let object_id = if matches!(value, ScriptValue::Object) {
                    self.register_remote_object_id(object_id.clone(), object_group);
                    Some(object_id.as_str())
                } else {
                    None
                };
                self.remote_object_from_js_value(&value, object_id)
            })
        };
        match result {
            Ok(remote_object) => {
                let mut notifications = self.drain_side_effect_notifications(req);
                match self.drain_navigation_notifications(req) {
                    Ok(mut navigation_notifications) => {
                        notifications.append(&mut navigation_notifications);
                    }
                    Err(err) => return CdpDispatch::error(-32603, err),
                }
                CdpDispatch::ok_with_notifications(
                    json!({ "result": remote_object }),
                    notifications,
                )
            }
            Err(e) => self.runtime_exception_result(e, req),
        }
    }

    fn runtime_get_properties(&mut self, req: &CdpRequest) -> CdpDispatch {
        let Some(object_id) = req.params.get("objectId").and_then(Value::as_str) else {
            return CdpDispatch::error(-32602, "Runtime.getProperties: missing `objectId`");
        };
        if let Err(error) = self.validate_remote_object_id(object_id, "Runtime.getProperties") {
            return CdpDispatch::error(-32000, error);
        }
        let object_group = self.remote_object_group(object_id);
        let own_properties = req
            .params
            .get("ownProperties")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let object_id = serde_json::to_string(object_id).unwrap_or_else(|_| "\"\"".into());
        let object_prefix = serde_json::to_string(&self.remote_object_prefix())
            .unwrap_or_else(|_| "\"vixen-object-invalid-\"".into());
        let script = format!(
            r#"(() => {{
                const __objectId = {object_id};
                const __store = globalThis.__vixenCdpObjects = globalThis.__vixenCdpObjects || Object.create(null);
                const __target = __store[__objectId];
                if (__target === null || __target === undefined) return '[]';
                globalThis.__vixenCdpObjectCounter = globalThis.__vixenCdpObjectCounter || 0;
                const __remote = (value) => {{
                    const type = typeof value;
                    if (type === 'undefined') return {{ type: 'undefined', description: 'undefined' }};
                    if (type === 'string') return {{ type: 'string', value, description: value }};
                    if (type === 'boolean') return {{ type: 'boolean', value, description: String(value) }};
                    if (type === 'number') {{
                        if (Number.isNaN(value)) return {{ type: 'number', unserializableValue: 'NaN', description: 'NaN' }};
                        if (value === Infinity) return {{ type: 'number', unserializableValue: 'Infinity', description: 'Infinity' }};
                        if (value === -Infinity) return {{ type: 'number', unserializableValue: '-Infinity', description: '-Infinity' }};
                        if (Object.is(value, -0)) return {{ type: 'number', unserializableValue: '-0', description: '-0' }};
                        return {{ type: 'number', value, description: String(value) }};
                    }}
                    if (type === 'bigint') return {{ type: 'bigint', unserializableValue: String(value) + 'n', description: String(value) + 'n' }};
                    if (type === 'symbol') return {{ type: 'symbol', description: String(value) }};
                    if (value === null) return {{ type: 'object', subtype: 'null', value: null, description: 'null' }};
                    const objectId = {object_prefix} + 'js-' + (++globalThis.__vixenCdpObjectCounter);
                    __store[objectId] = value;
                    const remote = {{
                        type: type === 'function' ? 'function' : 'object',
                        objectId,
                        description: type === 'function' ? (value.name ? `function ${{value.name}}()` : 'function') : 'Object',
                    }};
                    if (Array.isArray(value)) {{
                        remote.subtype = 'array';
                        remote.description = `Array(${{value.length}})`;
                    }} else if (typeof Node !== 'undefined' && value instanceof Node) {{
                        remote.subtype = 'node';
                        remote.description = value.nodeName || 'Node';
                    }}
                    return remote;
                }};
                const __findDescriptor = (object, name) => {{
                    for (let cursor = Object(object); cursor; cursor = Object.getPrototypeOf(cursor)) {{
                        const descriptor = Object.getOwnPropertyDescriptor(cursor, name);
                        if (descriptor) return descriptor;
                    }}
                    return undefined;
                }};
                const __descriptor = (name, descriptor) => {{
                    const out = {{
                        name: String(name),
                        enumerable: !!descriptor.enumerable,
                        configurable: !!descriptor.configurable,
                        isOwn: Object.prototype.hasOwnProperty.call(Object(__target), name),
                    }};
                    if ('writable' in descriptor) out.writable = !!descriptor.writable;
                    if ('value' in descriptor) out.value = __remote(descriptor.value);
                    if (typeof descriptor.get === 'function') out.get = __remote(descriptor.get);
                    if (typeof descriptor.set === 'function') out.set = __remote(descriptor.set);
                    return out;
                }};
                const names = Object.getOwnPropertyNames(Object(__target));
                const seen = new Set(names);
                if (!{own_properties}) {{
                    for (const name in Object(__target)) {{
                        if (!seen.has(name)) {{
                            names.push(name);
                            seen.add(name);
                        }}
                    }}
                }}
                const descriptors = Object.getOwnPropertyDescriptors(Object(__target));
                const result = names.map((name) => __descriptor(name, descriptors[name] || __findDescriptor(__target, name) || {{ value: __target[name], enumerable: true, configurable: true, writable: true }}));
                return JSON.stringify(result);
            }})()"#,
        );

        match self.evaluate_js(&script) {
            Ok(ScriptValue::String(properties)) => match serde_json::from_str::<Value>(&properties)
            {
                Ok(result) => {
                    register_remote_ids(&result, |object_id| {
                        self.register_remote_object_id(object_id.to_owned(), object_group.clone());
                    });
                    CdpDispatch::ok(json!({
                        "result": result,
                        "internalProperties": [],
                        "privateProperties": [],
                    }))
                }
                Err(err) => CdpDispatch::error(-32603, format!("Runtime.getProperties: {err}")),
            },
            Ok(_) => CdpDispatch::error(-32603, "Runtime.getProperties: non-string result"),
            Err(err) => CdpDispatch::error(-32603, err.to_string()),
        }
    }

    fn runtime_await_promise(&mut self, req: &CdpRequest) -> CdpDispatch {
        let Some(promise_object_id) = req.params.get("promiseObjectId").and_then(Value::as_str)
        else {
            return CdpDispatch::error(-32602, "Runtime.awaitPromise: missing `promiseObjectId`");
        };
        if let Err(error) =
            self.validate_remote_object_id(promise_object_id, "Runtime.awaitPromise")
        {
            return CdpDispatch::error(-32000, error);
        }
        let return_by_value = req
            .params
            .get("returnByValue")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let object_group = self.remote_object_group(promise_object_id);
        let promise_object_id_json =
            serde_json::to_string(promise_object_id).unwrap_or_else(|_| "\"\"".into());
        let await_expr = format!(
            r#"(async () => {{
                const __objectId = {promise_object_id_json};
                const __store = globalThis.__vixenCdpObjects || Object.create(null);
                if (!Object.prototype.hasOwnProperty.call(__store, __objectId)) {{
                    throw new Error('Runtime.awaitPromise: object not found');
                }}
                return await __store[__objectId];
            }})()"#,
        );
        let result = if return_by_value {
            self.evaluate_serialized_value(&await_expr)
                .map(|value| serialized_remote_object(&value))
        } else {
            let object_id = self.next_remote_object_id();
            let object_id_json =
                serde_json::to_string(&object_id).unwrap_or_else(|_| "\"\"".into());
            let store_expr = format!(
                "(async () => {{ const __v = await ({await_expr}); if (__v !== null && (typeof __v === 'object' || typeof __v === 'function')) {{ globalThis.__vixenCdpObjects = globalThis.__vixenCdpObjects || Object.create(null); globalThis.__vixenCdpObjects[{object_id_json}] = __v; }} return __v; }})()"
            );
            self.evaluate_js(&store_expr).map(|value| {
                let stored_object_id = if matches!(value, ScriptValue::Object) {
                    self.register_remote_object_id(object_id.clone(), object_group);
                    Some(object_id.as_str())
                } else {
                    None
                };
                self.remote_object_from_js_value(&value, stored_object_id)
            })
        };
        match result {
            Ok(remote_object) => {
                let mut notifications = self.drain_side_effect_notifications(req);
                match self.drain_navigation_notifications(req) {
                    Ok(mut navigation_notifications) => {
                        notifications.append(&mut navigation_notifications);
                    }
                    Err(err) => return CdpDispatch::error(-32603, err),
                }
                CdpDispatch::ok_with_notifications(
                    json!({ "result": remote_object }),
                    notifications,
                )
            }
            Err(e) => self.runtime_exception_result(e, req),
        }
    }

    fn runtime_release_object(&mut self, req: &CdpRequest) -> CdpDispatch {
        let Some(object_id) = req.params.get("objectId").and_then(Value::as_str) else {
            return CdpDispatch::error(-32602, "Runtime.releaseObject: missing `objectId`");
        };
        if let Err(error) = self.validate_remote_object_id(object_id, "Runtime.releaseObject") {
            return CdpDispatch::error(-32000, error);
        }
        let object_id = object_id.to_owned();
        self.remote_handles
            .retain(|handle| handle.object_id != object_id);
        match self.delete_remote_objects_from_runtime(&[object_id]) {
            Ok(_) => CdpDispatch::ok(json!({})),
            Err(error) => CdpDispatch::error(-32603, error.to_string()),
        }
    }

    fn runtime_release_object_group(&mut self, req: &CdpRequest) -> CdpDispatch {
        let Some(object_group) = req.params.get("objectGroup").and_then(Value::as_str) else {
            return CdpDispatch::error(-32602, "Runtime.releaseObjectGroup: missing `objectGroup`");
        };
        let object_ids = self
            .remote_handles
            .iter()
            .filter(|handle| handle.object_group.as_deref() == Some(object_group))
            .map(|handle| handle.object_id.clone())
            .collect::<Vec<_>>();
        if object_ids.is_empty() {
            return CdpDispatch::ok(json!({}));
        }
        self.remote_handles
            .retain(|handle| handle.object_group.as_deref() != Some(object_group));
        match self.delete_remote_objects_from_runtime(&object_ids) {
            Ok(_) => CdpDispatch::ok(json!({})),
            Err(error) => CdpDispatch::error(-32603, error.to_string()),
        }
    }

    fn runtime_add_binding(&mut self, req: &CdpRequest) -> CdpDispatch {
        let Some(name) = req.params.get("name").and_then(Value::as_str) else {
            return CdpDispatch::error(-32602, "Runtime.addBinding: missing `name`");
        };
        if name.is_empty() {
            return CdpDispatch::error(-32602, "Runtime.addBinding: empty `name`");
        }
        if !self.runtime_bindings.iter().any(|binding| binding == name) {
            self.runtime_bindings.push(name.to_owned());
        }
        if let Err(err) = self.configure_current_context() {
            return CdpDispatch::error(-32603, err);
        }
        CdpDispatch::ok(json!({}))
    }

    fn runtime_exception_result(&mut self, e: BrowserError, req: &CdpRequest) -> CdpDispatch {
        let code = e.code;
        let msg = e.to_string();
        let mut notifications = self.drain_side_effect_notifications(req);
        if self.runtime_enabled {
            notifications.push(exception_thrown_notification(
                &msg,
                code,
                req.session_id.as_deref(),
            ));
        }
        CdpDispatch::ok_with_notifications(
            json!({
                "result": { "type": "undefined" },
                "exceptionDetails": {
                    "exceptionId": 1,
                    "text": msg,
                    "code": code,
                }
            }),
            notifications,
        )
    }

    fn evaluate_js(&mut self, expr: &str) -> Result<ScriptValue, BrowserError> {
        let context_id = self
            .current_context()
            .map_err(|message| BrowserError::new("browser.unknown-context", message))?;
        let state = self
            .context_state(context_id)
            .map_err(|message| BrowserError::new("browser.closed", message))?;
        let runtime_context_id = state.runtime_context_id.ok_or_else(|| {
            BrowserError::new("browser.stale-runtime", "document has no active runtime")
        })?;
        let result = match self
            .browser
            .dispatch(BrowserCommand::EvaluateForAutomation {
                context_id,
                document_id: state.document_id,
                runtime_context_id,
                source: expr.to_owned(),
            })? {
            BrowserCommandResult::AutomationEvaluation(evaluation) => {
                self.queue_runtime_effects(runtime_context_id, evaluation.effects);
                self.command_navigation_actions
                    .extend(evaluation.navigation_actions);
                evaluation.value
            }
            result => {
                return Err(BrowserError::new(
                    "browser.closed",
                    format!("unexpected automation-evaluation result: {result:?}"),
                ));
            }
        };
        Ok(result)
    }

    fn current_viewport(&self) -> (u32, u32) {
        self.emulated_viewport.unwrap_or(DEFAULT_CAPTURE_VIEWPORT)
    }

    fn evaluate_serialized_value(&mut self, expr: &str) -> Result<Value, BrowserError> {
        let expr = format!(
            "(async () => {{ const __v = await ({expr}); return JSON.stringify({{ t: typeof __v, v: __v === undefined ? null : __v }}); }})()"
        );
        match self.evaluate_js(&expr)? {
            ScriptValue::String(serialized) => Ok(serde_json::from_str(&serialized)
                .unwrap_or_else(|_| json!({ "t": "undefined", "v": Value::Null }))),
            other => Ok(json!({ "t": "string", "v": other.to_display() })),
        }
    }

    fn next_remote_object_id(&mut self) -> String {
        self.next_object_id += 1;
        let state = self.current_state().ok();
        let object_id = format!(
            "vixen-object-c{}-r{}-{}",
            state
                .as_ref()
                .map(|state| state.context_id.get())
                .unwrap_or(0),
            state
                .and_then(|state| state.runtime_context_id)
                .map(|runtime| runtime.get())
                .unwrap_or(0),
            self.next_object_id
        );
        object_id
    }

    fn remote_object_prefix(&mut self) -> String {
        let state = self.current_state().ok();
        format!(
            "vixen-object-c{}-r{}-",
            state
                .as_ref()
                .map(|state| state.context_id.get())
                .unwrap_or(0),
            state
                .and_then(|state| state.runtime_context_id)
                .map(|runtime| runtime.get())
                .unwrap_or(0),
        )
    }

    fn register_remote_object_id(&mut self, object_id: String, object_group: Option<String>) {
        if self
            .remote_handles
            .iter()
            .any(|handle| handle.object_id == object_id)
        {
            return;
        }
        let evicted = if self.remote_handles.len() >= MAX_REMOTE_HANDLES {
            self.remote_handles
                .pop_front()
                .map(|handle| handle.object_id)
        } else {
            None
        };
        self.remote_handles.push_back(RemoteHandle {
            object_id,
            object_group,
        });
        if let Some(object_id) = evicted {
            let _ = self.delete_remote_objects_from_runtime(&[object_id]);
        }
    }

    fn validate_remote_object_id(&mut self, object_id: &str, method: &str) -> Result<(), String> {
        if !object_id.starts_with(&self.remote_object_prefix()) {
            return Err(format!(
                "{method}: objectId belongs to a stale or different runtime generation"
            ));
        }
        if !self
            .remote_handles
            .iter()
            .any(|handle| handle.object_id == object_id)
        {
            return Err(format!("{method}: unknown or released objectId"));
        }
        Ok(())
    }

    fn remote_object_group(&self, object_id: &str) -> Option<String> {
        self.remote_handles
            .iter()
            .find(|handle| handle.object_id == object_id)
            .and_then(|handle| handle.object_group.clone())
    }

    fn delete_remote_objects_from_runtime(
        &mut self,
        object_ids: &[String],
    ) -> Result<ScriptValue, BrowserError> {
        let object_ids = serde_json::to_string(object_ids).unwrap_or_else(|_| "[]".to_owned());
        self.evaluate_js(&format!(
            "if (globalThis.__vixenCdpObjects) for (const __id of {object_ids}) delete globalThis.__vixenCdpObjects[__id]; undefined"
        ))
    }

    fn remote_object_from_js_value(
        &mut self,
        value: &ScriptValue,
        object_id: Option<&str>,
    ) -> Value {
        match value {
            ScriptValue::Int32(n) => {
                json!({ "type": "number", "value": n, "description": value.to_display() })
            }
            ScriptValue::Number(n) => {
                json!({ "type": "number", "value": n, "description": value.to_display() })
            }
            ScriptValue::String(s) => {
                json!({ "type": "string", "value": s, "description": value.to_display() })
            }
            ScriptValue::Bool(b) => {
                json!({ "type": "boolean", "value": b, "description": value.to_display() })
            }
            ScriptValue::Null => {
                json!({ "type": "object", "subtype": "null", "value": Value::Null, "description": "null" })
            }
            ScriptValue::Undefined => json!({ "type": "undefined", "description": "undefined" }),
            ScriptValue::Object => {
                let mut object = serde_json::Map::new();
                object.insert("type".to_owned(), json!("object"));
                object.insert("description".to_owned(), json!("Object"));
                if let Some(object_id) = object_id {
                    object.insert("objectId".to_owned(), json!(object_id));
                    if let Some(subtype) = self.remote_object_subtype(object_id)
                        && !subtype.is_empty()
                    {
                        object.insert("subtype".to_owned(), json!(subtype));
                    }
                } else {
                    object.insert("value".to_owned(), json!({}));
                }
                Value::Object(object)
            }
        }
    }

    fn remote_object_subtype(&mut self, object_id: &str) -> Option<String> {
        let object_expr = cdp_object_expr(object_id);
        let probe = format!(
            "(() => {{ const __o = {object_expr}; if (!__o) return ''; if (typeof Node !== 'undefined' && __o instanceof Node) return 'node'; if (Array.isArray(__o)) return 'array'; return ''; }})()"
        );
        match self.evaluate_js(&probe).ok()? {
            ScriptValue::String(value) => Some(value),
            _ => None,
        }
    }

    fn seed_initial_target(&mut self, url: String) -> Result<(), String> {
        let context_id = self.current_context()?;
        self.configure_context(context_id)?;
        self.drain_core_events();
        let navigation_id = match self
            .browser
            .dispatch(BrowserCommand::Navigate { context_id, url })
        {
            Ok(BrowserCommandResult::NavigationAccepted { navigation_id }) => navigation_id,
            Ok(result) => return Err(format!("unexpected initial navigation result: {result:?}")),
            Err(error) => return Err(error.to_string()),
        };
        self.wait_for_navigation(context_id, navigation_id)
            .map(|_| ())
    }

    fn push_loaded_target(&mut self, url: String) -> Result<BrowsingContextId, String> {
        let (context_id, navigation_id) = self.push_target(url)?;
        if let Some(navigation_id) = navigation_id
            && let Err(error) = self.wait_for_navigation(context_id, navigation_id)
        {
            self.remove_failed_target(context_id);
            return Err(error);
        }
        Ok(context_id)
    }

    fn push_target(
        &mut self,
        url: String,
    ) -> Result<(BrowsingContextId, Option<NavigationId>), String> {
        let context_id = match self
            .browser
            .dispatch(BrowserCommand::CreateBrowsingContext)
            .map_err(|error| error.to_string())?
        {
            BrowserCommandResult::BrowsingContextCreated { context_id } => context_id,
            result => return Err(format!("unexpected create-target result: {result:?}")),
        };
        self.next_session_id += 1;
        self.targets.push(Target {
            context_id,
            session_id: self.next_session_id,
        });
        self.context_presentations
            .insert(context_id, ContextPresentation::default());
        self.dispatch_context = Some(context_id);
        self.load_presentation(context_id);
        if let Err(error) = self.configure_context(context_id) {
            self.remove_failed_target(context_id);
            return Err(error);
        }
        self.drain_core_events();
        if url == "about:blank" {
            return Ok((context_id, None));
        }
        let navigation_id = match self
            .browser
            .dispatch(BrowserCommand::Navigate { context_id, url })
        {
            Ok(BrowserCommandResult::NavigationAccepted { navigation_id }) => navigation_id,
            Ok(result) => {
                let error = format!("unexpected target navigation result: {result:?}");
                self.remove_failed_target(context_id);
                return Err(error);
            }
            Err(error) => {
                let error = error.to_string();
                self.remove_failed_target(context_id);
                return Err(error);
            }
        };
        Ok((context_id, Some(navigation_id)))
    }

    fn remove_failed_target(&mut self, context_id: BrowsingContextId) {
        let _ = self
            .browser
            .dispatch(BrowserCommand::CloseBrowsingContext { context_id });
        let _ = self.pump_core_events();
        self.targets
            .retain(|target| target.context_id != context_id);
        self.attached_sessions
            .retain(|session| session.context_id != context_id);
        self.context_presentations.remove(&context_id);
        self.pending_core_navigations
            .retain(|_, pending| pending.context_id != context_id);
        self.command_navigation_actions
            .retain(|action| match action {
                NavigationActionOutcome::SameDocument { .. } => true,
                NavigationActionOutcome::CrossDocument { navigation_id, .. } => {
                    self.pending_core_navigations.contains_key(navigation_id)
                }
            });
        self.dispatch_context = self
            .dispatch_context
            .filter(|dispatch_context| *dispatch_context != context_id)
            .or_else(|| self.targets.first().map(|target| target.context_id));
        self.active_presentation_context = self
            .active_presentation_context
            .filter(|active_context| *active_context != context_id);
        if self.active_presentation_context.is_none()
            && let Some(context_id) = self.dispatch_context
        {
            self.load_presentation(context_id);
        }
    }

    fn drain_navigation_notifications(&mut self, req: &CdpRequest) -> Result<Vec<String>, String> {
        if self.defer_navigation_notifications {
            return Ok(Vec::new());
        }
        let navigation_actions = std::mem::take(&mut self.command_navigation_actions);
        let context_id = self
            .dispatch_context
            .or_else(|| self.context_for_session(req.session_id.as_deref()))
            .ok_or_else(|| "navigation has no page target".to_owned())?;
        let mut notifications = Vec::new();
        for action in &navigation_actions {
            if let NavigationActionOutcome::SameDocument { url } = action {
                notifications.extend(self.same_document_notifications(
                    context_id,
                    url,
                    req.session_id.as_deref(),
                ));
            }
        }
        let navigation_ids = cross_document_ids(&navigation_actions);
        let Some((&final_navigation_id, superseded)) = navigation_ids.split_last() else {
            return Ok(notifications);
        };
        for navigation_id in superseded {
            let _ = self.wait_for_navigation(context_id, *navigation_id);
        }
        let committed = self.wait_for_navigation(context_id, final_navigation_id)?;
        notifications.extend(
            self.navigation_notifications_after_completion(
                context_id,
                req,
                committed,
                final_cross_document_kind(&navigation_actions)
                    .unwrap_or(CrossDocumentNavigationKind::Regular),
            )?,
        );
        notifications.extend(self.drain_side_effect_notifications(req));
        Ok(notifications)
    }

    fn navigation_notifications_after_completion(
        &mut self,
        context_id: BrowsingContextId,
        req: &CdpRequest,
        committed: bool,
        kind: CrossDocumentNavigationKind,
    ) -> Result<Vec<String>, String> {
        if committed {
            self.reset_input_for_navigation();
            if let CrossDocumentNavigationKind::ContentReplacement {
                replaced_document_id,
            } = kind
            {
                Ok(self.current_content_replaced_notifications(
                    context_id,
                    req.session_id.as_deref(),
                    replaced_document_id.get(),
                ))
            } else {
                Ok(self.current_page_load_notifications(context_id, req.session_id.as_deref()))
            }
        } else {
            Ok(Vec::new())
        }
    }

    fn runtime_main_context_created_notification(&mut self, session_id: Option<&str>) -> String {
        let state = self
            .context_for_session(session_id)
            .and_then(|context_id| self.context_state(context_id).ok());
        match state {
            Some(state) => {
                self.runtime_main_context_created_notification_for_state(&state, session_id)
            }
            None => self.runtime_context_created_notification(RuntimeContextNotification {
                id: 0,
                name: "Vixen",
                unique_prefix: "vixen-main-context",
                is_default: true,
                context_type: "default",
                frame_id: "tab-0",
                origin: "://",
                loader_id: 0,
                session_id,
            }),
        }
    }

    fn runtime_main_context_created_notification_for_state(
        &mut self,
        state: &BrowsingContextState,
        session_id: Option<&str>,
    ) -> String {
        let frame_id = frame_id(state);
        let origin = origin_for_url(&state.url);
        self.runtime_context_created_notification(RuntimeContextNotification {
            id: state
                .runtime_context_id
                .map(|runtime_id| runtime_id.get())
                .unwrap_or(0),
            name: "Vixen",
            unique_prefix: "vixen-main-context",
            is_default: true,
            context_type: "default",
            frame_id: &frame_id,
            origin: &origin,
            loader_id: state.document_id.get(),
            session_id,
        })
    }

    fn runtime_context_created_notification(
        &mut self,
        context: RuntimeContextNotification<'_>,
    ) -> String {
        notification(
            "Runtime.executionContextCreated",
            json!({
                "context": {
                    "id": context.id,
                    "origin": context.origin,
                    "name": context.name,
                    "uniqueId": format!("{}-{}", context.unique_prefix, context.loader_id),
                    "auxData": {
                        "isDefault": context.is_default,
                        "type": context.context_type,
                        "frameId": context.frame_id,
                    }
                }
            }),
            context.session_id,
        )
    }

    fn current_page_load_notifications(
        &mut self,
        context_id: BrowsingContextId,
        session_id: Option<&str>,
    ) -> Vec<String> {
        let Ok(state) = self.context_state(context_id) else {
            return Vec::new();
        };
        self.notification_sessions(context_id, session_id)
            .into_iter()
            .flat_map(|session_id| {
                self.page_load_notifications_for_session(&state, session_id.as_deref())
            })
            .collect()
    }

    fn same_document_notifications(
        &mut self,
        context_id: BrowsingContextId,
        url: &str,
        session_id: Option<&str>,
    ) -> Vec<String> {
        let Ok(state) = self.context_state(context_id) else {
            return Vec::new();
        };
        self.notification_sessions(context_id, session_id)
            .into_iter()
            .map(|session_id| {
                notification(
                    "Page.navigatedWithinDocument",
                    json!({ "frameId": frame_id(&state), "url": url }),
                    session_id.as_deref(),
                )
            })
            .collect()
    }

    fn current_content_replaced_notifications(
        &mut self,
        context_id: BrowsingContextId,
        session_id: Option<&str>,
        loader_id: u64,
    ) -> Vec<String> {
        let Ok(state) = self.context_state(context_id) else {
            return Vec::new();
        };
        let frame_id = frame_id(&state);
        let origin = origin_for_url(&state.url);
        let timestamp = now_ms();
        self.notification_sessions(context_id, session_id)
            .into_iter()
            .flat_map(|session_id| {
                let session_id = session_id.as_deref();
                let mut notifications = Vec::new();
                if self.runtime_enabled {
                    notifications.push(notification(
                        "Runtime.executionContextsCleared",
                        json!({}),
                        session_id,
                    ));
                    notifications.push(
                        self.runtime_main_context_created_notification_for_state(
                            &state, session_id,
                        ),
                    );
                    if let Some(world_name) = self.isolated_world_name.clone() {
                        notifications.push(self.runtime_context_created_notification(
                            RuntimeContextNotification {
                                id: utility_context_id(&state),
                                name: &world_name,
                                unique_prefix: "vixen-utility-context",
                                is_default: false,
                                context_type: "isolated",
                                frame_id: &frame_id,
                                origin: &origin,
                                loader_id: state.document_id.get(),
                                session_id,
                            },
                        ));
                    }
                }
                notifications.push(notification(
                    "Page.domContentEventFired",
                    json!({ "timestamp": timestamp }),
                    session_id,
                ));
                if self.lifecycle_events_enabled {
                    notifications.push(notification(
                        "Page.lifecycleEvent",
                        json!({
                            "frameId": &frame_id,
                            "loaderId": format!("loader-{loader_id}"),
                            "name": "DOMContentLoaded",
                            "timestamp": timestamp,
                        }),
                        session_id,
                    ));
                }
                notifications.push(notification(
                    "Page.loadEventFired",
                    json!({ "timestamp": timestamp }),
                    session_id,
                ));
                if self.lifecycle_events_enabled {
                    notifications.push(notification(
                        "Page.lifecycleEvent",
                        json!({
                            "frameId": &frame_id,
                            "loaderId": format!("loader-{loader_id}"),
                            "name": "load",
                            "timestamp": timestamp,
                        }),
                        session_id,
                    ));
                }
                notifications
            })
            .collect()
    }

    fn page_load_notifications_for_session(
        &mut self,
        state: &BrowsingContextState,
        session_id: Option<&str>,
    ) -> Vec<String> {
        let frame_id = frame_id(state);
        let origin = origin_for_url(&state.url);
        let timestamp = now_ms();
        let mut notifications = Vec::new();
        if self.network_enabled {
            notifications.push(network_request_will_be_sent_notification(
                state, &frame_id, timestamp, session_id,
            ));
        }
        notifications.push(notification(
            "Page.frameStartedLoading",
            json!({ "frameId": &frame_id }),
            session_id,
        ));
        if self.lifecycle_events_enabled {
            notifications.push(page_lifecycle_event_notification(
                state, "init", timestamp, session_id,
            ));
        }
        if self.runtime_enabled {
            notifications.push(notification(
                "Runtime.executionContextsCleared",
                json!({}),
                session_id,
            ));
        }
        notifications.push(notification(
            "Page.frameNavigated",
            json!({ "frame": frame_json(state) }),
            session_id,
        ));
        if self.lifecycle_events_enabled {
            notifications.push(page_lifecycle_event_notification(
                state, "commit", timestamp, session_id,
            ));
        }
        if self.network_enabled {
            notifications.push(network_response_received_notification(
                state, &frame_id, timestamp, session_id,
            ));
        }
        if self.runtime_enabled {
            notifications
                .push(self.runtime_main_context_created_notification_for_state(state, session_id));
            if let Some(world_name) = self.isolated_world_name.clone() {
                notifications.push(self.runtime_context_created_notification(
                    RuntimeContextNotification {
                        id: utility_context_id(state),
                        name: &world_name,
                        unique_prefix: "vixen-utility-context",
                        is_default: false,
                        context_type: "isolated",
                        frame_id: &frame_id,
                        origin: &origin,
                        loader_id: state.document_id.get(),
                        session_id,
                    },
                ));
            }
        }
        notifications.push(notification(
            "Page.domContentEventFired",
            json!({ "timestamp": timestamp }),
            session_id,
        ));
        if self.lifecycle_events_enabled {
            notifications.push(page_lifecycle_event_notification(
                state,
                "DOMContentLoaded",
                timestamp,
                session_id,
            ));
        }
        notifications.push(notification(
            "Page.loadEventFired",
            json!({ "timestamp": timestamp }),
            session_id,
        ));
        if self.lifecycle_events_enabled {
            notifications.push(page_lifecycle_event_notification(
                state, "load", timestamp, session_id,
            ));
        }
        if self.network_enabled {
            notifications.push(network_loading_finished_notification(
                state, timestamp, session_id,
            ));
        }
        notifications.push(notification(
            "Page.frameStoppedLoading",
            json!({ "frameId": &frame_id }),
            session_id,
        ));
        notifications
    }

    fn drain_side_effect_notifications(&mut self, req: &CdpRequest) -> Vec<String> {
        let context_id = self
            .dispatch_context
            .or_else(|| self.context_for_session(req.session_id.as_deref()));
        self.drain_side_effect_notifications_for_optional_context(context_id, req)
    }

    fn drain_side_effect_notifications_for_context(
        &mut self,
        context_id: BrowsingContextId,
        req: &CdpRequest,
    ) -> Vec<String> {
        self.drain_side_effect_notifications_for_optional_context(Some(context_id), req)
    }

    fn drain_side_effect_notifications_for_optional_context(
        &mut self,
        context_id: Option<BrowsingContextId>,
        req: &CdpRequest,
    ) -> Vec<String> {
        let mut notifications = Vec::new();
        let mut retained = VecDeque::new();
        while let Some(pending) = self.pending_effects.pop_front() {
            if context_id != Some(pending.context_id) {
                retained.push_back(pending);
                continue;
            }
            let runtime_id = pending.runtime_context_id.get();
            let frame_id = format!("tab-{}", pending.frame_id.get());
            let loader_id = pending.document_id.get();
            let timestamp = now_ms();
            for session_id in
                self.notification_sessions(pending.context_id, req.session_id.as_deref())
            {
                let session_id = session_id.as_deref();
                if self.runtime_enabled {
                    notifications.extend(
                        pending
                            .effects
                            .console
                            .iter()
                            .cloned()
                            .map(|event| console_notification(event, runtime_id, session_id)),
                    );
                    notifications.extend(pending.effects.exceptions.iter().map(|event| {
                        exception_thrown_notification(
                            &event.error.to_string(),
                            event.error.code,
                            session_id,
                        )
                    }));
                }
                notifications.extend(
                    pending.effects.dialogs.iter().cloned().map(|event| {
                        dialog_notification(event, &frame_id, &pending.url, session_id)
                    }),
                );
                if self.runtime_enabled {
                    notifications.extend(
                        pending
                            .effects
                            .bindings
                            .iter()
                            .cloned()
                            .map(|event| binding_notification(event, runtime_id, session_id)),
                    );
                }
                if self.network_enabled {
                    notifications.extend(pending.effects.network.iter().cloned().flat_map(
                        |event| {
                            network_fetch_notifications(
                                event,
                                &frame_id,
                                loader_id,
                                &pending.url,
                                timestamp,
                                session_id,
                            )
                        },
                    ));
                }
            }
        }
        self.pending_effects = retained;
        notifications
    }

    fn reset_input_for_navigation(&mut self) {
        self.last_mouse_down = None;
        self.last_mouse_over_node_id = None;
        self.last_key_down_text = None;
        self.remote_handles.clear();
    }

    fn record_trace_event(
        &mut self,
        method: &str,
        session_id: Option<&str>,
        started: Instant,
        succeeded: bool,
    ) {
        if !self.tracing.active {
            return;
        }
        if self.tracing.events.len() >= MAX_TRACE_EVENTS {
            self.tracing.data_loss_occurred = true;
            return;
        }
        let duration = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_micros().min(u64::MAX as u128) as u64)
            .unwrap_or_default()
            .saturating_sub(duration);
        self.tracing.events.push(json!({
            "name": method,
            "cat": "vixen.cdp",
            "ph": "X",
            "ts": timestamp,
            "dur": duration,
            "pid": 1,
            "tid": 1,
            "args": {
                "sessionId": session_id,
                "ok": succeeded,
            }
        }));
    }
}

impl CdpState {
    /// Preserve the deterministic test setup while moving the runtime itself
    /// behind BrowserCore. The supplied runtime is consumed only for its
    /// transport policy and never stored by CDP.
    pub fn with_runtime(rt: JsRuntime) -> Self {
        let network = rt.network_config();
        drop(rt);
        let profile = BrowserProfile::open(None, "vixen-cdp-test-", "CDP test")
            .expect("create CDP test profile");
        let mut config = BrowserConfig::new(profile.database_path());
        config.network = network;
        Self::with_config_and_profile(config, profile).expect("start CDP BrowserCore")
    }
}

impl Default for CdpState {
    fn default() -> Self {
        Self::new().expect("start CDP BrowserCore")
    }
}

struct CdpDispatch {
    pre_response_notifications: Vec<String>,
    response: Result<Value, CdpError>,
    notifications: Vec<String>,
}

impl CdpDispatch {
    fn ok(result: Value) -> Self {
        Self {
            pre_response_notifications: Vec::new(),
            response: Ok(result),
            notifications: Vec::new(),
        }
    }

    fn ok_with_notifications(result: Value, notifications: Vec<String>) -> Self {
        Self {
            pre_response_notifications: Vec::new(),
            response: Ok(result),
            notifications,
        }
    }

    fn ok_with_pre_response_notifications(
        result: Value,
        pre_response_notifications: Vec<String>,
    ) -> Self {
        Self {
            pre_response_notifications,
            response: Ok(result),
            notifications: Vec::new(),
        }
    }

    fn error(code: i32, message: impl Into<String>) -> Self {
        Self {
            pre_response_notifications: Vec::new(),
            response: Err(CdpError::new(code, message)),
            notifications: Vec::new(),
        }
    }
}

fn canonical_permission_origin(origin: &str) -> Result<String, String> {
    let url = url::Url::parse(origin)
        .map_err(|error| format!("Browser.grantPermissions: invalid origin: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("Browser.grantPermissions: origin must use HTTP(S)".to_owned());
    }
    Ok(url.origin().ascii_serialization())
}

fn runtime_permission_name(permission: &str) -> Option<&'static str> {
    match permission {
        "geolocation" => Some("geolocation"),
        "notifications" => Some("notifications"),
        "videoCapture" => Some("camera"),
        "audioCapture" => Some("microphone"),
        "clipboardReadWrite" => Some("clipboard-read"),
        "durableStorage" => Some("persistent-storage"),
        _ => None,
    }
}

fn is_supported_cdp_permission(permission: &str) -> bool {
    matches!(
        permission,
        "geolocation"
            | "midi"
            | "midiSysex"
            | "notifications"
            | "durableStorage"
            | "audioCapture"
            | "videoCapture"
            | "backgroundSync"
            | "ambientLightSensor"
            | "sensors"
            | "accessibilityEvents"
            | "clipboardReadWrite"
            | "clipboardSanitizedWrite"
            | "paymentHandler"
            | "idleDetection"
            | "periodicBackgroundSync"
            | "wakeLockScreen"
            | "wakeLockSystem"
            | "nfc"
            | "displayCapture"
            | "localFonts"
            | "storageAccess"
            | "topLevelStorageAccess"
            | "windowManagement"
            | "capturedSurfaceControl"
            | "speakerSelection"
            | "localNetworkAccess"
            | "localNetwork"
            | "loopbackNetwork"
    )
}

fn cdp_target_id(context_id: BrowsingContextId) -> String {
    format!("tab-{}", context_id.get())
}

fn cross_document_actions(
    actions: &[NavigationActionOutcome],
) -> impl Iterator<Item = (NavigationId, CrossDocumentNavigationKind)> + '_ {
    actions.iter().filter_map(|action| match action {
        NavigationActionOutcome::SameDocument { .. } => None,
        NavigationActionOutcome::CrossDocument {
            navigation_id,
            kind,
        } => Some((*navigation_id, *kind)),
    })
}

fn cross_document_ids(actions: &[NavigationActionOutcome]) -> Vec<NavigationId> {
    cross_document_actions(actions)
        .map(|(navigation_id, _)| navigation_id)
        .collect()
}

fn final_cross_document_id(actions: &[NavigationActionOutcome]) -> Option<NavigationId> {
    cross_document_actions(actions)
        .last()
        .map(|(navigation_id, _)| navigation_id)
}

fn final_cross_document_kind(
    actions: &[NavigationActionOutcome],
) -> Option<CrossDocumentNavigationKind> {
    cross_document_actions(actions).last().map(|(_, kind)| kind)
}

fn frame_id(state: &BrowsingContextState) -> String {
    format!("tab-{}", state.main_frame_id.get())
}

fn utility_context_id(state: &BrowsingContextState) -> u64 {
    state
        .runtime_context_id
        .map(|runtime_id| runtime_id.get().saturating_add(1_000_000_000))
        .unwrap_or(1_000_000_000)
}

fn loader_id(state: &BrowsingContextState) -> String {
    format!("loader-{}", state.document_id.get())
}

fn target_info_json(context_id: BrowsingContextId, state: &BrowsingContextState) -> Value {
    json!({
        "targetId": cdp_target_id(context_id),
        "type": "page",
        "title": state.title.as_deref().unwrap_or(""),
        "url": state.url,
        "attached": false,
        "canAccessOpener": false,
        "browserContextId": "default",
    })
}

fn target_attached_notification(
    context_id: BrowsingContextId,
    target_session_id: u64,
    state: &BrowsingContextState,
    session_id: Option<&str>,
) -> String {
    notification(
        "Target.attachedToTarget",
        json!({
            "sessionId": format!("sess-{target_session_id}"),
            "targetInfo": target_info_json(context_id, state),
            "waitingForDebugger": false,
        }),
        session_id,
    )
}

fn target_detached_notification(target: &Target, session_id: Option<&str>) -> String {
    target_detached_notification_for_session(target, target.session_id, session_id)
}

fn target_detached_notification_for_session(
    target: &Target,
    detached_session_id: u64,
    session_id: Option<&str>,
) -> String {
    notification(
        "Target.detachedFromTarget",
        json!({
            "sessionId": format!("sess-{detached_session_id}"),
            "targetId": cdp_target_id(target.context_id),
        }),
        session_id,
    )
}

fn page_lifecycle_event_notification(
    state: &BrowsingContextState,
    name: &str,
    timestamp: u64,
    session_id: Option<&str>,
) -> String {
    notification(
        "Page.lifecycleEvent",
        json!({
            "frameId": frame_id(state),
            "loaderId": loader_id(state),
            "name": name,
            "timestamp": timestamp,
        }),
        session_id,
    )
}

fn frame_json(state: &BrowsingContextState) -> Value {
    json!({
        "id": frame_id(state),
        "loaderId": loader_id(state),
        "url": state.url,
        "securityOrigin": origin_for_url(&state.url),
        "mimeType": "text/html",
    })
}

fn page_get_layout_metrics(viewport: (u32, u32)) -> Value {
    let (width, height) = viewport;
    let width = width as f64;
    let height = height as f64;
    json!({
        "layoutViewport": {
            "pageX": 0,
            "pageY": 0,
            "clientWidth": width as u64,
            "clientHeight": height as u64,
        },
        "visualViewport": {
            "offsetX": 0,
            "offsetY": 0,
            "pageX": 0,
            "pageY": 0,
            "clientWidth": width,
            "clientHeight": height,
            "scale": 1,
            "zoom": 1,
        },
        "contentSize": {
            "x": 0,
            "y": 0,
            "width": width,
            "height": height,
        },
        "cssLayoutViewport": {
            "pageX": 0,
            "pageY": 0,
            "clientWidth": width as u64,
            "clientHeight": height as u64,
        },
        "cssVisualViewport": {
            "offsetX": 0,
            "offsetY": 0,
            "pageX": 0,
            "pageY": 0,
            "clientWidth": width,
            "clientHeight": height,
            "scale": 1,
        },
        "cssContentSize": {
            "x": 0,
            "y": 0,
            "width": width,
            "height": height,
        },
    })
}

fn runtime_binding_install_scripts(bindings: &[String]) -> Vec<String> {
    bindings
        .iter()
        .map(|name| runtime_binding_install_script(name))
        .collect()
}

fn runtime_binding_install_script(name: &str) -> String {
    let binding_name = serde_json::to_string(name).unwrap_or_else(|_| "\"\"".to_owned());
    format!(
        r#"(() => {{
            const bindingName = {binding_name};
            const events = globalThis.__vixenCdpBindingEvents = globalThis.__vixenCdpBindingEvents || [];
            if (typeof globalThis.__vixenDrainBindingEvents !== 'function') {{
              Object.defineProperty(globalThis, '__vixenDrainBindingEvents', {{
                value: function () {{ return events.splice(0, events.length); }},
                writable: true,
                configurable: true,
              }});
            }}
            Object.defineProperty(globalThis, bindingName, {{
              value: function (payload = '') {{
                const text = String(payload ?? '');
                events.push({{ name: bindingName, payload: text }});
                if (bindingName === '__playwright__binding__') {{
                  try {{
                    const parsed = JSON.parse(text);
                    const controller = globalThis.__playwright__binding__controller__;
                    if (controller && typeof controller.deliverBindingResult === 'function' && parsed && parsed.name !== undefined && parsed.seq !== undefined) {{
                      controller.deliverBindingResult({{ name: parsed.name, seq: parsed.seq, result: undefined }});
                    }}
                  }} catch (_) {{}}
                }}
              }},
              writable: true,
              configurable: true,
            }});
        }})()"#
    )
}

fn emulation_override_script(viewport: (u32, u32), media: &EmulatedMedia) -> String {
    let (width, height) = viewport;
    let media_type = serde_json::to_string(&media.media_type).expect("media type JSON");
    let color_scheme = serde_json::to_string(&media.color_scheme).expect("color scheme JSON");
    format!(
        r#"(() => {{
            const __width = {width};
            const __height = {height};
            const __mediaType = {media_type};
            const __colorScheme = {color_scheme};
            Object.defineProperties(globalThis, {{
              innerWidth: {{ value: __width, writable: true, configurable: true }},
              innerHeight: {{ value: __height, writable: true, configurable: true }},
              devicePixelRatio: {{ value: 1, writable: true, configurable: true }},
            }});
            globalThis.screen = Object.assign(globalThis.screen || {{}}, {{
              width: __width,
              height: __height,
              availWidth: __width,
              availHeight: __height,
              colorDepth: 24,
              pixelDepth: 24,
            }});
            globalThis.visualViewport = Object.assign(globalThis.visualViewport || {{}}, {{
              offsetLeft: 0,
              offsetTop: 0,
              pageLeft: 0,
              pageTop: 0,
              width: __width,
              height: __height,
              scale: 1,
            }});
            Object.defineProperty(globalThis, '__vixenEmulatedMedia', {{
              value: {{ media: __mediaType, colorScheme: __colorScheme }},
              writable: true,
              configurable: true,
            }});
        }})()"#
    )
}

fn origin_for_url(raw: &str) -> String {
    let Ok(url) = url::Url::parse(raw) else {
        return "://".to_owned();
    };
    match url.origin() {
        url::Origin::Tuple(scheme, host, port) => format!("{scheme}://{host}:{port}"),
        url::Origin::Opaque(_) => "://".to_owned(),
    }
}

fn cdp_query_root_node_id(params: &Value, method: &str) -> Result<usize, String> {
    let Some(value) = params.get("nodeId") else {
        return Err(format!("{method}: missing `nodeId`"));
    };
    let node_id = value
        .as_u64()
        .ok_or_else(|| format!("{method}: `nodeId` must be a non-negative integer"))?;
    usize::try_from(node_id).map_err(|_| format!("{method}: `nodeId` is too large"))
}

fn cdp_document_node(
    state: &BrowsingContextState,
    elements: &[vixen_api::ElementInfo],
    depth: i64,
) -> Value {
    let child = elements.first().map(cdp_node_from_element);
    let child_node_count = usize::from(child.is_some());
    let mut node = json!({
        "nodeId": CDP_DOCUMENT_NODE_ID,
        "backendNodeId": CDP_DOCUMENT_NODE_ID,
        "nodeType": 9,
        "nodeName": "#document",
        "localName": "",
        "nodeValue": "",
        "childNodeCount": child_node_count,
        "documentURL": state.url,
        "baseURL": state.url,
        "xmlVersion": "",
    });
    if depth != 0
        && let Some(child) = child
    {
        node["children"] = json!([child]);
    }
    node
}

fn cdp_node_from_element(element: &vixen_api::ElementInfo) -> Value {
    let attributes = element
        .attributes
        .iter()
        .flat_map(|(name, value)| [json!(name), json!(value)])
        .collect::<Vec<_>>();
    let local_name = element.tag.to_ascii_lowercase();
    json!({
        "nodeId": element.node_id,
        "backendNodeId": element.node_id,
        "nodeType": 1,
        "nodeName": local_name.to_ascii_uppercase(),
        "localName": local_name,
        "nodeValue": "",
        "childNodeCount": 0,
        "attributes": attributes,
    })
}

fn element_cdp_bbox(element: vixen_api::ElementInfo) -> Option<(f64, f64, f64, f64)> {
    let (x, y, width, height) = element.bbox?;
    let width = width.max(0.0);
    let height = height.max(0.0);
    if width > 0.0 && height > 0.0 {
        return Some((x, y, width, height));
    }
    let (fallback_width, fallback_height) = match element.tag.to_ascii_lowercase().as_str() {
        "input" => (150.0, 20.0),
        "textarea" => (200.0, 60.0),
        "select" => (120.0, 20.0),
        "button" => (64.0, 24.0),
        _ => return Some((x, y, width, height)),
    };
    Some((
        x,
        y,
        if width > 0.0 { width } else { fallback_width },
        if height > 0.0 {
            height
        } else {
            fallback_height
        },
    ))
}

fn quad_from_bbox((x, y, width, height): (f64, f64, f64, f64)) -> Value {
    json!([x, y, x + width, y, x + width, y + height, x, y + height])
}

fn box_model_from_bbox(bbox: (f64, f64, f64, f64)) -> Value {
    let quad = quad_from_bbox(bbox);
    json!({
        "model": {
            "content": quad.clone(),
            "padding": quad.clone(),
            "border": quad.clone(),
            "margin": quad,
            "width": bbox.2,
            "height": bbox.3,
        }
    })
}

fn finite_param(params: &Value, name: &str) -> Result<f64, String> {
    let value = params
        .get(name)
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("Input.dispatchMouseEvent: missing `{name}`"))?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(format!("Input.dispatchMouseEvent: `{name}` must be finite"))
    }
}

fn optional_finite_param(params: &Value, name: &str) -> Result<f64, String> {
    match params.get(name) {
        Some(_) => finite_param(params, name),
        None => Ok(0.0),
    }
}

fn positive_u32_param(params: &Value, name: &str, method: &str) -> Result<u32, String> {
    let value = params
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{method}: missing `{name}`"))?;
    if value == 0 || value > u32::MAX as u64 {
        return Err(format!("{method}: `{name}` must be a positive u32"));
    }
    Ok(value as u32)
}

fn mouse_button(params: &Value) -> Result<MouseButton, String> {
    match params
        .get("button")
        .and_then(Value::as_str)
        .unwrap_or("none")
    {
        "none" => Ok(MouseButton::None),
        "left" => Ok(MouseButton::Left),
        "middle" => Ok(MouseButton::Middle),
        "right" => Ok(MouseButton::Right),
        _ => Err("Input.dispatchMouseEvent: unsupported mouse button".to_owned()),
    }
}

impl MouseButton {
    fn dom_button_code(self) -> i32 {
        match self {
            MouseButton::Left | MouseButton::None => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
        }
    }
}

fn capture_viewport(params: &Value, default_viewport: (u32, u32)) -> Result<(u32, u32), String> {
    let Some(clip) = params.get("clip") else {
        return Ok(default_viewport);
    };
    let x = clip.get("x").and_then(Value::as_f64).unwrap_or(0.0);
    let y = clip.get("y").and_then(Value::as_f64).unwrap_or(0.0);
    let scale = clip.get("scale").and_then(Value::as_f64).unwrap_or(1.0);
    if x != 0.0 || y != 0.0 || scale != 1.0 {
        return Err("Page.captureScreenshot: only full-viewport clip is supported".into());
    }
    let width = clip
        .get("width")
        .and_then(Value::as_f64)
        .ok_or_else(|| "Page.captureScreenshot: clip.width is required".to_owned())?;
    let height = clip
        .get("height")
        .and_then(Value::as_f64)
        .ok_or_else(|| "Page.captureScreenshot: clip.height is required".to_owned())?;
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return Err("Page.captureScreenshot: clip dimensions must be positive".into());
    }
    if width > i32::MAX as f64 || height > i32::MAX as f64 {
        return Err("Page.captureScreenshot: clip dimensions are too large".into());
    }
    Ok((width.round() as u32, height.round() as u32))
}

fn serialized_remote_object(serialized: &Value) -> Value {
    let kind = serialized
        .get("t")
        .and_then(Value::as_str)
        .unwrap_or("undefined");
    let value = serialized.get("v").cloned().unwrap_or(Value::Null);
    match kind {
        "undefined" => json!({ "type": "undefined", "description": "undefined" }),
        "string" => {
            json!({ "type": "string", "value": value, "description": value.as_str().unwrap_or_default() })
        }
        "number" => json!({ "type": "number", "value": value, "description": value.to_string() }),
        "boolean" => json!({ "type": "boolean", "value": value, "description": value.to_string() }),
        "object" if value.is_null() => {
            json!({ "type": "object", "subtype": "null", "value": Value::Null, "description": "null" })
        }
        "object" if value.is_array() => {
            json!({ "type": "object", "subtype": "array", "value": value, "description": "Array" })
        }
        "object" => json!({ "type": "object", "value": value, "description": "Object" }),
        _ => json!({ "type": kind, "value": value }),
    }
}

fn register_remote_ids(value: &Value, mut register: impl FnMut(&str)) {
    fn visit(value: &Value, register: &mut impl FnMut(&str)) {
        match value {
            Value::Array(values) => {
                for value in values {
                    visit(value, register);
                }
            }
            Value::Object(object) => {
                if let Some(object_id) = object.get("objectId").and_then(Value::as_str) {
                    register(object_id);
                }
                for value in object.values() {
                    visit(value, register);
                }
            }
            _ => {}
        }
    }
    visit(value, &mut register);
}

fn cdp_object_expr(object_id: &str) -> String {
    let object_id = serde_json::to_string(object_id).unwrap_or_else(|_| "\"\"".into());
    format!("(globalThis.__vixenCdpObjects && globalThis.__vixenCdpObjects[{object_id}])")
}

fn cdp_call_argument_expr(arg: &Value) -> String {
    if let Some(object_id) = arg.get("objectId").and_then(Value::as_str) {
        return cdp_object_expr(object_id);
    }
    if let Some(unserializable) = arg.get("unserializableValue").and_then(Value::as_str) {
        return match unserializable {
            "Infinity" => "Infinity".to_owned(),
            "-Infinity" => "-Infinity".to_owned(),
            "NaN" => "NaN".to_owned(),
            "-0" => "-0".to_owned(),
            _ => "undefined".to_owned(),
        };
    }
    if let Some(value) = arg.get("value") {
        return serde_json::to_string(value).unwrap_or_else(|_| "undefined".to_owned());
    }
    "undefined".to_owned()
}

// ---------------------------------------------------------------------------
// Wire types: CdpRequest / CdpResponse / CdpNotification
// ---------------------------------------------------------------------------

fn parse_cdp_request(raw: &str) -> Result<CdpRequest, String> {
    let value: Value = serde_json::from_str(raw)
        .map_err(|error| error_response(None, CdpError::parse(error.to_string())))?;
    serde_json::from_value(value)
        .map_err(|error| error_response(None, CdpError::new(-32600, error.to_string())))
}

fn is_async_navigation_method(method: &str) -> bool {
    matches!(
        method,
        "Target.createTarget"
            | "Page.navigate"
            | "Page.reload"
            | "Page.navigateToHistoryEntry"
            | "DOM.describeNode"
            | "DOM.resolveNode"
            | "DOM.getAttributes"
            | "DOM.getOuterHTML"
            | "DOM.setAttributeValue"
            | "DOM.removeAttribute"
            | "DOM.getContentQuads"
            | "DOM.getBoxModel"
            | "Runtime.evaluate"
            | "Runtime.callFunctionOn"
            | "Runtime.getProperties"
            | "Runtime.awaitPromise"
            | "Runtime.releaseObject"
            | "Runtime.releaseObjectGroup"
            | "Input.dispatchMouseEvent"
            | "Input.dispatchKeyEvent"
            | "Input.insertText"
    )
}

#[derive(Debug, Serialize, Deserialize)]
struct CdpRequest {
    id: u64,
    #[serde(default, rename = "sessionId")]
    session_id: Option<String>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct CdpResponse {
    id: u64,
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<CdpError>,
    // CDP has `result` OR `error`, never both.
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
}

#[derive(Debug, Serialize)]
struct CdpError {
    code: i32,
    message: String,
    data: CdpErrorData,
}

#[derive(Debug, Serialize)]
struct CdpErrorData {
    #[serde(rename = "vixenCode")]
    vixen_code: &'static str,
}

impl CdpError {
    fn new(code: i32, message: impl Into<String>) -> Self {
        let vixen_code = match code {
            -32700 => "cdp.parse-error",
            -32600 => "cdp.invalid-request",
            -32601 => "cdp.method-not-found",
            -32602 => "cdp.invalid-params",
            -32603 => "cdp.internal",
            -32001 => "cdp.invalid-session",
            _ => "cdp.invalid-state",
        };
        let detail = message.into();
        Self {
            code,
            message: format!("{vixen_code}: {detail}"),
            data: CdpErrorData { vixen_code },
        }
    }

    fn parse(message: impl Into<String>) -> Self {
        Self::new(-32700, message)
    }
}

#[derive(Debug, Serialize)]
struct CdpNotification {
    method: String,
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    params: Value,
}

fn error_response(id: Option<u64>, error: CdpError) -> String {
    serde_json::to_string(&json!({
        "id": id,
        "error": error,
    }))
    .unwrap_or_else(|_| {
        format!(
            "{{\"id\":{},\"error\":{{\"code\":-32603}}}}",
            id.unwrap_or(0)
        )
    })
}

fn notification(method: &str, params: Value, session_id: Option<&str>) -> String {
    serde_json::to_string(&CdpNotification {
        method: method.to_owned(),
        session_id: session_id.map(ToOwned::to_owned),
        params,
    })
    .unwrap_or_else(|_| "{}".into())
}

fn console_notification(
    event: RuntimeConsoleEvent,
    runtime_id: u64,
    session_id: Option<&str>,
) -> String {
    notification(
        "Runtime.consoleAPICalled",
        json!({
            "type": event.kind,
            "args": event.args.iter().map(remote_object_from_console_arg).collect::<Vec<_>>(),
            "executionContextId": runtime_id,
            "timestamp": now_ms(),
            "stackTrace": { "callFrames": [] },
        }),
        session_id,
    )
}

fn binding_notification(
    event: RuntimeBindingEvent,
    runtime_id: u64,
    session_id: Option<&str>,
) -> String {
    notification(
        "Runtime.bindingCalled",
        json!({
            "name": event.name,
            "payload": event.payload,
            "executionContextId": runtime_id,
        }),
        session_id,
    )
}

fn dialog_notification(
    event: RuntimeDialogEvent,
    frame_id: &str,
    url: &str,
    session_id: Option<&str>,
) -> String {
    notification(
        "Page.javascriptDialogOpening",
        json!({
            "url": url,
            "frameId": frame_id,
            "message": event.message,
            "type": event.kind,
            "hasBrowserHandler": true,
            "defaultPrompt": event.default_prompt,
        }),
        session_id,
    )
}

fn exception_thrown_notification(text: &str, code: &str, session_id: Option<&str>) -> String {
    notification(
        "Runtime.exceptionThrown",
        json!({
            "timestamp": now_ms(),
            "exceptionDetails": {
                "exceptionId": 1,
                "text": text,
                "lineNumber": 0,
                "columnNumber": 0,
                "code": code,
                "exception": {
                    "type": "object",
                    "subtype": "error",
                    "description": text,
                }
            }
        }),
        session_id,
    )
}

fn network_request_will_be_sent_notification(
    state: &BrowsingContextState,
    frame_id: &str,
    timestamp: u64,
    session_id: Option<&str>,
) -> String {
    notification(
        "Network.requestWillBeSent",
        json!({
            "requestId": network_request_id(state),
            "loaderId": loader_id(state),
            "documentURL": state.url,
            "request": {
                "url": state.url,
                "method": "GET",
                "headers": {},
                "mixedContentType": "none",
                "initialPriority": "VeryHigh",
                "referrerPolicy": "strict-origin-when-cross-origin",
            },
            "timestamp": timestamp,
            "wallTime": timestamp as f64 / 1000.0,
            "initiator": { "type": "other" },
            "type": "Document",
            "frameId": frame_id,
            "hasUserGesture": false,
        }),
        session_id,
    )
}

fn network_response_received_notification(
    state: &BrowsingContextState,
    frame_id: &str,
    timestamp: u64,
    session_id: Option<&str>,
) -> String {
    notification(
        "Network.responseReceived",
        json!({
            "requestId": network_request_id(state),
            "loaderId": loader_id(state),
            "timestamp": timestamp,
            "type": "Document",
            "frameId": frame_id,
            "response": {
                "url": state.url,
                "status": 200,
                "statusText": "OK",
                "headers": {},
                "mimeType": "text/html",
                "connectionReused": false,
                "connectionId": 0,
                "encodedDataLength": 0,
                "securityState": "neutral",
                "protocol": network_protocol_for_url(&state.url),
            },
            "hasExtraInfo": false,
        }),
        session_id,
    )
}

fn network_loading_finished_notification(
    state: &BrowsingContextState,
    timestamp: u64,
    session_id: Option<&str>,
) -> String {
    notification(
        "Network.loadingFinished",
        json!({
            "requestId": network_request_id(state),
            "timestamp": timestamp,
            "encodedDataLength": 0,
        }),
        session_id,
    )
}

fn network_fetch_notifications(
    event: RuntimeNetworkEvent,
    frame_id: &str,
    loader_id: u64,
    document_url: &str,
    timestamp: u64,
    session_id: Option<&str>,
) -> Vec<String> {
    match event {
        RuntimeNetworkEvent::Request {
            request_id,
            url,
            method,
        } => vec![notification(
            "Network.requestWillBeSent",
            json!({
                "requestId": request_id,
                "loaderId": format!("loader-{loader_id}"),
                "documentURL": document_url,
                "request": {
                    "url": url,
                    "method": method,
                    "headers": {},
                    "mixedContentType": "none",
                    "initialPriority": "High",
                    "referrerPolicy": "strict-origin-when-cross-origin",
                },
                "timestamp": timestamp,
                "wallTime": timestamp as f64 / 1000.0,
                "initiator": { "type": "script" },
                "type": "Fetch",
                "frameId": frame_id,
                "hasUserGesture": false,
            }),
            session_id,
        )],
        RuntimeNetworkEvent::Redirect {
            request_id,
            from,
            status,
            ..
        } => vec![network_fetch_response_received_notification(
            &request_id,
            &from,
            status,
            frame_id,
            loader_id,
            timestamp,
            session_id,
        )],
        RuntimeNetworkEvent::Response {
            request_id,
            url,
            status,
        } => vec![
            network_fetch_response_received_notification(
                &request_id,
                &url,
                status,
                frame_id,
                loader_id,
                timestamp,
                session_id,
            ),
            network_fetch_loading_finished_notification(&request_id, timestamp, session_id),
        ],
        RuntimeNetworkEvent::Failure {
            request_id,
            url,
            error_text,
            blocked_reason,
        } => vec![network_fetch_loading_failed_notification(
            &request_id,
            &url,
            &error_text,
            blocked_reason.as_deref(),
            timestamp,
            session_id,
        )],
    }
}

fn network_fetch_response_received_notification(
    request_id: &str,
    url: &str,
    status: u16,
    frame_id: &str,
    loader_id: u64,
    timestamp: u64,
    session_id: Option<&str>,
) -> String {
    notification(
        "Network.responseReceived",
        json!({
            "requestId": request_id,
            "loaderId": format!("loader-{loader_id}"),
            "timestamp": timestamp,
            "type": "Fetch",
            "frameId": frame_id,
            "response": {
                "url": url,
                "status": status,
                "statusText": http_status_text(status),
                "headers": {},
                "mimeType": "text/plain",
                "connectionReused": false,
                "connectionId": 0,
                "encodedDataLength": 0,
                "securityState": "neutral",
                "protocol": network_protocol_for_url(url),
            },
            "hasExtraInfo": false,
        }),
        session_id,
    )
}

fn network_fetch_loading_finished_notification(
    request_id: &str,
    timestamp: u64,
    session_id: Option<&str>,
) -> String {
    notification(
        "Network.loadingFinished",
        json!({
            "requestId": request_id,
            "timestamp": timestamp,
            "encodedDataLength": 0,
        }),
        session_id,
    )
}

fn network_fetch_loading_failed_notification(
    request_id: &str,
    url: &str,
    error_text: &str,
    blocked_reason: Option<&str>,
    timestamp: u64,
    session_id: Option<&str>,
) -> String {
    let mut params = serde_json::Map::new();
    params.insert("requestId".to_owned(), json!(request_id));
    params.insert("timestamp".to_owned(), json!(timestamp));
    params.insert("type".to_owned(), json!("Fetch"));
    params.insert("errorText".to_owned(), json!(error_text));
    params.insert("canceled".to_owned(), json!(false));
    if let Some(reason) = blocked_reason {
        params.insert("blockedReason".to_owned(), json!(reason));
    }
    params.insert("url".to_owned(), json!(url));
    notification("Network.loadingFailed", Value::Object(params), session_id)
}

fn http_status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "",
    }
}

fn network_request_id(state: &BrowsingContextState) -> String {
    format!("request-{}", state.document_id.get())
}

fn network_protocol_for_url(raw: &str) -> &'static str {
    match url::Url::parse(raw).ok().map(|url| url.scheme().to_owned()) {
        Some(scheme) if scheme == "https" => "h2",
        Some(scheme) if scheme == "http" => "http/1.1",
        Some(scheme) if scheme == "file" => "file",
        Some(scheme) if scheme == "about" => "about",
        _ => "unknown",
    }
}

fn remote_object_from_console_arg(arg: &RuntimeConsoleArg) -> Value {
    let mut object = serde_json::Map::new();
    object.insert("type".to_owned(), json!(arg.type_name));
    if let Some(subtype) = &arg.subtype {
        object.insert("subtype".to_owned(), json!(subtype));
    }
    if let Some(value) = &arg.value {
        object.insert("value".to_owned(), console_value_json(value));
    }
    if let Some(value) = &arg.unserializable_value {
        object.insert("unserializableValue".to_owned(), json!(value));
    }
    object.insert("description".to_owned(), json!(arg.description));
    Value::Object(object)
}

fn console_value_json(value: &RuntimeConsoleValue) -> Value {
    match value {
        RuntimeConsoleValue::String(value) => json!(value),
        RuntimeConsoleValue::Number(value) => json!(value),
        RuntimeConsoleValue::Bool(value) => json!(value),
        RuntimeConsoleValue::Null => Value::Null,
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Stable error code export — the CLI surfaces these verbatim. Kept as a
// documentation anchor for the stable-code contract that Runtime.evaluate
// failures carry (one of the codes from vixen_engine::engine_error::codes).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdp_explicit_profile_creates_database() {
        let parent = tempfile::tempdir().unwrap();
        let profile_dir = parent.path().join("cdp-profile");

        drop(CdpState::with_profile_dir(&profile_dir).unwrap());

        assert!(profile_dir.join("profile.redb").is_file());
    }

    fn dispatch_one(state: &mut CdpState, method: &str, params: Value) -> Value {
        let req = CdpRequest {
            id: 1,
            session_id: None,
            method: method.into(),
            params,
        };
        state.dispatch(&req).response.expect("success response")
    }

    fn dispatch_error(state: &mut CdpState, method: &str, params: Value) -> CdpError {
        let req = CdpRequest {
            id: 1,
            session_id: None,
            method: method.into(),
            params,
        };
        state.dispatch(&req).response.expect_err("CDP error")
    }

    fn spawn_fetch_server(
        host: &str,
        body: &str,
    ) -> (
        String,
        vixen_net::NetworkConfig,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let body = body.to_owned();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let mut config = vixen_net::NetworkConfig::default();
        config.dns_overrides.push((host.to_owned(), vec![addr]));
        (
            format!("http://{host}:{}/payload", addr.port()),
            config,
            handle,
        )
    }

    struct GatedNavigationRequest {
        path: String,
        respond: std::sync::mpsc::SyncSender<String>,
    }

    fn spawn_gated_navigation_server(
        host: &str,
        requests: usize,
    ) -> (
        String,
        vixen_net::NetworkConfig,
        tokio::sync::mpsc::UnboundedReceiver<GatedNavigationRequest>,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let (request_tx, request_rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = std::thread::spawn(move || {
            for _ in 0..requests {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut request = [0_u8; 2048];
                let read = stream.read(&mut request).unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..read]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_owned();
                let (respond, response) = std::sync::mpsc::sync_channel(1);
                if request_tx
                    .send(GatedNavigationRequest { path, respond })
                    .is_err()
                {
                    return;
                }
                let Ok(body) = response.recv_timeout(Duration::from_secs(10)) else {
                    return;
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        let mut config = vixen_net::NetworkConfig::default();
        config.dns_overrides.push((host.to_owned(), vec![addr]));
        (
            format!("http://{host}:{}", addr.port()),
            config,
            request_rx,
            handle,
        )
    }

    async fn receive_cdp_responses<S>(
        socket: &mut tokio_tungstenite::WebSocketStream<S>,
        expected_ids: &[u64],
    ) -> (Vec<u64>, HashMap<u64, Value>)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let mut order = Vec::new();
        let mut responses = HashMap::new();
        while responses.len() < expected_ids.len() {
            let message = socket
                .next()
                .await
                .expect("CDP socket response")
                .expect("valid CDP socket message");
            if !message.is_text() {
                continue;
            }
            let value: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            let Some(id) = value.get("id").and_then(Value::as_u64) else {
                continue;
            };
            if expected_ids.contains(&id) {
                order.push(id);
                responses.insert(id, value);
            }
        }
        (order, responses)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn socket_navigation_adopts_author_successor_and_drains_destination_effects() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (origin, network, mut requests, server) =
                    spawn_gated_navigation_server("vixen-cdp-successor.com", 2);
                let runtime = JsRuntime::with_network_config(network).expect("JS runtime");
                let state = Rc::new(RefCell::new(CdpState::with_runtime(runtime)));
                {
                    let mut state = state.borrow_mut();
                    dispatch_one(&mut state, "Runtime.enable", json!({}));
                    dispatch_one(&mut state, "Page.enable", json!({}));
                    dispatch_one(&mut state, "Network.enable", json!({}));
                    dispatch_one(
                        &mut state,
                        "Runtime.addBinding",
                        json!({ "name": "destinationBinding" }),
                    );
                }

                let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
                let address = listener.local_addr().unwrap();
                let server_state = Rc::clone(&state);
                let connection = tokio::task::spawn_local(async move {
                    let (stream, _) = listener.accept().await.unwrap();
                    handle_connection(stream, server_state).await.unwrap();
                });
                let (mut socket, _) =
                    tokio_tungstenite::connect_async(format!("ws://{address}"))
                        .await
                        .unwrap();
                socket
                    .send(Message::text(
                        json!({
                            "id": 5,
                            "method": "Page.navigate",
                            "params": { "url": format!("{origin}/first") }
                        })
                        .to_string(),
                    ))
                    .await
                    .unwrap();

                let first = tokio::time::timeout(Duration::from_secs(5), requests.recv())
                    .await
                    .expect("first navigation reached server")
                    .expect("first navigation request");
                assert_eq!(first.path, "/first");
                first
                    .respond
                    .send(
                        "<script>console.log('intermediate-effect'); fetch('data:text/plain,intermediate-network'); location.assign('/successor')</script>"
                            .to_owned(),
                    )
                    .unwrap();
                let successor = tokio::time::timeout(Duration::from_secs(5), requests.recv())
                    .await
                    .expect("author successor reached server")
                    .expect("author successor request");
                assert_eq!(successor.path, "/successor");
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), socket.next())
                        .await
                        .is_err(),
                    "initiating response completed before its active successor"
                );
                successor
                    .respond
                    .send(
                        "<script>console.log('destination-effect'); destinationBinding('bound-effect'); alert('dialog-effect'); fetch('data:text/plain,network-effect'); throw new Error('exception-effect')</script>"
                            .to_owned(),
                    )
                    .unwrap();

                let deadline = Instant::now() + Duration::from_secs(5);
                let mut messages = Vec::new();
                loop {
                    let message = tokio::time::timeout(
                        deadline.saturating_duration_since(Instant::now()),
                        socket.next(),
                    )
                    .await
                    .expect("navigation response and destination notifications")
                    .expect("socket message")
                    .expect("valid socket message");
                    if !message.is_text() {
                        continue;
                    }
                    messages.push(
                        serde_json::from_str::<Value>(message.to_text().unwrap()).unwrap(),
                    );
                    let methods = messages
                        .iter()
                        .filter_map(|message| message["method"].as_str())
                        .collect::<Vec<_>>();
                    if messages.iter().any(|message| message["id"] == 5)
                        && [
                            "Runtime.consoleAPICalled",
                            "Runtime.bindingCalled",
                            "Page.javascriptDialogOpening",
                            "Runtime.exceptionThrown",
                            "Network.requestWillBeSent",
                        ]
                        .iter()
                        .all(|expected| methods.contains(expected))
                    {
                        break;
                    }
                }
                assert_eq!(messages[0]["id"], 5, "response must precede notifications");
                assert!(messages.iter().any(|message| {
                    message["method"] == "Runtime.consoleAPICalled"
                        && message["params"]["args"][0]["value"] == "destination-effect"
                }));
                assert!(messages.iter().any(|message| {
                    message["method"] == "Runtime.bindingCalled"
                        && message["params"]["payload"] == "bound-effect"
                }));
                let final_state = state.borrow_mut().current_state().unwrap();
                let intermediate_console = messages
                    .iter()
                    .find(|message| {
                        message["method"] == "Runtime.consoleAPICalled"
                            && message["params"]["args"][0]["value"] == "intermediate-effect"
                    })
                    .expect("intermediate destination console effect");
                assert_ne!(
                    intermediate_console["params"]["executionContextId"],
                    final_state.runtime_context_id.unwrap().get()
                );
                let intermediate_network = messages
                    .iter()
                    .find(|message| {
                        message["method"] == "Network.requestWillBeSent"
                            && message["params"]["request"]["url"]
                                == "data:text/plain,intermediate-network"
                    })
                    .expect("intermediate destination network effect");
                assert_eq!(
                    intermediate_network["params"]["documentURL"],
                    format!("{origin}/first")
                );
                assert_ne!(
                    intermediate_network["params"]["loaderId"],
                    loader_id(&final_state)
                );

                socket.close(None).await.unwrap();
                connection.await.unwrap();
                drop(state);
                server.join().unwrap();
            })
            .await;
    }

    #[test]
    fn socket_operation_never_adopts_successor_claimed_by_another_wire_request() {
        let mut state = CdpState::default();
        let first_request = CdpRequest {
            id: 1,
            session_id: None,
            method: "Page.navigate".to_owned(),
            params: json!({ "url": "https://claimed.test/first" }),
        };
        let mut first = match state.start_socket_operation(&first_request) {
            Ok(Some(operation)) => operation,
            _ => panic!("first continuation did not start"),
        };
        let second_request = CdpRequest {
            id: 2,
            session_id: None,
            method: "Page.navigate".to_owned(),
            params: json!({ "url": "https://claimed.test/second" }),
        };
        let second = match state.start_socket_operation(&second_request) {
            Ok(Some(operation)) => operation,
            _ => panic!("second continuation did not start"),
        };
        state.pump_core_events().unwrap();
        let first_navigation_id = final_cross_document_id(&first.navigation_actions).unwrap();
        let second_navigation_id = final_cross_document_id(&second.navigation_actions).unwrap();
        state
            .pending_core_navigations
            .get_mut(&second_navigation_id)
            .unwrap()
            .predecessor_navigation_id = Some(first_navigation_id);

        state.adopt_active_socket_successor(&mut first).unwrap();
        assert_eq!(cross_document_ids(&first.navigation_actions).len(), 1);
        assert_eq!(cross_document_ids(&second.navigation_actions).len(), 1);

        let mut operations = VecDeque::from([PendingCdpOperation {
            request: first_request,
            started: Instant::now(),
            deadline: Instant::now() + Duration::from_secs(1),
            operation: first,
        }]);
        assert_eq!(state.poll_socket_operations(&mut operations).len(), 1);
        assert!(operations.is_empty());
        assert!(state.pending_core_navigations[&second_navigation_id].wire_claimed);

        state.cancel_socket_operation(&second);
    }

    #[test]
    fn socket_operation_adopts_terminal_and_active_successor_chain_in_order() {
        let mut state = CdpState::default();
        let context_id = state.current_context().unwrap();
        let first = NavigationId::new(100).unwrap();
        let second = NavigationId::new(101).unwrap();
        let third = NavigationId::new(102).unwrap();
        for (navigation_id, predecessor_navigation_id, terminal, wire_claimed) in [
            (first, None, Some(Ok(())), true),
            (second, Some(first), Some(Ok(())), false),
            (third, Some(second), None, false),
        ] {
            state.pending_core_navigations.insert(
                navigation_id,
                PendingCoreNavigation {
                    context_id,
                    predecessor_navigation_id,
                    kind: CrossDocumentNavigationKind::Regular,
                    committed: terminal.is_some(),
                    terminal,
                    wire_claimed,
                    abandoned: false,
                },
            );
        }
        let mut operation = StartedSocketOperation {
            context_id,
            navigation_actions: vec![NavigationActionOutcome::CrossDocument {
                navigation_id: first,
                kind: CrossDocumentNavigationKind::Regular,
            }],
            completion: SocketCompletion::TargetCreate,
        };

        state.adopt_active_socket_successor(&mut operation).unwrap();

        assert_eq!(
            cross_document_ids(&operation.navigation_actions),
            vec![first, second, third]
        );
        assert!(state.pending_core_navigations[&second].wire_claimed);
        assert!(state.pending_core_navigations[&third].wire_claimed);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn expired_socket_operation_cancels_and_consumes_exact_terminal() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (origin, network, mut requests, server) =
                    spawn_gated_navigation_server("vixen-cdp-timeout.com", 1);
                let runtime = JsRuntime::with_network_config(network).expect("JS runtime");
                let mut state = CdpState::with_runtime(runtime);
                let context_id = state.current_context().unwrap();
                let request = CdpRequest {
                    id: 1,
                    session_id: None,
                    method: "Page.navigate".to_owned(),
                    params: json!({ "url": format!("{origin}/slow") }),
                };
                let operation = match state.start_socket_operation(&request) {
                    Ok(Some(operation)) => operation,
                    _ => panic!("navigation continuation did not start"),
                };
                let navigation_id = final_cross_document_id(&operation.navigation_actions).unwrap();
                let slow = tokio::time::timeout(Duration::from_secs(5), requests.recv())
                    .await
                    .expect("navigation reached server")
                    .expect("navigation request");
                let mut pending = VecDeque::from([PendingCdpOperation {
                    request,
                    started: Instant::now(),
                    deadline: Instant::now() - Duration::from_millis(1),
                    operation,
                }]);

                let completed = state.poll_socket_operations(&mut pending);
                assert_eq!(completed.len(), 1);
                assert!(pending.is_empty());
                assert!(!state.pending_core_navigations.contains_key(&navigation_id));
                assert_eq!(
                    state
                        .context_state(context_id)
                        .unwrap()
                        .active_navigation_id,
                    None
                );
                let _ = slow
                    .respond
                    .send("<!doctype html><title>Too late</title>".to_owned());
                tokio::time::sleep(Duration::from_millis(20)).await;
                state.pump_core_events().unwrap();
                assert_eq!(state.context_state(context_id).unwrap().url, "about:blank");
                drop(state);
                server.join().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn disconnect_drops_pending_destination_and_cancellation_effects() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (origin, network, mut requests, server) =
                    spawn_gated_navigation_server("vixen-cdp-disconnect-effects.com", 2);
                let runtime = JsRuntime::with_network_config(network).expect("JS runtime");
                let state = Rc::new(RefCell::new(CdpState::with_runtime(runtime)));
                dispatch_one(&mut state.borrow_mut(), "Runtime.enable", json!({}));

                let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
                let address = listener.local_addr().unwrap();
                let server_state = Rc::clone(&state);
                let connection = tokio::task::spawn_local(async move {
                    let (stream, _) = listener.accept().await.unwrap();
                    handle_connection(stream, server_state).await.unwrap();
                });
                let (mut socket, _) =
                    tokio_tungstenite::connect_async(format!("ws://{address}"))
                        .await
                        .unwrap();
                socket
                    .send(Message::text(
                        json!({
                            "id": 1,
                            "method": "Page.navigate",
                            "params": { "url": format!("{origin}/first") }
                        })
                        .to_string(),
                    ))
                    .await
                    .unwrap();
                let first = tokio::time::timeout(Duration::from_secs(5), requests.recv())
                    .await
                    .expect("first navigation reached server")
                    .expect("first navigation request");
                first
                    .respond
                    .send(
                        "<script>console.log('disconnect-effect'); location.assign('/slow')</script>"
                            .to_owned(),
                    )
                    .unwrap();
                let slow = tokio::time::timeout(Duration::from_secs(5), requests.recv())
                    .await
                    .expect("successor reached server")
                    .expect("successor request");

                socket.close(None).await.unwrap();
                connection.await.unwrap();
                {
                    let state = state.borrow();
                    assert!(state.pending_effects.is_empty());
                    assert!(state
                        .context_presentations
                        .values()
                        .all(|presentation| presentation.pending_effects.is_empty()));
                }
                let _ = slow
                    .respond
                    .send("<!doctype html><title>Too late</title>".to_owned());
                let lines = state.borrow_mut().handle_text_sync(
                    &json!({
                        "id": 2,
                        "method": "Runtime.evaluate",
                        "params": { "expression": "'clean'" }
                    })
                    .to_string(),
                );
                assert!(lines.iter().all(|line| !line.contains("disconnect-effect")));

                drop(state);
                server.join().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn socket_navigation_and_reload_can_be_stopped_before_source_completion() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (origin, network, mut gated_requests, gated_server) =
                    spawn_gated_navigation_server("vixen-cdp-stop.com", 3);
                let runtime = JsRuntime::with_network_config(network).expect("JS runtime");
                let state = Rc::new(RefCell::new(CdpState::with_runtime(runtime)));
                let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
                let address = listener.local_addr().unwrap();
                let server_state = Rc::clone(&state);
                let connection = tokio::task::spawn_local(async move {
                    let (stream, _) = listener.accept().await.unwrap();
                    handle_connection(stream, server_state).await.unwrap();
                });
                let (mut socket, _) =
                    tokio_tungstenite::connect_async(format!("ws://{address}"))
                        .await
                        .unwrap();

                socket
                    .send(Message::text(
                        json!({ "id": 1, "method": "Page.navigate", "params": { "url": format!("{origin}/slow") } }).to_string(),
                    ))
                    .await
                    .unwrap();
                let slow = tokio::time::timeout(Duration::from_secs(2), gated_requests.recv())
                    .await
                    .expect("slow navigation reached server")
                    .expect("slow navigation request");
                assert_eq!(slow.path, "/slow");
                socket
                    .send(Message::text(
                        json!({ "id": 2, "method": "Page.stopLoading", "params": {} })
                            .to_string(),
                    ))
                    .await
                    .unwrap();
                let (order, responses) = tokio::time::timeout(
                    Duration::from_secs(2),
                    receive_cdp_responses(&mut socket, &[1, 2]),
                )
                .await
                .expect("stop and cancelled navigation responses");
                assert_eq!(order, vec![2, 1]);
                assert_eq!(responses[&2]["result"], json!({}));
                assert!(
                    responses[&1]["error"]["message"]
                        .as_str()
                        .unwrap()
                        .contains("cancelled")
                );
                slow.respond
                    .send("<!doctype html><title>Too late</title>".to_owned())
                    .unwrap();

                socket
                    .send(Message::text(
                        json!({ "id": 3, "method": "Page.navigate", "params": { "url": format!("{origin}/stable") } }).to_string(),
                    ))
                    .await
                    .unwrap();
                let stable = tokio::time::timeout(Duration::from_secs(2), gated_requests.recv())
                    .await
                    .expect("stable navigation reached server")
                    .expect("stable navigation request");
                assert_eq!(stable.path, "/stable");
                stable
                    .respond
                    .send("<!doctype html><title>Stable</title>".to_owned())
                    .unwrap();
                let (_, responses) = tokio::time::timeout(
                    Duration::from_secs(2),
                    receive_cdp_responses(&mut socket, &[3]),
                )
                .await
                .expect("stable navigation response");
                assert_eq!(responses[&3]["result"]["frameId"], "tab-1");

                socket
                    .send(Message::text(
                        json!({ "id": 4, "method": "Page.reload", "params": {} }).to_string(),
                    ))
                    .await
                    .unwrap();
                let reload = tokio::time::timeout(Duration::from_secs(2), gated_requests.recv())
                    .await
                    .expect("reload reached server")
                    .expect("reload request");
                assert_eq!(reload.path, "/stable");
                socket
                    .send(Message::text(
                        json!({ "id": 5, "method": "Page.stopLoading", "params": {} })
                            .to_string(),
                    ))
                    .await
                    .unwrap();
                let (order, responses) = tokio::time::timeout(
                    Duration::from_secs(2),
                    receive_cdp_responses(&mut socket, &[4, 5]),
                )
                .await
                .expect("stop and cancelled reload responses");
                assert_eq!(order, vec![5, 4]);
                assert_eq!(responses[&5]["result"], json!({}));
                assert!(
                    responses[&4]["error"]["message"]
                        .as_str()
                        .unwrap()
                        .contains("cancelled")
                );
                reload
                    .respond
                    .send("<!doctype html><title>Too late reload</title>".to_owned())
                    .unwrap();

                socket.close(None).await.unwrap();
                connection.await.unwrap();
                drop(state);
                gated_server.join().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn socket_history_and_runtime_navigation_can_be_stopped_without_poisoning_later_work() {
        const TEST_TIMEOUT: Duration = Duration::from_secs(5);
        tokio::task::LocalSet::new()
            .run_until(async {
                let (origin, network, mut gated_requests, gated_server) =
                    spawn_gated_navigation_server("vixen-cdp-actions.com", 7);
                let runtime = JsRuntime::with_network_config(network).expect("JS runtime");
                let state = Rc::new(RefCell::new(CdpState::with_runtime(runtime)));
                let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
                let address = listener.local_addr().unwrap();
                let server_state = Rc::clone(&state);
                let connection = tokio::task::spawn_local(async move {
                    let (stream, _) = listener.accept().await.unwrap();
                    handle_connection(stream, server_state).await.unwrap();
                });
                let (mut socket, _) =
                    tokio_tungstenite::connect_async(format!("ws://{address}"))
                        .await
                        .unwrap();

                for (id, path, title) in [(10, "/one", "One"), (11, "/two", "Two")] {
                    socket
                        .send(Message::text(
                            json!({ "id": id, "method": "Page.navigate", "params": { "url": format!("{origin}{path}") } }).to_string(),
                        ))
                        .await
                        .unwrap();
                    let request = tokio::time::timeout(TEST_TIMEOUT, gated_requests.recv())
                        .await
                        .expect("setup navigation reached server")
                        .expect("setup navigation request");
                    assert_eq!(request.path, path);
                    request
                        .respond
                        .send(format!("<!doctype html><title>{title}</title>"))
                        .unwrap();
                    let (_, responses) = tokio::time::timeout(
                        TEST_TIMEOUT,
                        receive_cdp_responses(&mut socket, &[id]),
                    )
                    .await
                    .expect("setup navigation response");
                    assert!(responses[&id].get("error").is_none());
                }

                socket
                    .send(Message::text(
                        json!({ "id": 12, "method": "Page.getNavigationHistory", "params": {} })
                            .to_string(),
                    ))
                    .await
                    .unwrap();
                let (_, history_response) = receive_cdp_responses(&mut socket, &[12]).await;
                let first_entry = history_response[&12]["result"]["entries"][0]["id"]
                    .as_u64()
                    .unwrap();

                socket
                    .send(Message::text(
                        json!({ "id": 13, "method": "Page.navigateToHistoryEntry", "params": { "entryId": first_entry } }).to_string(),
                    ))
                    .await
                    .unwrap();
                let history = tokio::time::timeout(TEST_TIMEOUT, gated_requests.recv())
                    .await
                    .expect("history navigation reached server")
                    .expect("history navigation request");
                assert_eq!(history.path, "/one");
                socket
                    .send(Message::text(
                        json!({ "id": 14, "method": "Page.stopLoading", "params": {} })
                            .to_string(),
                    ))
                    .await
                    .unwrap();
                let (order, responses) = tokio::time::timeout(
                    TEST_TIMEOUT,
                    receive_cdp_responses(&mut socket, &[13, 14]),
                )
                .await
                .expect("history stop responses");
                assert_eq!(order, vec![14, 13]);
                assert!(responses[&13]["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("cancelled"));
                history
                    .respond
                    .send("<!doctype html><title>Late history</title>".to_owned())
                    .unwrap();

                socket
                    .send(Message::text(
                        json!({
                            "id": 15,
                            "method": "Runtime.evaluate",
                            "params": { "expression": format!(
                                "location.assign({:?}); location.assign({:?}); 'queued'",
                                format!("{origin}/superseded"),
                                format!("{origin}/runtime")
                            ) }
                        })
                        .to_string(),
                    ))
                    .await
                    .unwrap();
                let runtime = tokio::time::timeout(TEST_TIMEOUT, gated_requests.recv())
                    .await
                    .expect("runtime navigation reached server")
                    .expect("runtime navigation request");
                assert_eq!(runtime.path, "/runtime");
                socket
                    .send(Message::text(
                        json!({ "id": 16, "method": "Page.stopLoading", "params": {} })
                            .to_string(),
                    ))
                    .await
                    .unwrap();
                let (order, responses) = tokio::time::timeout(
                    TEST_TIMEOUT,
                    receive_cdp_responses(&mut socket, &[15, 16]),
                )
                .await
                .expect("runtime stop responses");
                assert_eq!(order, vec![16, 15]);
                assert!(responses[&15]["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("cancelled"));
                runtime
                    .respond
                    .send("<!doctype html><title>Late runtime</title>".to_owned())
                    .unwrap();

                socket
                    .send(Message::text(
                        json!({ "id": 17, "method": "Page.navigate", "params": { "url": format!("{origin}/stable") } }).to_string(),
                    ))
                    .await
                    .unwrap();
                let stable = tokio::time::timeout(TEST_TIMEOUT, gated_requests.recv())
                    .await
                    .expect("later navigation reached server")
                    .expect("later navigation request");
                assert_eq!(stable.path, "/stable");
                stable
                    .respond
                    .send("<!doctype html><title>Stable</title>".to_owned())
                    .unwrap();
                let (_, responses) = tokio::time::timeout(
                    TEST_TIMEOUT,
                    receive_cdp_responses(&mut socket, &[17]),
                )
                .await
                .expect("later navigation response");
                assert!(responses[&17].get("error").is_none());

                socket
                    .send(Message::text(
                        json!({ "id": 18, "method": "Target.createTarget", "params": { "url": format!("{origin}/target") } }).to_string(),
                    ))
                    .await
                    .unwrap();
                let target = tokio::time::timeout(TEST_TIMEOUT, gated_requests.recv())
                    .await
                    .expect("target navigation reached server")
                    .expect("target navigation request");
                assert_eq!(target.path, "/target");
                socket
                    .send(Message::text(
                        json!({ "id": 19, "method": "Browser.getVersion", "params": {} })
                            .to_string(),
                    ))
                    .await
                    .unwrap();
                let (order, responses) = tokio::time::timeout(
                    TEST_TIMEOUT,
                    receive_cdp_responses(&mut socket, &[19]),
                )
                .await
                .expect("request behind target creation response");
                assert_eq!(order, vec![19]);
                assert!(responses[&19]["result"]["product"]
                    .as_str()
                    .unwrap()
                    .starts_with("Vixen/"));
                target
                    .respond
                    .send("<!doctype html><title>Target</title>".to_owned())
                    .unwrap();
                let (_, responses) = tokio::time::timeout(
                    TEST_TIMEOUT,
                    receive_cdp_responses(&mut socket, &[18]),
                )
                .await
                .expect("target creation response");
                assert!(responses[&18]["result"]["targetId"]
                    .as_str()
                    .unwrap()
                    .starts_with("tab-"));

                let retained_targets = state.borrow().targets.len();
                socket
                    .send(Message::text(
                        json!({ "id": 20, "method": "Target.createTarget", "params": { "url": format!("{origin}/abandoned-target") } }).to_string(),
                    ))
                    .await
                    .unwrap();
                let abandoned_target = tokio::time::timeout(TEST_TIMEOUT, gated_requests.recv())
                    .await
                    .expect("abandoned target navigation reached server")
                    .expect("abandoned target request");
                assert_eq!(abandoned_target.path, "/abandoned-target");
                socket.close(None).await.unwrap();
                connection.await.unwrap();
                abandoned_target
                    .respond
                    .send("<!doctype html><title>Too late</title>".to_owned())
                    .unwrap();
                let state = state.borrow();
                assert_eq!(state.targets.len(), retained_targets);
                assert_eq!(state.context_presentations.len(), retained_targets);
                assert!(state.attached_sessions.iter().all(|session| state
                    .targets
                    .iter()
                    .any(|target| target.context_id == session.context_id)));
                drop(state);
                gated_server.join().unwrap();
            })
            .await;
    }

    /// All CDP dispatcher checks except runtime-backed `Runtime.evaluate`,
    /// which lives in `tests/cdp_runtime.rs` for focused end-to-end coverage.
    #[test]
    fn cdp_dispatcher_surface() {
        // No JsRuntime: the dispatcher lazily inits one when first needed,
        // and none of these methods need it.
        let mut s = CdpState::default();

        // Browser.getVersion — stable product string.
        let v = dispatch_one(&mut s, "Browser.getVersion", json!({}));
        assert_eq!(v["protocolVersion"], "1.3");
        assert!(v["product"].as_str().unwrap().starts_with("Vixen/"));
        assert_eq!(v["jsVersion"], "V8 (deno_core)");

        let v = dispatch_one(
            &mut s,
            "Browser.setDownloadBehavior",
            json!({ "behavior": "allow", "downloadPath": "/tmp/vixen-downloads", "eventsEnabled": true }),
        );
        assert_eq!(v, json!({}));
        assert_eq!(s.download_behavior.policy, DownloadPolicy::Allow);
        assert_eq!(
            s.download_behavior.download_path.as_deref(),
            Some("/tmp/vixen-downloads")
        );
        assert!(s.download_behavior.events_enabled);

        let req = CdpRequest {
            id: 4,
            session_id: None,
            method: "Browser.setDownloadBehavior".into(),
            params: json!({ "behavior": "allow" }),
        };
        let err = s
            .dispatch(&req)
            .response
            .expect_err("missing downloadPath should fail");
        assert_eq!(err.code, -32602);

        // Target.createTarget — returns a targetId.
        let v = dispatch_one(
            &mut s,
            "Target.createTarget",
            json!({ "url": "about:blank" }),
        );
        assert!(v["targetId"].as_str().unwrap().starts_with("tab-"));

        // Target.attachToTarget — returns a sessionId.
        let v = dispatch_one(
            &mut s,
            "Target.attachToTarget",
            json!({ "targetId": "tab-1" }),
        );
        assert_eq!(v["sessionId"], "sess-3");

        let v = dispatch_one(&mut s, "Target.getTargets", json!({}));
        assert_eq!(v["targetInfos"].as_array().unwrap().len(), 2);

        let req = CdpRequest {
            id: 2,
            session_id: Some("sess-1".into()),
            method: "Runtime.enable".into(),
            params: json!({}),
        };
        let outcome = s.dispatch(&req);
        assert!(outcome.response.is_ok());
        let notif: Value = serde_json::from_str(&outcome.notifications[0]).unwrap();
        assert_eq!(notif["sessionId"], "sess-1");
        assert_eq!(notif["method"], "Runtime.executionContextCreated");

        let req = CdpRequest {
            id: 3,
            session_id: None,
            method: "Page.enable".into(),
            params: json!({}),
        };
        assert!(s.dispatch(&req).response.is_ok());

        // Page.navigate — returns success and queues lifecycle/load events.
        let req = CdpRequest {
            id: 1,
            session_id: None,
            method: "Page.navigate".into(),
            params: json!({ "url": "about:blank" }),
        };
        let outcome = s.dispatch(&req);
        let result = outcome.response.expect("navigate response");
        assert_eq!(result["frameId"], "tab-1");
        let load_event = outcome
            .notifications
            .iter()
            .map(|line| serde_json::from_str::<Value>(line).expect("notification JSON"))
            .find(|notif| notif["method"] == "Page.loadEventFired")
            .expect("load event notification");
        assert!(load_event["params"]["timestamp"].as_u64().is_some());

        let metrics = dispatch_one(&mut s, "Page.getLayoutMetrics", json!({}));
        assert_eq!(metrics["cssLayoutViewport"]["clientWidth"], 800);
        assert_eq!(metrics["cssLayoutViewport"]["clientHeight"], 600);
        assert_eq!(metrics["cssVisualViewport"]["scale"], 1);
        assert_eq!(metrics["cssContentSize"]["width"], 800.0);
        assert_eq!(metrics["cssContentSize"]["height"], 600.0);

        dispatch_one(
            &mut s,
            "Emulation.setDeviceMetricsOverride",
            json!({ "width": 500, "height": 320, "deviceScaleFactor": 1, "mobile": false }),
        );
        let metrics = dispatch_one(&mut s, "Page.getLayoutMetrics", json!({}));
        assert_eq!(metrics["cssLayoutViewport"]["clientWidth"], 500);
        assert_eq!(metrics["cssLayoutViewport"]["clientHeight"], 320);
        assert_eq!(metrics["cssContentSize"]["width"], 500.0);
        dispatch_one(&mut s, "Emulation.clearDeviceMetricsOverride", json!({}));
        let metrics = dispatch_one(&mut s, "Page.getLayoutMetrics", json!({}));
        assert_eq!(metrics["cssLayoutViewport"]["clientWidth"], 800);

        // Unknown method → JSON-RPC -32601.
        let req = CdpRequest {
            id: 99,
            session_id: None,
            method: "Foo.bar".into(),
            params: json!({}),
        };
        let err = s.dispatch(&req).response.expect_err("unknown method error");
        assert_eq!(err.code, -32601);

        // Malformed JSON → JSON-RPC -32700.
        let out = s.handle_text_sync("not json");
        let parsed: Value = serde_json::from_str(&out[0]).unwrap();
        assert_eq!(parsed["error"]["code"], -32700);

        // Wire path echoes flattened-session ids Playwright sends.
        let out = s.handle_text_sync(
            &json!({
                "id": 101,
                "sessionId": "sess-flat",
                "method": "Page.getFrameTree",
                "params": {}
            })
            .to_string(),
        );
        let parsed: Value = serde_json::from_str(&out[0]).unwrap();
        assert_eq!(parsed["sessionId"], "sess-flat");
    }

    #[test]
    fn target_create_attaches_before_response_and_close_detaches() {
        let mut s = CdpState::default();
        let create = serde_json::to_string(&json!({
            "id": 1,
            "method": "Target.createTarget",
            "params": { "url": "about:blank" },
        }))
        .unwrap();

        let lines = s.handle_text_sync(&create);
        assert!(lines.len() >= 2);
        let attached: Value = serde_json::from_str(&lines[0]).unwrap();
        let response: Value = serde_json::from_str(&lines[1]).unwrap();
        let target_id = response["result"]["targetId"].as_str().unwrap();
        assert_eq!(attached["method"], "Target.attachedToTarget");
        assert_eq!(attached["params"]["targetInfo"]["targetId"], target_id);

        let close = serde_json::to_string(&json!({
            "id": 2,
            "method": "Target.closeTarget",
            "params": { "targetId": target_id },
        }))
        .unwrap();
        let lines = s.handle_text_sync(&close);
        assert_eq!(lines.len(), 2);
        let response: Value = serde_json::from_str(&lines[0]).unwrap();
        let detached: Value = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(response["result"]["success"], true);
        assert_eq!(detached["method"], "Target.detachedFromTarget");
        assert_eq!(detached["params"]["targetId"], target_id);
        assert_eq!(s.targets.len(), 1, "the initial target remains open");
    }

    #[test]
    fn target_attach_routes_new_session_to_requested_target_and_detaches() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one.html");
        let two = dir.path().join("two.html");
        std::fs::write(&one, "<title>One</title>").unwrap();
        std::fs::write(&two, "<title>Two</title>").unwrap();

        let mut s = CdpState::default();
        let first = dispatch_one(
            &mut s,
            "Target.createTarget",
            json!({ "url": format!("file://{}", one.display()) }),
        );
        let second = dispatch_one(
            &mut s,
            "Target.createTarget",
            json!({ "url": format!("file://{}", two.display()) }),
        );
        assert_eq!(first["targetId"], "tab-2");
        assert_eq!(second["targetId"], "tab-3");

        let attached = dispatch_one(
            &mut s,
            "Target.attachToTarget",
            json!({ "targetId": second["targetId"].as_str().unwrap(), "flatten": true }),
        );
        let session_id = attached["sessionId"].as_str().unwrap();
        assert_eq!(session_id, "sess-4");

        let frame_req = CdpRequest {
            id: 11,
            session_id: Some(session_id.to_owned()),
            method: "Page.getFrameTree".to_owned(),
            params: json!({}),
        };
        let frame = s.dispatch(&frame_req).response.expect("frame tree");
        assert_eq!(frame["frameTree"]["frame"]["id"], "tab-3");
        assert!(
            frame["frameTree"]["frame"]["url"]
                .as_str()
                .unwrap()
                .ends_with("two.html")
        );

        let detach_req = CdpRequest {
            id: 12,
            session_id: None,
            method: "Target.detachFromTarget".to_owned(),
            params: json!({ "sessionId": session_id }),
        };
        let detached = s.dispatch(&detach_req);
        assert_eq!(detached.response.expect("detach"), json!({}));
        let notification: Value = serde_json::from_str(&detached.notifications[0]).unwrap();
        assert_eq!(notification["method"], "Target.detachedFromTarget");
        assert_eq!(notification["params"]["sessionId"], "sess-4");
        assert_eq!(notification["params"]["targetId"], "tab-3");
        assert_eq!(s.targets.len(), 3, "detach must not close the target");
        assert!(
            s.attached_sessions.is_empty(),
            "detach drops only the routed session"
        );
    }

    #[test]
    fn navigation_notifies_every_session_attached_to_the_target() {
        let mut s = CdpState::default();
        let attached = dispatch_one(
            &mut s,
            "Target.attachToTarget",
            json!({ "targetId": "tab-1", "flatten": true }),
        );
        assert_eq!(attached["sessionId"], "sess-2");
        for session_id in ["sess-1", "sess-2"] {
            let enabled = CdpRequest {
                id: 1,
                session_id: Some(session_id.to_owned()),
                method: "Runtime.enable".to_owned(),
                params: json!({}),
            };
            s.dispatch(&enabled).response.expect("runtime enabled");
        }

        let navigate = CdpRequest {
            id: 2,
            session_id: Some("sess-2".to_owned()),
            method: "Page.navigate".to_owned(),
            params: json!({ "url": "about:blank" }),
        };
        let outcome = s.dispatch(&navigate);
        outcome.response.expect("navigation accepted");
        let context_sessions = outcome
            .notifications
            .iter()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|event| event["method"] == "Runtime.executionContextCreated")
            .map(|event| event["sessionId"].as_str().unwrap().to_owned())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            context_sessions,
            std::collections::BTreeSet::from(["sess-1".to_owned(), "sess-2".to_owned()])
        );
    }

    #[test]
    fn runtime_effects_notify_every_session_attached_to_the_target() {
        let mut state = CdpState::default();
        let attached = dispatch_one(
            &mut state,
            "Target.attachToTarget",
            json!({ "targetId": "tab-1", "flatten": true }),
        );
        assert_eq!(attached["sessionId"], "sess-2");
        for session_id in ["sess-1", "sess-2"] {
            let enabled = CdpRequest {
                id: 1,
                session_id: Some(session_id.to_owned()),
                method: "Runtime.enable".to_owned(),
                params: json!({}),
            };
            state.dispatch(&enabled).response.expect("runtime enabled");
        }
        dispatch_one(
            &mut state,
            "Runtime.addBinding",
            json!({ "name": "fanoutBinding" }),
        );
        let request = CdpRequest {
            id: 2,
            session_id: Some("sess-2".to_owned()),
            method: "Runtime.evaluate".to_owned(),
            params: json!({
                "expression": "console.log('fanout-effect'); fanoutBinding('fanout-binding'); 'done'"
            }),
        };

        let outcome = state.dispatch(&request);
        assert_eq!(
            outcome.response.expect("runtime response")["result"]["value"],
            "done"
        );
        for method in ["Runtime.consoleAPICalled", "Runtime.bindingCalled"] {
            let sessions = outcome
                .notifications
                .iter()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .filter(|event| event["method"] == method)
                .map(|event| event["sessionId"].as_str().unwrap().to_owned())
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(
                sessions,
                std::collections::BTreeSet::from(["sess-1".to_owned(), "sess-2".to_owned()])
            );
        }
    }

    #[test]
    fn network_enable_emits_top_level_navigation_events() {
        let dir = tempfile::tempdir().unwrap();
        let html = dir.path().join("network.html");
        std::fs::write(&html, "<title>Network</title>").unwrap();
        let url = format!("file://{}", html.display());

        let mut s = CdpState::default();
        dispatch_one(&mut s, "Network.enable", json!({}));
        let navigate = CdpRequest {
            id: 1,
            session_id: Some("sess-1".to_owned()),
            method: "Page.navigate".to_owned(),
            params: json!({ "url": url }),
        };
        let outcome = s.dispatch(&navigate);
        let response = outcome.response.expect("navigate response");
        let loader_id = response["loaderId"].as_str().unwrap();
        let notifications = outcome
            .notifications
            .iter()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();

        let request = notifications
            .iter()
            .find(|event| event["method"] == "Network.requestWillBeSent")
            .expect("request event");
        assert_eq!(request["sessionId"], "sess-1");
        assert_eq!(request["params"]["loaderId"], loader_id);
        assert_eq!(request["params"]["request"]["method"], "GET");
        assert!(
            request["params"]["request"]["url"]
                .as_str()
                .unwrap()
                .ends_with("network.html")
        );
        let request_id = request["params"]["requestId"].clone();

        let response = notifications
            .iter()
            .find(|event| event["method"] == "Network.responseReceived")
            .expect("response event");
        assert_eq!(response["params"]["requestId"], request_id);
        assert_eq!(response["params"]["type"], "Document");
        assert_eq!(response["params"]["response"]["status"], 200);
        assert_eq!(response["params"]["response"]["mimeType"], "text/html");

        let finished = notifications
            .iter()
            .find(|event| event["method"] == "Network.loadingFinished")
            .expect("loadingFinished event");
        assert_eq!(finished["params"]["requestId"], request_id);

        dispatch_one(&mut s, "Network.disable", json!({}));
        let second = s.dispatch(&CdpRequest {
            id: 2,
            session_id: None,
            method: "Page.navigate".to_owned(),
            params: json!({ "url": "about:blank" }),
        });
        assert!(second.notifications.iter().all(|line| {
            serde_json::from_str::<Value>(line).unwrap()["method"]
                .as_str()
                .is_some_and(|method| !method.starts_with("Network."))
        }));
    }

    #[test]
    fn page_lifecycle_events_follow_enabled_flag() {
        let dir = tempfile::tempdir().unwrap();
        let html = dir.path().join("lifecycle.html");
        std::fs::write(&html, "<title>Lifecycle</title>").unwrap();
        let url = format!("file://{}", html.display());

        let mut s = CdpState::default();
        let navigate = |state: &mut CdpState, id| {
            state.dispatch(&CdpRequest {
                id,
                session_id: None,
                method: "Page.navigate".to_owned(),
                params: json!({ "url": url.clone() }),
            })
        };

        let default_outcome = navigate(&mut s, 1);
        assert!(default_outcome.notifications.iter().all(|line| {
            serde_json::from_str::<Value>(line).unwrap()["method"] != "Page.lifecycleEvent"
        }));

        dispatch_one(
            &mut s,
            "Page.setLifecycleEventsEnabled",
            json!({ "enabled": true }),
        );
        let enabled_outcome = navigate(&mut s, 2);
        let lifecycle_names = enabled_outcome
            .notifications
            .iter()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|event| event["method"] == "Page.lifecycleEvent")
            .map(|event| event["params"]["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            lifecycle_names,
            vec!["init", "commit", "DOMContentLoaded", "load"]
        );

        dispatch_one(
            &mut s,
            "Page.setLifecycleEventsEnabled",
            json!({ "enabled": false }),
        );
        let disabled_outcome = navigate(&mut s, 3);
        assert!(disabled_outcome.notifications.iter().all(|line| {
            serde_json::from_str::<Value>(line).unwrap()["method"] != "Page.lifecycleEvent"
        }));
    }

    #[test]
    fn network_enable_emits_fetch_events_after_runtime_evaluate() {
        let (url, network_config, server) = spawn_fetch_server("vixen-cdp-fetch.com", "cdp body");
        let rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let mut s = CdpState::with_runtime(rt);
        dispatch_one(&mut s, "Network.enable", json!({}));

        let evaluate = CdpRequest {
            id: 7,
            session_id: None,
            method: "Runtime.evaluate".to_owned(),
            params: json!({
                "expression": format!("fetch({url:?}).then((response) => response.text())"),
                "returnByValue": true,
            }),
        };
        let outcome = s.dispatch(&evaluate);
        assert_eq!(
            outcome.response.expect("evaluate response")["result"]["value"],
            "cdp body"
        );
        let notifications = outcome
            .notifications
            .iter()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();

        let request = notifications
            .iter()
            .find(|event| event["method"] == "Network.requestWillBeSent")
            .expect("fetch request event");
        assert_eq!(request["params"]["type"], "Fetch");
        assert_eq!(request["params"]["requestId"], "fetch-1");
        assert_eq!(request["params"]["request"]["url"], url);
        assert_eq!(request["params"]["request"]["method"], "GET");

        let response = notifications
            .iter()
            .find(|event| event["method"] == "Network.responseReceived")
            .expect("fetch response event");
        assert_eq!(response["params"]["requestId"], "fetch-1");
        assert_eq!(response["params"]["type"], "Fetch");
        assert_eq!(response["params"]["response"]["status"], 200);

        let finished = notifications
            .iter()
            .find(|event| event["method"] == "Network.loadingFinished")
            .expect("fetch loadingFinished event");
        assert_eq!(finished["params"]["requestId"], "fetch-1");
        server.join().unwrap();
    }

    #[test]
    fn capture_screenshot_uses_initial_core_target() {
        let mut s = CdpState::default();
        let no_page = CdpRequest {
            id: 1,
            session_id: None,
            method: "Page.captureScreenshot".into(),
            params: json!({}),
        };
        let screenshot = s
            .dispatch(&no_page)
            .response
            .expect("initial target screenshot");
        assert!(!screenshot["data"].as_str().unwrap().is_empty());

        let bad_format = CdpRequest {
            id: 2,
            session_id: None,
            method: "Page.captureScreenshot".into(),
            params: json!({ "format": "jpeg" }),
        };
        let err = s
            .dispatch(&bad_format)
            .response
            .expect_err("unsupported format must fail before GL");
        assert_eq!(err.code, -32602);
    }

    #[test]
    fn capture_viewport_accepts_default_and_full_clip_only() {
        assert_eq!(
            capture_viewport(&json!({}), (500, 320)).unwrap(),
            (500, 320)
        );
        assert_eq!(
            capture_viewport(
                &json!({ "clip": { "x": 0, "y": 0, "width": 160, "height": 120, "scale": 1 } }),
                (500, 320),
            )
            .unwrap(),
            (160, 120)
        );
        assert!(
            capture_viewport(
                &json!({ "clip": { "x": 10, "y": 0, "width": 160, "height": 120, "scale": 1 } }),
                (500, 320),
            )
            .is_err()
        );
    }

    #[test]
    fn input_mouse_event_validates_on_initial_target() {
        let mut s = CdpState::default();
        let req = CdpRequest {
            id: 1,
            session_id: None,
            method: "Input.dispatchMouseEvent".into(),
            params: json!({ "type": "mousePressed", "x": 1, "y": 1, "button": "left" }),
        };
        assert!(s.dispatch(&req).response.is_ok());

        let req = CdpRequest {
            id: 2,
            session_id: None,
            method: "Input.dispatchMouseEvent".into(),
            params: json!({ "type": "mousePressed", "x": "bad", "y": 1 }),
        };
        let err = s
            .dispatch(&req)
            .response
            .expect_err("bad coordinates must fail");
        assert_eq!(err.code, -32602);
    }

    #[test]
    fn input_key_event_validates_on_initial_target() {
        let mut s = CdpState::default();
        let req = CdpRequest {
            id: 1,
            session_id: None,
            method: "Input.dispatchKeyEvent".into(),
            params: json!({ "type": "keyDown", "key": "A", "text": "A" }),
        };
        assert!(s.dispatch(&req).response.is_ok());

        let req = CdpRequest {
            id: 2,
            session_id: None,
            method: "Input.dispatchKeyEvent".into(),
            params: json!({ "type": "unsupported" }),
        };
        let err = s
            .dispatch(&req)
            .response
            .expect_err("bad key event type must fail");
        assert_eq!(err.code, -32602);
    }

    #[test]
    fn input_insert_text_validates_on_initial_target() {
        let mut s = CdpState::default();
        let req = CdpRequest {
            id: 1,
            session_id: None,
            method: "Input.insertText".into(),
            params: json!({ "text": "A" }),
        };
        assert!(s.dispatch(&req).response.is_ok());

        let req = CdpRequest {
            id: 2,
            session_id: None,
            method: "Input.insertText".into(),
            params: json!({}),
        };
        let err = s
            .dispatch(&req)
            .response
            .expect_err("insertText without text must fail");
        assert_eq!(err.code, -32602);
    }

    #[test]
    fn input_mouse_move_dispatches_hover_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let html = dir.path().join("hover.html");
        std::fs::write(
            &html,
            "<style>#hit{display:block;width:120px;height:30px}</style><button id='hit'>Go</button><script>globalThis.__hover=[];const hit=document.querySelector('#hit');for(const type of ['mouseover','mouseenter','mousemove']) hit.addEventListener(type, event => globalThis.__hover.push(type + ':' + (event.relatedTarget ? event.relatedTarget.id : '')));</script>",
        )
        .unwrap();
        let url = format!("file://{}", html.display());

        let mut s = CdpState::default();
        dispatch_one(&mut s, "Page.navigate", json!({ "url": url }));
        dispatch_one(
            &mut s,
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseMoved", "x": 10, "y": 10, "button": "none" }),
        );
        let result = dispatch_one(
            &mut s,
            "Runtime.evaluate",
            json!({ "expression": "globalThis.__hover.join('>')" }),
        );

        assert_eq!(
            result["result"]["value"],
            "mouseover:>mouseenter:>mousemove:"
        );
    }

    #[test]
    fn input_mouse_release_dispatches_double_click() {
        let dir = tempfile::tempdir().unwrap();
        let html = dir.path().join("dblclick.html");
        std::fs::write(
            &html,
            "<style>#hit{display:block;width:120px;height:30px}</style><button id='hit'>Go</button><script>globalThis.__dbl=[];const hit=document.querySelector('#hit');for(const type of ['click','dblclick']) hit.addEventListener(type, event => globalThis.__dbl.push(type + ':' + event.detail));</script>",
        )
        .unwrap();
        let url = format!("file://{}", html.display());

        let mut s = CdpState::default();
        dispatch_one(&mut s, "Page.navigate", json!({ "url": url }));
        for (event_type, buttons, click_count) in [
            ("mousePressed", 1, 1),
            ("mouseReleased", 0, 1),
            ("mousePressed", 1, 2),
            ("mouseReleased", 0, 2),
        ] {
            dispatch_one(
                &mut s,
                "Input.dispatchMouseEvent",
                json!({ "type": event_type, "x": 10, "y": 10, "button": "left", "buttons": buttons, "clickCount": click_count }),
            );
        }
        let result = dispatch_one(
            &mut s,
            "Runtime.evaluate",
            json!({ "expression": "globalThis.__dbl.join('>')" }),
        );

        assert_eq!(result["result"]["value"], "click:1>click:2>dblclick:2");
    }

    #[test]
    fn input_right_mouse_release_dispatches_contextmenu() {
        let dir = tempfile::tempdir().unwrap();
        let html = dir.path().join("contextmenu.html");
        std::fs::write(
            &html,
            "<style>#hit{display:block;width:120px;height:30px}</style><button id='hit'>Go</button><script>globalThis.__ctx=[];const hit=document.querySelector('#hit');for(const type of ['mousedown','mouseup','contextmenu']) hit.addEventListener(type, event => globalThis.__ctx.push(type + ':' + event.button));</script>",
        )
        .unwrap();
        let url = format!("file://{}", html.display());

        let mut s = CdpState::default();
        dispatch_one(&mut s, "Page.navigate", json!({ "url": url }));
        for (event_type, buttons) in [("mousePressed", 2), ("mouseReleased", 0)] {
            dispatch_one(
                &mut s,
                "Input.dispatchMouseEvent",
                json!({ "type": event_type, "x": 10, "y": 10, "button": "right", "buttons": buttons }),
            );
        }
        let result = dispatch_one(
            &mut s,
            "Runtime.evaluate",
            json!({ "expression": "globalThis.__ctx.join('>')" }),
        );

        assert_eq!(
            result["result"]["value"],
            "mousedown:2>mouseup:2>contextmenu:2"
        );
    }

    #[test]
    fn input_mouse_wheel_dispatches_wheel_event() {
        let dir = tempfile::tempdir().unwrap();
        let html = dir.path().join("wheel.html");
        std::fs::write(
            &html,
            "<style>#hit{display:block;width:120px;height:30px}</style><button id='hit'>Go</button><script>globalThis.__wheel=[];const hit=document.querySelector('#hit');hit.addEventListener('wheel', event => globalThis.__wheel.push(event.deltaX + ':' + event.deltaY + ':' + event.deltaMode + ':' + event.clientX + ':' + event.clientY));</script>",
        )
        .unwrap();
        let url = format!("file://{}", html.display());

        let mut s = CdpState::default();
        dispatch_one(&mut s, "Page.navigate", json!({ "url": url }));
        dispatch_one(
            &mut s,
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseWheel", "x": 10, "y": 10, "button": "none", "deltaX": 4, "deltaY": 25 }),
        );
        let result = dispatch_one(
            &mut s,
            "Runtime.evaluate",
            json!({ "expression": "globalThis.__wheel.join('>')" }),
        );

        assert_eq!(result["result"]["value"], "4:25:0:10:10");
    }

    #[test]
    fn runtime_document_write_commits_page_content() {
        let mut s = CdpState::default();
        dispatch_one(&mut s, "Page.navigate", json!({ "url": "about:blank" }));
        dispatch_one(
            &mut s,
            "Runtime.evaluate",
            json!({ "expression": "document.open(); document.write('<title>Written</title><main id=written>Hello write</main>'); document.close();" }),
        );
        let result = dispatch_one(
            &mut s,
            "Runtime.evaluate",
            json!({ "expression": "document.title + ':' + document.querySelector('#written').textContent" }),
        );

        assert_eq!(result["result"]["value"], "Written:Hello write");
    }

    #[test]
    fn page_init_scripts_run_before_author_scripts_on_navigation() {
        let dir = tempfile::tempdir().unwrap();
        let html = dir.path().join("init-script.html");
        std::fs::write(
            &html,
            "<script>globalThis.__authorSawInit = globalThis.__initOrder; globalThis.__initOrder = globalThis.__initOrder + ':author';</script>",
        )
        .unwrap();
        let url = format!("file://{}", html.display());

        let mut s = CdpState::default();
        dispatch_one(
            &mut s,
            "Page.addScriptToEvaluateOnNewDocument",
            json!({ "source": "globalThis.__initOrder = 'init';" }),
        );
        let removed = dispatch_one(
            &mut s,
            "Page.addScriptToEvaluateOnNewDocument",
            json!({ "source": "globalThis.__removedInit = true;" }),
        );
        dispatch_one(
            &mut s,
            "Page.removeScriptToEvaluateOnNewDocument",
            json!({ "identifier": removed["identifier"].as_str().unwrap() }),
        );
        dispatch_one(&mut s, "Page.navigate", json!({ "url": url }));
        let result = dispatch_one(
            &mut s,
            "Runtime.evaluate",
            json!({ "expression": "globalThis.__initOrder + ':' + globalThis.__authorSawInit + ':' + String(globalThis.__removedInit)" }),
        );

        assert_eq!(result["result"]["value"], "init:author:init:undefined");
    }

    #[test]
    fn runtime_get_properties_returns_stored_object_properties() {
        let mut s = CdpState::default();
        let remote = dispatch_one(
            &mut s,
            "Runtime.evaluate",
            json!({ "expression": "({ answer: 42, nested: { ok: true } })" }),
        );
        let object_id = remote["result"]["objectId"].as_str().unwrap();

        let properties = dispatch_one(
            &mut s,
            "Runtime.getProperties",
            json!({ "objectId": object_id, "ownProperties": true }),
        );
        let props = properties["result"].as_array().unwrap();
        let answer = props
            .iter()
            .find(|prop| prop["name"] == "answer")
            .expect("answer property");
        assert_eq!(answer["value"]["type"], "number");
        assert_eq!(answer["value"]["value"], 42);

        let nested = props
            .iter()
            .find(|prop| prop["name"] == "nested")
            .expect("nested property");
        let nested_id = nested["value"]["objectId"].as_str().unwrap();
        let nested_ok = dispatch_one(
            &mut s,
            "Runtime.callFunctionOn",
            json!({
                "objectId": nested_id,
                "functionDeclaration": "(function() { return this.ok; })",
                "returnByValue": true,
            }),
        );
        assert_eq!(nested_ok["result"]["value"], true);
    }

    #[test]
    fn runtime_remote_handles_are_released_by_object_and_group() {
        let mut s = CdpState::default();
        let grouped = dispatch_one(
            &mut s,
            "Runtime.evaluate",
            json!({
                "expression": "({ nested: { ok: true } })",
                "objectGroup": "test-group",
            }),
        );
        let grouped_id = grouped["result"]["objectId"].as_str().unwrap();
        let properties = dispatch_one(
            &mut s,
            "Runtime.getProperties",
            json!({ "objectId": grouped_id, "ownProperties": true }),
        );
        let nested_id = properties["result"]
            .as_array()
            .unwrap()
            .iter()
            .find(|property| property["name"] == "nested")
            .unwrap()["value"]["objectId"]
            .as_str()
            .unwrap()
            .to_owned();

        dispatch_one(
            &mut s,
            "Runtime.releaseObjectGroup",
            json!({ "objectGroup": "test-group" }),
        );
        for object_id in [grouped_id, nested_id.as_str()] {
            let error = dispatch_error(
                &mut s,
                "Runtime.getProperties",
                json!({ "objectId": object_id }),
            );
            assert_eq!(error.code, -32000);
            assert!(error.message.contains("unknown or released objectId"));
        }

        let single = dispatch_one(
            &mut s,
            "Runtime.evaluate",
            json!({ "expression": "({ value: 1 })" }),
        );
        let single_id = single["result"]["objectId"].as_str().unwrap();
        dispatch_one(
            &mut s,
            "Runtime.releaseObject",
            json!({ "objectId": single_id }),
        );
        let error = dispatch_error(
            &mut s,
            "Runtime.getProperties",
            json!({ "objectId": single_id }),
        );
        assert_eq!(error.code, -32000);
        assert!(error.message.contains("unknown or released objectId"));
    }

    #[test]
    fn runtime_remote_handles_are_generation_scoped_and_bounded() {
        let mut s = CdpState::default();
        let remote = dispatch_one(
            &mut s,
            "Runtime.evaluate",
            json!({ "expression": "({ retained: true })" }),
        );
        let object_id = remote["result"]["objectId"].as_str().unwrap().to_owned();
        let prefix = s.remote_object_prefix();
        for serial in 0..MAX_REMOTE_HANDLES {
            s.register_remote_object_id(format!("{prefix}test-{serial}"), None);
        }
        assert_eq!(s.remote_handles.len(), MAX_REMOTE_HANDLES);
        assert!(
            !s.remote_handles
                .iter()
                .any(|handle| handle.object_id == object_id)
        );
        let stored = dispatch_one(
            &mut s,
            "Runtime.evaluate",
            json!({
                "expression": format!(
                    "Object.prototype.hasOwnProperty.call(globalThis.__vixenCdpObjects, {})",
                    serde_json::to_string(&object_id).unwrap()
                ),
                "returnByValue": true,
            }),
        );
        assert_eq!(stored["result"]["value"], false);

        let fresh = dispatch_one(
            &mut s,
            "Runtime.evaluate",
            json!({ "expression": "({ staleAfterNavigation: true })" }),
        );
        let stale_id = fresh["result"]["objectId"].as_str().unwrap().to_owned();
        dispatch_one(&mut s, "Page.navigate", json!({ "url": "about:blank" }));
        let error = dispatch_error(
            &mut s,
            "Runtime.getProperties",
            json!({ "objectId": stale_id }),
        );
        assert_eq!(error.code, -32000);
        assert!(
            error
                .message
                .contains("stale or different runtime generation")
        );
    }

    #[test]
    fn runtime_await_promise_resolves_stored_promise_handles() {
        let mut s = CdpState::default();
        let remote = dispatch_one(
            &mut s,
            "Runtime.evaluate",
            json!({ "expression": "Promise.resolve({ answer: 42, nested: { ok: true } })" }),
        );
        let promise_id = remote["result"]["objectId"].as_str().unwrap();

        let by_value = dispatch_one(
            &mut s,
            "Runtime.awaitPromise",
            json!({ "promiseObjectId": promise_id, "returnByValue": true }),
        );
        assert_eq!(by_value["result"]["type"], "object");
        assert_eq!(by_value["result"]["value"]["answer"], 42);

        let by_handle = dispatch_one(
            &mut s,
            "Runtime.awaitPromise",
            json!({ "promiseObjectId": promise_id }),
        );
        let object_id = by_handle["result"]["objectId"].as_str().unwrap();
        let properties = dispatch_one(
            &mut s,
            "Runtime.getProperties",
            json!({ "objectId": object_id, "ownProperties": true }),
        );
        let props = properties["result"].as_array().unwrap();
        let answer = props
            .iter()
            .find(|prop| prop["name"] == "answer")
            .expect("answer property");
        assert_eq!(answer["value"]["value"], 42);
    }

    #[test]
    fn runtime_add_binding_emits_binding_called_notifications() {
        let mut s = CdpState::default();
        dispatch_one(&mut s, "Runtime.enable", json!({}));
        dispatch_one(
            &mut s,
            "Runtime.addBinding",
            json!({ "name": "hostBinding" }),
        );
        let runtime_context_id = s.current_state().unwrap().runtime_context_id.unwrap().get();

        let req = CdpRequest {
            id: 9,
            session_id: Some("sess-1".to_owned()),
            method: "Runtime.evaluate".to_owned(),
            params: json!({ "expression": "hostBinding('payload'); 'done'" }),
        };
        let outcome = s.dispatch(&req);
        let response = outcome.response.expect("success response");
        assert_eq!(response["result"]["value"], "done");

        let binding = outcome
            .notifications
            .iter()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|event| event["method"] == "Runtime.bindingCalled")
            .expect("binding notification");
        assert_eq!(binding["sessionId"], "sess-1");
        assert_eq!(binding["params"]["name"], "hostBinding");
        assert_eq!(binding["params"]["payload"], "payload");
        assert_eq!(binding["params"]["executionContextId"], runtime_context_id);
    }

    #[test]
    fn runtime_add_binding_is_available_before_new_document_scripts() {
        let mut s = CdpState::default();
        dispatch_one(
            &mut s,
            "Runtime.addBinding",
            json!({ "name": "__playwright__binding__" }),
        );
        dispatch_one(
            &mut s,
            "Page.addScriptToEvaluateOnNewDocument",
            json!({ "source": "globalThis.__bindingTypeAtInit = typeof globalThis.__playwright__binding__;" }),
        );
        dispatch_one(&mut s, "Page.navigate", json!({ "url": "about:blank" }));

        let result = dispatch_one(
            &mut s,
            "Runtime.evaluate",
            json!({ "expression": "globalThis.__bindingTypeAtInit" }),
        );
        assert_eq!(result["result"]["value"], "function");
    }

    #[test]
    fn runtime_alert_emits_javascript_dialog_notifications() {
        let mut s = CdpState::default();
        dispatch_one(&mut s, "Page.navigate", json!({ "url": "about:blank" }));
        dispatch_one(&mut s, "Page.enable", json!({}));

        let req = CdpRequest {
            id: 7,
            session_id: Some("sess-1".to_owned()),
            method: "Runtime.evaluate".to_owned(),
            params: json!({ "expression": "alert('hello dialog')" }),
        };
        let outcome = s.dispatch(&req);
        assert!(outcome.response.is_ok());
        let opening = outcome
            .notifications
            .iter()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|event| event["method"] == "Page.javascriptDialogOpening")
            .expect("dialog opening notification");
        assert_eq!(opening["sessionId"], "sess-1");
        assert_eq!(opening["params"]["type"], "alert");
        assert_eq!(opening["params"]["message"], "hello dialog");

        let close_req = CdpRequest {
            id: 8,
            session_id: Some("sess-1".to_owned()),
            method: "Page.handleJavaScriptDialog".to_owned(),
            params: json!({ "accept": true }),
        };
        let closed = s.dispatch(&close_req);
        assert!(closed.response.is_ok());
        assert!(closed.notifications.iter().any(|line| {
            serde_json::from_str::<Value>(line).unwrap()["method"] == "Page.javascriptDialogClosed"
        }));
    }

    #[test]
    fn dom_query_methods_project_page_selectors() {
        let dir = tempfile::tempdir().unwrap();
        let html = dir.path().join("query.html");
        std::fs::write(
            &html,
            "<main id='root'><button id='hit'>Go</button><p class='note'>One</p><p class='note'>Two</p></main>",
        )
        .unwrap();
        let url = format!("file://{}", html.display());

        let mut s = CdpState::default();
        dispatch_one(&mut s, "Page.navigate", json!({ "url": url }));

        let document = dispatch_one(&mut s, "DOM.getDocument", json!({ "depth": 1 }));
        assert_eq!(document["root"]["nodeId"], CDP_DOCUMENT_NODE_ID);
        assert_eq!(document["root"]["nodeType"], 9);
        assert_eq!(document["root"]["children"][0]["localName"], "html");

        let hit = dispatch_one(
            &mut s,
            "DOM.querySelector",
            json!({ "nodeId": CDP_DOCUMENT_NODE_ID, "selector": "#hit" }),
        );
        let hit_node_id = hit["nodeId"].as_u64().expect("button node id");
        assert!(hit_node_id > 0);

        let missing = dispatch_one(
            &mut s,
            "DOM.querySelector",
            json!({ "nodeId": CDP_DOCUMENT_NODE_ID, "selector": "#missing" }),
        );
        assert_eq!(missing["nodeId"], 0);

        let notes = dispatch_one(
            &mut s,
            "DOM.querySelectorAll",
            json!({ "nodeId": CDP_DOCUMENT_NODE_ID, "selector": ".note" }),
        );
        let note_ids = notes["nodeIds"].as_array().expect("node id list");
        assert_eq!(note_ids.len(), 2);

        let described = dispatch_one(&mut s, "DOM.describeNode", json!({ "nodeId": hit_node_id }));
        assert_eq!(described["node"]["localName"], "button");
    }

    #[test]
    fn dom_attribute_and_outer_html_methods_mutate_page_dom() {
        let dir = tempfile::tempdir().unwrap();
        let html = dir.path().join("attrs.html");
        std::fs::write(
            &html,
            "<main id='root'><button id='hit' data-state='old'>Go</button></main>",
        )
        .unwrap();
        let url = format!("file://{}", html.display());

        let mut s = CdpState::default();
        dispatch_one(&mut s, "Page.navigate", json!({ "url": url }));
        let node_id = dispatch_one(
            &mut s,
            "DOM.querySelector",
            json!({ "nodeId": CDP_DOCUMENT_NODE_ID, "selector": "#hit" }),
        )["nodeId"]
            .as_u64()
            .unwrap();

        let attrs = dispatch_one(&mut s, "DOM.getAttributes", json!({ "nodeId": node_id }));
        assert_eq!(
            attrs["attributes"].as_array().unwrap(),
            &vec![json!("id"), json!("hit"), json!("data-state"), json!("old")]
        );

        dispatch_one(
            &mut s,
            "DOM.setAttributeValue",
            json!({ "nodeId": node_id, "name": "data-state", "value": "new" }),
        );
        let outer = dispatch_one(&mut s, "DOM.getOuterHTML", json!({ "nodeId": node_id }));
        assert_eq!(
            outer["outerHTML"].as_str().unwrap(),
            "<button id=\"hit\" data-state=\"new\">Go</button>"
        );

        dispatch_one(
            &mut s,
            "DOM.removeAttribute",
            json!({ "nodeId": node_id, "name": "data-state" }),
        );
        let value = dispatch_one(
            &mut s,
            "Runtime.evaluate",
            json!({ "expression": "document.querySelector('#hit').getAttribute('data-state')" }),
        );
        assert_eq!(value["result"]["subtype"], "null");
    }

    #[test]
    fn page_performance_and_security_methods_are_browser_shaped() {
        let dir = tempfile::tempdir().unwrap();
        let html = dir.path().join("resource.html");
        std::fs::write(&html, "<title>Resource</title><main>Body</main>").unwrap();
        let url = format!("file://{}", html.display());

        let mut s = CdpState::default();
        dispatch_one(&mut s, "Page.navigate", json!({ "url": url.clone() }));

        let resources = dispatch_one(&mut s, "Page.getResourceTree", json!({}));
        assert_eq!(resources["frameTree"]["frame"]["url"], url);
        assert_eq!(
            resources["frameTree"]["resources"]
                .as_array()
                .unwrap()
                .len(),
            0
        );

        let content = dispatch_one(&mut s, "Page.getResourceContent", json!({ "url": url }));
        assert_eq!(content["base64Encoded"], false);
        assert!(content["content"].as_str().unwrap().contains("Resource"));

        let metrics = dispatch_one(&mut s, "Performance.getMetrics", json!({}));
        assert!(
            metrics["metrics"].as_array().unwrap().iter().any(|entry| {
                entry["name"] == "Nodes" && entry["value"].as_f64().unwrap() >= 1.0
            })
        );

        let security = dispatch_one(&mut s, "Security.getSecurityState", json!({}));
        assert_eq!(security["securityState"], "neutral");
        assert_eq!(security["schemeIsCryptographic"], false);

        dispatch_one(&mut s, "Page.stopLoading", json!({}));
        dispatch_one(&mut s, "Page.resetNavigationHistory", json!({}));
        let history = dispatch_one(&mut s, "Page.getNavigationHistory", json!({}));
        assert_eq!(history["entries"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn page_set_bypass_csp_controls_page_script_execution() {
        let dir = tempfile::tempdir().unwrap();
        let html = dir.path().join("csp.html");
        std::fs::write(
            &html,
            r#"<meta http-equiv="Content-Security-Policy" content="script-src 'none'">
               <body><script>document.body.setAttribute('data-ran', 'yes');</script></body>"#,
        )
        .unwrap();
        let url = format!("file://{}", html.display());

        let mut s = CdpState::default();
        dispatch_one(&mut s, "Page.navigate", json!({ "url": url.clone() }));
        let blocked = dispatch_one(
            &mut s,
            "Runtime.evaluate",
            json!({ "expression": "document.body.getAttribute('data-ran')" }),
        );
        assert_eq!(blocked["result"]["subtype"], "null");

        dispatch_one(&mut s, "Page.setBypassCSP", json!({ "enabled": true }));
        dispatch_one(&mut s, "Page.navigate", json!({ "url": url }));
        let bypassed = dispatch_one(
            &mut s,
            "Runtime.evaluate",
            json!({ "expression": "document.body.getAttribute('data-ran')" }),
        );
        assert_eq!(bypassed["result"]["value"], "yes");
    }

    #[test]
    fn dom_geometry_methods_project_layout_boxes() {
        let dir = tempfile::tempdir().unwrap();
        let html = dir.path().join("geometry.html");
        std::fs::write(
            &html,
            "<style>#hit{display:block;width:120px;height:30px}</style><button id='hit'>Go</button><input id='empty'>",
        )
        .unwrap();
        let url = format!("file://{}", html.display());

        let mut s = CdpState::default();
        dispatch_one(&mut s, "Page.navigate", json!({ "url": url }));
        let document = dispatch_one(&mut s, "DOM.getDocument", json!({}));
        let node_id = dispatch_one(
            &mut s,
            "DOM.querySelector",
            json!({ "nodeId": document["root"]["nodeId"], "selector": "#hit" }),
        )["nodeId"]
            .as_u64()
            .unwrap();

        let quads = dispatch_one(&mut s, "DOM.getContentQuads", json!({ "nodeId": node_id }));
        let quad = quads["quads"][0].as_array().unwrap();
        assert_eq!(quad.len(), 8);
        assert!(quad[2].as_f64().unwrap() > quad[0].as_f64().unwrap());
        assert!(quad[5].as_f64().unwrap() > quad[1].as_f64().unwrap());

        let model = dispatch_one(&mut s, "DOM.getBoxModel", json!({ "nodeId": node_id }));
        assert!(model["model"]["width"].as_f64().unwrap() > 0.0);
        assert!(model["model"]["height"].as_f64().unwrap() > 0.0);

        let remote = dispatch_one(
            &mut s,
            "Runtime.evaluate",
            json!({ "expression": "document.querySelector('#hit')" }),
        );
        let object_id = remote["result"]["objectId"].as_str().unwrap();
        let described = dispatch_one(&mut s, "DOM.describeNode", json!({ "objectId": object_id }));
        assert_eq!(described["node"]["nodeId"], node_id);
        assert_eq!(described["node"]["backendNodeId"], node_id);
        assert_eq!(described["node"]["localName"], "button");
        let resolved = dispatch_one(
            &mut s,
            "DOM.resolveNode",
            json!({ "backendNodeId": node_id }),
        );
        assert_eq!(resolved["object"]["subtype"], "node");
        let resolved_object_id = resolved["object"]["objectId"].as_str().unwrap();
        let object_quads = dispatch_one(
            &mut s,
            "DOM.getContentQuads",
            json!({ "objectId": object_id }),
        );
        assert_eq!(object_quads["quads"][0], quads["quads"][0]);
        let resolved_quads = dispatch_one(
            &mut s,
            "DOM.getContentQuads",
            json!({ "objectId": resolved_object_id }),
        );
        assert_eq!(resolved_quads["quads"][0], quads["quads"][0]);

        let input_node_id = dispatch_one(
            &mut s,
            "DOM.querySelector",
            json!({ "nodeId": document["root"]["nodeId"], "selector": "#empty" }),
        )["nodeId"]
            .as_u64()
            .unwrap();
        let input_model = dispatch_one(
            &mut s,
            "DOM.getBoxModel",
            json!({ "nodeId": input_node_id }),
        );
        assert!(input_model["model"]["width"].as_f64().unwrap() > 0.0);
        assert!(input_model["model"]["height"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn wire_path_returns_response_before_notifications() {
        let mut s = CdpState::default();
        let out = s.handle_text_sync(
            &json!({
                "id": 7,
                "method": "Page.navigate",
                "params": { "url": "about:blank" }
            })
            .to_string(),
        );

        assert!(out.len() >= 2);
        let response: Value = serde_json::from_str(&out[0]).unwrap();
        let notification: Value = serde_json::from_str(&out[1]).unwrap();
        assert_eq!(response["id"], 7);
        assert_eq!(response["result"]["frameId"], "tab-1");
        assert_eq!(notification["method"], "Page.frameStartedLoading");
    }

    #[test]
    fn page_enable_is_idempotent_after_initial_state_sync() {
        let mut s = CdpState::default();
        s.seed_initial_target("about:blank".to_owned()).unwrap();

        let req = CdpRequest {
            id: 1,
            session_id: Some("sess-1".to_owned()),
            method: "Page.enable".to_owned(),
            params: json!({}),
        };
        let first = s.dispatch(&req);
        assert!(first.response.is_ok());
        assert!(first.notifications.iter().any(|line| {
            serde_json::from_str::<Value>(line).unwrap()["method"] == "Page.frameNavigated"
        }));

        let second = s.dispatch(&req);
        assert!(second.response.is_ok());
        assert!(second.notifications.is_empty());
    }

    #[test]
    fn full_document_navigation_resets_runtime_contexts() {
        let mut s = CdpState::default();
        s.seed_initial_target("about:blank".to_owned()).unwrap();

        let runtime_enable = CdpRequest {
            id: 1,
            session_id: Some("sess-1".to_owned()),
            method: "Runtime.enable".to_owned(),
            params: json!({}),
        };
        assert!(s.dispatch(&runtime_enable).response.is_ok());

        let isolated_world = CdpRequest {
            id: 2,
            session_id: Some("sess-1".to_owned()),
            method: "Page.createIsolatedWorld".to_owned(),
            params: json!({ "frameId": "tab-1", "worldName": "playwright-utility" }),
        };
        let isolated = s.dispatch(&isolated_world);
        assert!(isolated.response.is_ok());
        assert!(isolated.notifications.iter().any(|line| {
            let notification = serde_json::from_str::<Value>(line).unwrap();
            notification["method"] == "Runtime.executionContextCreated"
                && notification["params"]["context"]["auxData"]["type"] == "isolated"
        }));

        let navigate = CdpRequest {
            id: 3,
            session_id: Some("sess-1".to_owned()),
            method: "Page.navigate".to_owned(),
            params: json!({ "url": "about:blank" }),
        };
        let outcome = s.dispatch(&navigate);
        assert!(outcome.response.is_ok());

        let notifications = outcome
            .notifications
            .iter()
            .map(|line| serde_json::from_str::<Value>(line).expect("notification JSON"))
            .collect::<Vec<_>>();
        assert!(
            notifications
                .iter()
                .any(|notif| notif["method"] == "Runtime.executionContextsCleared")
        );
        assert!(notifications.iter().any(|notif| {
            notif["method"] == "Runtime.executionContextCreated"
                && notif["params"]["context"]["auxData"]["isDefault"] == true
        }));
        assert!(notifications.iter().any(|notif| {
            notif["method"] == "Runtime.executionContextCreated"
                && notif["params"]["context"]["auxData"]["type"] == "isolated"
        }));
    }

    #[test]
    fn set_content_console_marker_keeps_its_source_runtime_generation() {
        let mut s = CdpState::default();
        let session_id = Some("sess-1".to_owned());
        for method in ["Runtime.enable", "Page.setLifecycleEventsEnabled"] {
            let request = CdpRequest {
                id: 1,
                session_id: session_id.clone(),
                method: method.to_owned(),
                params: if method == "Page.setLifecycleEventsEnabled" {
                    json!({ "enabled": true })
                } else {
                    json!({})
                },
            };
            assert!(s.dispatch(&request).response.is_ok());
        }
        let old_runtime_id = s.current_state().unwrap().runtime_context_id.unwrap().get();
        let request = CdpRequest {
            id: 2,
            session_id,
            method: "Runtime.callFunctionOn".to_owned(),
            params: json!({
                "functionDeclaration": "(html, tag) => { document.open(); console.debug(tag); document.write(html); document.close(); }",
                "arguments": [
                    { "value": "<!doctype html><title>replacement</title><main id='replaced'>ready</main>" },
                    { "value": "playwright:set-content-marker" }
                ],
                "returnByValue": true
            }),
        };
        let outcome = s.dispatch(&request);
        assert!(outcome.response.is_ok());
        let notifications = outcome
            .notifications
            .iter()
            .map(|line| serde_json::from_str::<Value>(line).expect("notification JSON"))
            .collect::<Vec<_>>();
        let console_index = notifications
            .iter()
            .position(|event| event["method"] == "Runtime.consoleAPICalled")
            .expect("setContent marker");
        let cleared_index = notifications
            .iter()
            .position(|event| event["method"] == "Runtime.executionContextsCleared")
            .expect("runtime reset");
        let load_index = notifications
            .iter()
            .position(|event| event["method"] == "Page.loadEventFired")
            .expect("replacement load event");
        assert_eq!(
            notifications[console_index]["params"]["executionContextId"],
            old_runtime_id
        );
        assert!(console_index < cleared_index);
        assert!(cleared_index < load_index);
        assert_ne!(
            s.current_state().unwrap().runtime_context_id.unwrap().get(),
            old_runtime_id
        );
        assert_eq!(
            dispatch_one(
                &mut s,
                "Runtime.evaluate",
                json!({ "expression": "document.title" }),
            )["result"]["value"],
            "replacement"
        );
    }

    #[test]
    fn runtime_evaluate_can_read_navigated_page_dom_facade_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let html = dir.path().join("cdp-title.html");
        std::fs::write(&html, "<title>CDP title</title><p>Body</p>").unwrap();
        let url = format!("file://{}", html.display());

        let mut s = CdpState::default();
        let navigate = s.handle_text_sync(
            &json!({
                "id": 1,
                "method": "Page.navigate",
                "params": { "url": url }
            })
            .to_string(),
        );
        let response: Value = serde_json::from_str(&navigate[0]).unwrap();
        assert_eq!(response["result"]["frameId"], "tab-1");

        let eval = s.handle_text_sync(
            &json!({
                "id": 2,
                "method": "Runtime.evaluate",
                "params": { "expression": "document.readyState" }
            })
            .to_string(),
        );
        let response: Value = serde_json::from_str(&eval[0]).unwrap();
        assert_eq!(response["result"]["result"]["type"], "string");
        assert_eq!(response["result"]["result"]["value"], "complete");
    }

    #[test]
    fn browser_permission_overrides_reach_runtime_and_reset() {
        let mut state = CdpState::default();
        dispatch_one(&mut state, "Page.navigate", json!({ "url": "about:blank" }));
        dispatch_one(
            &mut state,
            "Browser.grantPermissions",
            json!({ "permissions": ["notifications"] }),
        );

        let granted = dispatch_one(
            &mut state,
            "Runtime.evaluate",
            json!({
                "expression": "Promise.all(['notifications','geolocation'].map((name) => navigator.permissions.query({ name }).then((status) => status.state))).then((states) => states.join(':'))",
                "returnByValue": true,
            }),
        );
        assert_eq!(granted["result"]["value"], "granted:denied");

        dispatch_one(&mut state, "Browser.resetPermissions", json!({}));
        let reset = dispatch_one(
            &mut state,
            "Runtime.evaluate",
            json!({
                "expression": "navigator.permissions.query({ name: 'notifications' }).then((status) => status.state)",
                "returnByValue": true,
            }),
        );
        assert_eq!(reset["result"]["value"], "prompt");
    }

    #[test]
    fn tracing_stream_records_bounded_protocol_events() {
        let mut state = CdpState::default();
        let start = state.handle_text_sync(
            &json!({
                "id": 1,
                "method": "Tracing.start",
                "params": { "transferMode": "ReturnAsStream", "categories": "devtools.timeline" }
            })
            .to_string(),
        );
        assert_eq!(
            serde_json::from_str::<Value>(&start[0]).unwrap()["result"],
            json!({})
        );
        state.handle_text_sync(
            &json!({ "id": 2, "method": "Browser.getVersion", "params": {} }).to_string(),
        );
        state.handle_text_sync(
            &json!({ "id": 3, "method": "Page.stopLoading", "params": {} }).to_string(),
        );
        let end = state.handle_text_sync(
            &json!({ "id": 4, "method": "Tracing.end", "params": {} }).to_string(),
        );
        let complete = end
            .iter()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|message| message["method"] == "Tracing.tracingComplete")
            .expect("tracingComplete notification");
        let handle = complete["params"]["stream"].as_str().unwrap();

        let read = state.handle_text_sync(
            &json!({ "id": 5, "method": "IO.read", "params": { "handle": handle } }).to_string(),
        );
        let read: Value = serde_json::from_str(&read[0]).unwrap();
        assert_eq!(read["result"]["base64Encoded"], true);
        assert_eq!(read["result"]["eof"], true);
        let bytes = BASE64_STANDARD
            .decode(read["result"]["data"].as_str().unwrap())
            .unwrap();
        let trace: Value = serde_json::from_slice(&bytes).unwrap();
        let names = trace["traceEvents"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|event| event["name"].as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"Tracing.start"));
        assert!(names.contains(&"Browser.getVersion"));
        assert!(names.contains(&"Page.stopLoading"));
        assert!(!names.contains(&"Tracing.end"));

        assert_eq!(
            dispatch_one(&mut state, "IO.close", json!({ "handle": handle })),
            json!({})
        );
    }

    #[test]
    fn protocol_errors_have_stable_machine_codes() {
        let mut state = CdpState::default();
        let parse: Value = serde_json::from_str(&state.handle_text_sync("{")[0]).unwrap();
        assert_eq!(parse["error"]["code"], -32700);
        assert_eq!(parse["error"]["data"]["vixenCode"], "cdp.parse-error");
        assert!(
            parse["error"]["message"]
                .as_str()
                .unwrap()
                .starts_with("cdp.parse-error:")
        );

        let invalid: Value =
            serde_json::from_str(&state.handle_text_sync(r#"{"id":1}"#)[0]).unwrap();
        assert_eq!(invalid["error"]["code"], -32600);
        assert_eq!(invalid["error"]["data"]["vixenCode"], "cdp.invalid-request");

        let missing: Value = serde_json::from_str(
            &state.handle_text_sync(r#"{"id":2,"method":"Vixen.missing"}"#)[0],
        )
        .unwrap();
        assert_eq!(missing["error"]["code"], -32601);
        assert_eq!(
            missing["error"]["data"]["vixenCode"],
            "cdp.method-not-found"
        );

        state.seed_initial_target("about:blank".to_owned()).unwrap();
        let invalid_session: Value = serde_json::from_str(
            &state.handle_text_sync(r#"{"id":3,"sessionId":"sess-999","method":"Runtime.enable"}"#)
                [0],
        )
        .unwrap();
        assert_eq!(invalid_session["error"]["code"], -32001);
        assert_eq!(
            invalid_session["error"]["data"]["vixenCode"],
            "cdp.invalid-session"
        );
    }

    #[test]
    fn trace_event_buffer_is_bounded() {
        let mut state = CdpState::default();
        state.tracing.active = true;
        for _ in 0..(MAX_TRACE_EVENTS + 10) {
            state.record_trace_event("DOM.getDocument", None, Instant::now(), true);
        }
        assert_eq!(state.tracing.events.len(), MAX_TRACE_EVENTS);
        assert!(state.tracing.data_loss_occurred);
    }

    #[test]
    fn pending_core_navigation_outcomes_are_bounded() {
        let mut state = CdpState::default();
        let context_id = state.current_context().unwrap();
        for raw in 1..=(MAX_PENDING_CORE_NAVIGATIONS as u64 + 1) {
            state.pending_core_navigations.insert(
                NavigationId::new(raw).unwrap(),
                PendingCoreNavigation {
                    context_id,
                    predecessor_navigation_id: None,
                    kind: CrossDocumentNavigationKind::Regular,
                    committed: true,
                    terminal: Some(Ok(())),
                    wire_claimed: false,
                    abandoned: false,
                },
            );
        }
        state.prune_pending_core_navigations();
        assert_eq!(
            state.pending_core_navigations.len(),
            MAX_PENDING_CORE_NAVIGATIONS
        );
        assert!(
            !state
                .pending_core_navigations
                .contains_key(&NavigationId::new(1).unwrap())
        );
    }

    #[test]
    fn abandoned_socket_navigation_remains_claimed_for_late_terminal_outcome() {
        let mut state = CdpState::default();
        let context_id = state.current_context().unwrap();
        let navigation_id = NavigationId::new(99).unwrap();
        state.pending_core_navigations.insert(
            navigation_id,
            PendingCoreNavigation {
                context_id,
                predecessor_navigation_id: None,
                kind: CrossDocumentNavigationKind::Regular,
                committed: false,
                terminal: None,
                wire_claimed: true,
                abandoned: false,
            },
        );

        state.abandon_navigation_ids(&[navigation_id]);
        let tombstone = state.pending_core_navigations.get(&navigation_id).unwrap();
        assert!(tombstone.wire_claimed);
        assert!(tombstone.abandoned);

        state.record_core_event(BrowserEvent::NavigationCancelled {
            context_id,
            frame_id: vixen_api::FrameId::new(1).unwrap(),
            navigation_id,
            request_id: None,
            reason: vixen_api::NavigationCancellationReason::Stopped,
        });
        assert_eq!(state.take_navigation_outcome(navigation_id), None);
        assert!(state.pending_core_navigations.contains_key(&navigation_id));
    }

    #[test]
    fn pre_navigation_event_pump_preserves_unclaimed_terminal_outcomes() {
        let mut state = CdpState::default();
        let context_id = state.current_context().unwrap();
        let navigation_id = NavigationId::new(100).unwrap();
        state.pending_core_navigations.insert(
            navigation_id,
            PendingCoreNavigation {
                context_id,
                predecessor_navigation_id: None,
                kind: CrossDocumentNavigationKind::Regular,
                committed: true,
                terminal: Some(Ok(())),
                wire_claimed: false,
                abandoned: false,
            },
        );

        state.drain_core_events();

        assert!(state.pending_core_navigations.contains_key(&navigation_id));
        assert_eq!(
            state.take_navigation_outcome(navigation_id),
            Some((true, Ok(())))
        );
    }

    #[test]
    fn socket_capacity_only_rejects_commands_that_need_a_continuation() {
        let mut state = CdpState::default();
        let target_count = state.targets.len();

        for (id, method, params) in [
            (1, "Runtime.evaluate", json!({ "expression": "1 + 1" })),
            (
                2,
                "Input.dispatchMouseEvent",
                json!({ "type": "mouseMoved", "x": 10_000, "y": 10_000 }),
            ),
            (
                3,
                "Runtime.evaluate",
                json!({ "expression": "history.pushState({}, '', '/same')" }),
            ),
            (4, "Target.createTarget", json!({ "url": "about:blank" })),
        ] {
            let request = CdpRequest {
                id,
                session_id: None,
                method: method.to_owned(),
                params,
            };
            let immediate = match state.start_socket_operation_with_capacity(&request, true) {
                Err(dispatch) => dispatch,
                Ok(_) => panic!("immediate dispatch was incorrectly deferred"),
            };
            assert!(
                immediate.response.is_ok(),
                "{method} was rejected at capacity"
            );
        }
        assert_eq!(state.targets.len(), target_count + 1);

        let request = CdpRequest {
            id: 5,
            session_id: None,
            method: "Runtime.evaluate".to_owned(),
            params: json!({ "expression": "location.assign('https://capacity.test/'); 'queued'" }),
        };
        let rejected = match state.start_socket_operation_with_capacity(&request, true) {
            Err(dispatch) => dispatch,
            Ok(_) => panic!("continuation was accepted at capacity"),
        };
        assert!(rejected.response.is_err());
        assert!(
            state
                .pending_core_navigations
                .values()
                .all(|pending| !pending.wire_claimed)
        );
    }

    #[test]
    fn failed_target_creation_removes_all_allocated_presentation_state() {
        let mut state = CdpState::default();
        let targets = state.targets.len();
        let presentations = state.context_presentations.len();
        let request = CdpRequest {
            id: 1,
            session_id: None,
            method: "Target.createTarget".to_owned(),
            params: json!({ "url": "http://[" }),
        };
        let dispatch = state.dispatch(&request);
        assert!(dispatch.response.is_err());
        assert_eq!(state.targets.len(), targets);
        assert_eq!(state.context_presentations.len(), presentations);
        assert!(
            state
                .targets
                .iter()
                .all(|target| state.context_presentations.contains_key(&target.context_id))
        );
        assert!(state.attached_sessions.iter().all(|session| {
            state
                .targets
                .iter()
                .any(|target| target.context_id == session.context_id)
        }));
    }

    #[test]
    fn deferred_navigation_failure_keeps_response_ordered_runtime_notifications() {
        let mut state = CdpState::default();
        dispatch_one(&mut state, "Runtime.enable", json!({}));
        let request = CdpRequest {
            id: 90,
            session_id: None,
            method: "Runtime.evaluate".to_owned(),
            params: json!({
                "expression": "console.log('kept-before-failure'); location.assign('file:///vixen-does-not-exist'); 'queued'"
            }),
        };
        let operation = match state.start_socket_operation(&request) {
            Ok(Some(operation)) => operation,
            Ok(None) => panic!("cross-document operation did not create a continuation"),
            Err(_) => panic!("cross-document operation failed to start"),
        };
        let mut pending = VecDeque::from([PendingCdpOperation {
            request,
            started: Instant::now(),
            deadline: Instant::now() + Duration::from_secs(10),
            operation,
        }]);
        let deadline = Instant::now() + Duration::from_secs(10);
        let lines = loop {
            let mut completed = state.poll_socket_operations(&mut pending);
            if let Some(lines) = completed.pop() {
                break lines;
            }
            assert!(
                Instant::now() < deadline,
                "navigation failure did not complete"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        let response: Value = serde_json::from_str(&lines[0]).unwrap();
        assert!(response.get("error").is_some());
        let console_index = lines
            .iter()
            .position(|line| line.contains("kept-before-failure"))
            .expect("console notification survives failure");
        assert!(
            console_index > 0,
            "response must precede console notification"
        );
    }

    #[test]
    fn runtime_get_properties_proxy_navigation_is_socket_asynchronous() {
        let mut state = CdpState::default();
        let object = dispatch_one(
            &mut state,
            "Runtime.evaluate",
            json!({
                "expression": "new Proxy({}, { ownKeys() { location.assign('https://properties.test/'); return []; } })"
            }),
        );
        let object_id = object["result"]["objectId"].as_str().unwrap();
        let request = CdpRequest {
            id: 2,
            session_id: None,
            method: "Runtime.getProperties".to_owned(),
            params: json!({ "objectId": object_id, "ownProperties": true }),
        };
        assert!(is_async_navigation_method("Runtime.getProperties"));
        let operation = match state.start_socket_operation(&request) {
            Ok(Some(operation)) => operation,
            Ok(None) => panic!("Proxy trap did not create a navigation continuation"),
            Err(dispatch) => panic!(
                "Runtime.getProperties failed to start: {:?}",
                dispatch.response
            ),
        };
        let navigation_ids = cross_document_ids(&operation.navigation_actions);
        assert_eq!(navigation_ids.len(), 2);
        assert!(navigation_ids[0] < navigation_ids[1]);
        state.cancel_socket_operation(&operation);
    }

    #[test]
    fn deferred_completion_uses_captured_context_after_session_detach() {
        let mut state = CdpState::default();
        let created = dispatch_one(
            &mut state,
            "Target.createTarget",
            json!({ "url": "about:blank" }),
        );
        let target_id = created["targetId"].as_str().unwrap();
        let attached = dispatch_one(
            &mut state,
            "Target.attachToTarget",
            json!({ "targetId": target_id, "flatten": true }),
        );
        let detached_session = attached["sessionId"].as_str().unwrap().to_owned();
        let context_id = state.targets.last().unwrap().context_id;
        let expected_frame_id = frame_id(&state.context_state(context_id).unwrap());
        dispatch_one(
            &mut state,
            "Target.detachFromTarget",
            json!({ "sessionId": detached_session }),
        );

        state.load_presentation(context_id);
        let request = CdpRequest {
            id: 8,
            session_id: Some(detached_session),
            method: "Runtime.evaluate".to_owned(),
            params: json!({}),
        };
        let notifications = state
            .navigation_notifications_after_completion(
                context_id,
                &request,
                true,
                CrossDocumentNavigationKind::Regular,
            )
            .unwrap();
        let frame_navigated = notifications
            .iter()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|line| line["method"] == "Page.frameNavigated")
            .expect("captured target navigation notification");
        assert_eq!(frame_navigated["params"]["frame"]["id"], expected_frame_id);

        state.runtime_enabled = true;
        let runtime_context_id = state
            .context_state(context_id)
            .unwrap()
            .runtime_context_id
            .unwrap();
        state.queue_runtime_effects(
            runtime_context_id,
            RuntimeEffects {
                console: vec![RuntimeConsoleEvent {
                    kind: "log".to_owned(),
                    args: vec![RuntimeConsoleArg {
                        type_name: "string".to_owned(),
                        subtype: None,
                        value: Some(RuntimeConsoleValue::String("captured-effect".to_owned())),
                        unserializable_value: None,
                        description: "captured-effect".to_owned(),
                    }],
                }],
                ..RuntimeEffects::default()
            },
        );
        let effects = state.drain_side_effect_notifications_for_context(context_id, &request);
        let consoles = effects
            .iter()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|line| line["method"] == "Runtime.consoleAPICalled")
            .collect::<Vec<_>>();
        assert!(consoles.iter().any(|console| {
            console["sessionId"] == request.session_id.as_deref().unwrap()
                && console["params"]["executionContextId"] == runtime_context_id.get()
        }));
    }
}
