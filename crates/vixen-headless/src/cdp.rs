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
//! Architecture: the [`CdpDispatcher`] owns the per-connection state and is
//! pure with respect to networking — every method is a synchronous
//! `handle(method, params) -> Response`. [`serve`] wraps it in the WebSocket
//! accept loop; tests drive the dispatcher directly to keep the test surface
//! tight (no sockets in unit tests).
//!
//! Phase 8 covers the contract surface; full DOM/inspector backing comes
//! with the cascade (Phase 3 step 3) and host hooks (Phase 6).

#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use vixen_engine::engine_error::{EngineError, codes};
use vixen_engine::history::HistoryEntry;
use vixen_engine::page::Page;
use vixen_engine::script::{
    JsBindingEvent, JsConsoleArg, JsConsoleEvent, JsConsoleValue, JsDialogEvent,
    JsNavigationAction, JsNetworkEvent, JsRuntime, JsValue,
};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const DEFAULT_CAPTURE_VIEWPORT: (u32, u32) = (800, 600);
const CDP_DOCUMENT_NODE_ID: usize = 1_000_000_000;

/// CDP server entry point. Binds `127.0.0.1:{port}` and serves the
/// WebSocket CDP protocol until the process is killed.
///
/// **Single-threaded.** `deno_core::JsRuntime` is `!Send + !Sync`, so the whole
/// server runs on one tokio `LocalSet`. Connections are handled serially:
/// CDP clients (Chrome DevTools, Puppeteer, Playwright) maintain a single
/// WebSocket per browser instance, so this is not a bottleneck in practice.
pub async fn serve(port: u16) -> std::io::Result<()> {
    serve_with_initial_url(port, None).await
}

/// CDP server entry point with an already requested page URL. The URL is loaded
/// through the same headless trust boundary as CLI page actions before clients
/// connect, so early `Runtime.evaluate` DOM probes see the requested page.
pub async fn serve_with_initial_url(port: u16, initial_url: Option<String>) -> std::io::Result<()> {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let listener = TcpListener::bind(addr).await?;
    // The state is `Rc<RefCell<>>` so it can move across `await` points in
    // a single-threaded `LocalSet` without going through `Arc<Mutex>`.
    let mut initial_state = CdpState::default();
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

    while let Some(msg) = read.next().await {
        let msg = msg?;
        if msg.is_text() || msg.is_binary() {
            let text = msg.into_text().unwrap_or_default();
            // Borrow the state synchronously (no await while borrowed).
            let resp = state.borrow_mut().handle_text_sync(&text);
            for line in resp {
                write.send(Message::text(line)).await?;
            }
        } else if msg.is_close() {
            break;
        }
    }
    Ok(())
}

/// All per-process CDP state — the dispatcher and a runtime that backs
/// `Runtime.evaluate`. Lives behind `Rc<RefCell<>>` because the JS runtime is
/// `!Send + !Sync`; the whole server runs on a single `LocalSet`.
#[derive(Default)]
pub struct CdpState {
    next_target_id: AtomicU64,
    targets: Vec<Target>,
    attached_sessions: Vec<TargetSession>,
    js: Option<JsRuntime>,
    runtime_enabled: bool,
    network_enabled: bool,
    page_enabled: bool,
    lifecycle_events_enabled: bool,
    isolated_world_name: Option<String>,
    log_enabled: bool,
    last_mouse_down: Option<MouseDownTarget>,
    last_mouse_over_node_id: Option<usize>,
    last_key_down_text: Option<String>,
    next_object_id: u64,
    emulated_viewport: Option<(u32, u32)>,
    emulated_media: EmulatedMedia,
    next_new_document_script_id: u64,
    new_document_scripts: Vec<NewDocumentScript>,
    runtime_bindings: Vec<String>,
    download_behavior: DownloadBehavior,
}

#[allow(dead_code)] // Bookkeeping fields; required for future per-target routing.
struct Target {
    id: u64,
    session_id: u64,
    loader_id: u64,
    url: String,
    title: Option<String>,
    load_fired: bool,
    page: Option<Page>,
}

struct TargetSession {
    session_id: u64,
    target_id: u64,
}

#[derive(Debug, Clone)]
struct NewDocumentScript {
    identifier: String,
    source: String,
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
    /// Dispatch a single JSON request (synchronous — no await while state
    /// is borrowed). Returns outgoing lines: exactly one response followed by
    /// zero or more notifications caused by that response.
    pub fn handle_text_sync(&mut self, raw: &str) -> Vec<String> {
        let req: CdpRequest = match serde_json::from_str(raw) {
            Ok(r) => r,
            Err(e) => {
                return vec![error_response(None, -32700, &e.to_string())];
            }
        };
        let id = req.id;
        let outcome = self.dispatch(&req);
        let resp = match outcome.response {
            Ok(result) => CdpResponse {
                id,
                session_id: req.session_id.clone(),
                result: Some(result),
                error: None,
            },
            Err(error) => CdpResponse {
                id,
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
            Err(e) => out.push(error_response(Some(id), -32603, &e.to_string())),
        }
        out.extend(outcome.notifications);
        out
    }

    /// Pure dispatch on the method name.
    fn dispatch(&mut self, req: &CdpRequest) -> CdpDispatch {
        match req.method.as_str() {
            "Browser.getVersion" => CdpDispatch::ok(self.browser_get_version()),
            "Browser.close" => CdpDispatch::ok(json!({})),
            "Browser.setDownloadBehavior" => self.browser_set_download_behavior(req),
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
            "Page.reload" => self.page_reload(req),
            "Page.getNavigationHistory" => self.page_get_navigation_history(),
            "Page.navigateToHistoryEntry" => self.page_navigate_to_history_entry(req),
            "Page.captureScreenshot" => self.page_capture_screenshot(req),
            "Page.getLayoutMetrics" => {
                CdpDispatch::ok(page_get_layout_metrics(self.current_viewport()))
            }
            "Page.getFrameTree" => CdpDispatch::ok(self.page_get_frame_tree(req)),
            "Page.addScriptToEvaluateOnNewDocument" => {
                self.page_add_script_to_evaluate_on_new_document(req)
            }
            "Page.removeScriptToEvaluateOnNewDocument" => {
                self.page_remove_script_to_evaluate_on_new_document(req)
            }
            "Page.handleJavaScriptDialog" => self.page_handle_javascript_dialog(req),
            "Page.createIsolatedWorld" => self.page_create_isolated_world(req),
            "Page.setLifecycleEventsEnabled" => self.page_set_lifecycle_events_enabled(req),
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
            "Runtime.releaseObject"
            | "Runtime.releaseObjectGroup"
            | "Runtime.runIfWaitingForDebugger" => CdpDispatch::ok(json!({})),
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
            "DOM.enable"
            | "DOM.disable"
            | "Emulation.setTouchEmulationEnabled"
            | "Emulation.setFocusEmulationEnabled" => CdpDispatch::ok(json!({})),
            "Emulation.setDeviceMetricsOverride" => self.emulation_set_device_metrics_override(req),
            "Emulation.setEmulatedMedia" => self.emulation_set_emulated_media(req),
            "Emulation.clearDeviceMetricsOverride" => {
                self.emulated_viewport = None;
                CdpDispatch::ok(json!({}))
            }
            "DOM.scrollIntoViewIfNeeded" => CdpDispatch::ok(json!({})),
            "DOM.getDocument" => self.dom_get_document(req),
            "DOM.querySelector" => self.dom_query_selector(req),
            "DOM.querySelectorAll" => self.dom_query_selector_all(req),
            "DOM.describeNode" => self.dom_describe_node(req),
            "DOM.resolveNode" => self.dom_resolve_node(req),
            "DOM.getContentQuads" => self.dom_get_content_quads(req),
            "DOM.getBoxModel" => self.dom_get_box_model(req),
            "Input.dispatchMouseEvent" => self.input_dispatch_mouse_event(req),
            "Input.dispatchKeyEvent" => self.input_dispatch_key_event(req),
            "Input.insertText" => self.input_insert_text(req),
            _ => CdpDispatch::error(-32601, format!("method not found: {}", req.method)),
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
            .target_id;
        self.targets.iter().find(|target| target.id == target_id)
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

    fn target_create(&mut self, req: &CdpRequest) -> CdpDispatch {
        let url = req
            .params
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("about:blank")
            .to_owned();
        match self.push_loaded_target(url) {
            Ok(id) => {
                let target = self
                    .targets
                    .iter()
                    .find(|target| target.id == id)
                    .expect("created target is stored");
                CdpDispatch::ok_with_pre_response_notifications(
                    json!({ "targetId": format!("tab-{id}") }),
                    vec![target_attached_notification(
                        target,
                        req.session_id.as_deref(),
                    )],
                )
            }
            Err(e) => CdpDispatch::error(-32602, e),
        }
    }

    fn target_close(&mut self, req: &CdpRequest) -> CdpDispatch {
        let Some(target_id) = req.params.get("targetId").and_then(Value::as_str) else {
            return CdpDispatch::error(-32602, "Target.closeTarget: missing `targetId`");
        };
        let Some(index) = self
            .targets
            .iter()
            .position(|target| format!("tab-{}", target.id) == target_id)
        else {
            return CdpDispatch::ok(json!({ "success": false }));
        };
        let target = self.targets.remove(index);
        self.attached_sessions
            .retain(|session| session.target_id != target.id);
        if index == 0 {
            self.reset_js_for_navigation();
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
            .find(|target| format!("tab-{}", target.id) == target_id)
            .map(|target| target.id)
        else {
            return CdpDispatch::error(-32602, "Target.attachToTarget: unknown `targetId`");
        };
        let session_id = self.next_target_id.fetch_add(1, Ordering::SeqCst) + 1;
        self.attached_sessions.push(TargetSession {
            session_id,
            target_id,
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
        let target_id = target.id;
        let primary_session_id = target.session_id;
        if detached_session_id != primary_session_id {
            self.attached_sessions
                .retain(|session| session.session_id != detached_session_id);
        }
        let Some(target) = self.targets.iter().find(|target| target.id == target_id) else {
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

    fn target_get_targets(&self) -> Value {
        json!({
            "targetInfos": self.targets.iter().map(target_info_json).collect::<Vec<_>>()
        })
    }

    fn target_get_target_info(&self, req: &CdpRequest) -> Value {
        let requested = req.params.get("targetId").and_then(Value::as_str);
        let target = requested
            .and_then(|target_id| {
                self.targets
                    .iter()
                    .find(|target| format!("tab-{}", target.id) == target_id)
            })
            .or_else(|| self.targets.first());
        json!({
            "targetInfo": target.map(target_info_json).unwrap_or_else(|| json!({
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

    fn target_set_discover_targets(&self, req: &CdpRequest) -> CdpDispatch {
        let discover = req
            .params
            .get("discover")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !discover {
            return CdpDispatch::ok(json!({}));
        }
        let notifications = self
            .targets
            .iter()
            .map(|target| {
                notification(
                    "Target.targetCreated",
                    json!({ "targetInfo": target_info_json(target) }),
                    None,
                )
            })
            .collect();
        CdpDispatch::ok_with_notifications(json!({}), notifications)
    }

    fn target_set_auto_attach(&self, req: &CdpRequest) -> CdpDispatch {
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
        let notifications = self
            .targets
            .iter()
            .map(|target| target_attached_notification(target, req.session_id.as_deref()))
            .collect();
        CdpDispatch::ok_with_notifications(json!({}), notifications)
    }

    fn page_enable(&mut self, req: &CdpRequest) -> CdpDispatch {
        if self.page_enabled {
            return CdpDispatch::ok(json!({}));
        }
        self.page_enabled = true;
        let Some(target) = self.target_for_session(req.session_id.as_deref()) else {
            return CdpDispatch::ok(json!({}));
        };
        let frame_id = target_frame_id(target);
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
                    json!({ "frameId": target_frame_id(target) }),
                    req.session_id.as_deref(),
                ),
                notification(
                    "Page.frameNavigated",
                    json!({ "frame": frame_json(target) }),
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

    fn page_navigate(&mut self, req: &CdpRequest) -> CdpDispatch {
        let Some(url) = req.params.get("url").and_then(Value::as_str) else {
            return CdpDispatch::error(-32602, "Page.navigate: missing `url`");
        };
        let url = url.to_owned();
        let mut page = match load_cdp_page(&url) {
            Ok(page) => page,
            Err(e) => return CdpDispatch::error(-32602, e),
        };
        let final_url = page.url().to_owned();
        let mut history = self
            .targets
            .first()
            .and_then(|target| target.page.as_ref())
            .map(|page| page.session_history().clone())
            .unwrap_or_else(|| page.session_history().clone());
        if !self.targets.is_empty() {
            history.push(HistoryEntry::navigation(final_url.clone()));
            page.set_session_history(history);
        }
        self.reset_js_for_navigation();
        if let Err(e) = self.execute_page_scripts_if_needed(&mut page) {
            return CdpDispatch::error(-32603, e);
        }
        let mut notifications = self.drain_side_effect_notifications(req);
        let title = page.document().title();
        let loader_id = now_ms();
        if let Some(t) = self.targets.first_mut() {
            t.loader_id = loader_id;
            t.url = page.url().to_owned();
            t.title = title;
            t.load_fired = true;
            t.page = Some(page);
        } else {
            let id = self.next_target_id.fetch_add(1, Ordering::SeqCst) + 1;
            let session_id = self.next_target_id.fetch_add(1, Ordering::SeqCst) + 1;
            self.targets.push(Target {
                id,
                session_id,
                loader_id,
                url: page.url().to_owned(),
                title,
                load_fired: true,
                page: Some(page),
            });
        }
        let frame_id = self
            .targets
            .first()
            .map(target_frame_id)
            .unwrap_or_else(|| "tab-0".to_owned());
        let mut load_notifications =
            self.current_page_load_notifications(req.session_id.as_deref());
        load_notifications.append(&mut notifications);
        notifications = load_notifications;
        CdpDispatch::ok_with_notifications(
            json!({ "frameId": frame_id, "loaderId": format!("loader-{loader_id}") }),
            notifications,
        )
    }

    fn page_capture_screenshot(&self, req: &CdpRequest) -> CdpDispatch {
        if let Some(format) = req.params.get("format").and_then(Value::as_str)
            && !format.eq_ignore_ascii_case("png")
        {
            return CdpDispatch::error(-32602, "Page.captureScreenshot: only png is supported");
        }
        let viewport = match capture_viewport(&req.params, self.current_viewport()) {
            Ok(viewport) => viewport,
            Err(err) => return CdpDispatch::error(-32602, err),
        };
        let Some(page) = self.targets.first().and_then(|target| target.page.as_ref()) else {
            return CdpDispatch::error(-32000, "Page.captureScreenshot: no page loaded");
        };
        match crate::capture_screenshot_png(page, viewport) {
            Ok(png) => CdpDispatch::ok(json!({ "data": BASE64_STANDARD.encode(png) })),
            Err(err) => {
                CdpDispatch::error(-32603, format!("{}: {err}", codes::UNSUPPORTED_SCREENSHOT))
            }
        }
    }

    fn page_reload(&mut self, req: &CdpRequest) -> CdpDispatch {
        let Some((url, history)) = self
            .targets
            .first()
            .and_then(|target| target.page.as_ref())
            .map(|page| (page.url().to_owned(), page.session_history().clone()))
        else {
            return CdpDispatch::error(-32000, "Page.reload: no page loaded");
        };
        let mut page = match load_cdp_page(&url) {
            Ok(page) => page,
            Err(err) => return CdpDispatch::error(-32603, err),
        };
        page.set_session_history(history);
        self.reset_js_for_navigation();
        if let Err(err) = self.execute_page_scripts_if_needed(&mut page) {
            return CdpDispatch::error(-32603, err);
        }
        let title = page.document().title();
        let loader_id = now_ms();
        if let Some(target) = self.targets.first_mut() {
            target.loader_id = loader_id;
            target.url = page.url().to_owned();
            target.title = title;
            target.load_fired = true;
            target.page = Some(page);
        }
        CdpDispatch::ok_with_notifications(
            json!({}),
            self.current_page_load_notifications(req.session_id.as_deref()),
        )
    }

    fn page_get_navigation_history(&self) -> CdpDispatch {
        let Some(page) = self.targets.first().and_then(|target| target.page.as_ref()) else {
            return CdpDispatch::ok(json!({ "currentIndex": 0, "entries": [] }));
        };
        let history = page.session_history();
        let entries = history
            .entries()
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
            "currentIndex": history.index(),
            "entries": entries,
        }))
    }

    fn page_navigate_to_history_entry(&mut self, req: &CdpRequest) -> CdpDispatch {
        let entry_id = match req.params.get("entryId").and_then(Value::as_u64) {
            Some(entry_id) if entry_id > 0 => entry_id as usize,
            _ => {
                return CdpDispatch::error(
                    -32602,
                    "Page.navigateToHistoryEntry: missing `entryId`",
                );
            }
        };
        let Some(page) = self.targets.first().and_then(|target| target.page.as_ref()) else {
            return CdpDispatch::error(-32000, "Page.navigateToHistoryEntry: no page loaded");
        };
        let history = page.session_history();
        if entry_id > history.length() {
            return CdpDispatch::error(-32602, "Page.navigateToHistoryEntry: unknown `entryId`");
        }
        let target_index = entry_id - 1;
        let delta = target_index as i32 - history.index() as i32;
        let mut notifications = Vec::new();
        if let Err(err) =
            self.apply_history_traversal(delta, req.session_id.as_deref(), &mut notifications)
        {
            return CdpDispatch::error(-32603, err);
        }
        CdpDispatch::ok_with_notifications(json!({}), notifications)
    }

    fn page_get_frame_tree(&self, req: &CdpRequest) -> Value {
        let frame = self
            .target_for_session(req.session_id.as_deref())
            .map(frame_json)
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
        let frame_id = req
            .params
            .get("frameId")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| self.current_frame_id());
        let world_name = req
            .params
            .get("worldName")
            .and_then(Value::as_str)
            .unwrap_or("Vixen")
            .to_owned();
        self.isolated_world_name = Some(world_name.clone());
        let notifications = self.runtime_enabled.then(|| {
            self.runtime_utility_context_created_notification(
                &world_name,
                &frame_id,
                req.session_id.as_deref(),
            )
        });
        CdpDispatch::ok_with_notifications(
            json!({ "executionContextId": 2 }),
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
        CdpDispatch::ok(json!({}))
    }

    fn runtime_enable(&mut self, req: &CdpRequest) -> CdpDispatch {
        self.runtime_enabled = true;
        CdpDispatch::ok_with_notifications(
            json!({}),
            vec![self.runtime_main_context_created_notification(req.session_id.as_deref())],
        )
    }

    fn dom_get_document(&self, req: &CdpRequest) -> CdpDispatch {
        let Some(target) = self.target_for_session(req.session_id.as_deref()) else {
            return CdpDispatch::error(-32000, "DOM.getDocument: no page loaded");
        };
        if target.page.is_none() {
            return CdpDispatch::error(-32000, "DOM.getDocument: no page loaded");
        }
        let depth = req.params.get("depth").and_then(Value::as_i64).unwrap_or(1);
        CdpDispatch::ok(json!({ "root": cdp_document_node(target, depth) }))
    }

    fn dom_query_selector(&self, req: &CdpRequest) -> CdpDispatch {
        let selector = match req.params.get("selector").and_then(Value::as_str) {
            Some(selector) if !selector.is_empty() => selector,
            _ => return CdpDispatch::error(-32602, "DOM.querySelector: missing `selector`"),
        };
        if let Err(err) = cdp_query_root_node_id(&req.params, "DOM.querySelector") {
            return CdpDispatch::error(-32602, err);
        }
        let Some(page) = self
            .target_for_session(req.session_id.as_deref())
            .and_then(|target| target.page.as_ref())
        else {
            return CdpDispatch::error(-32000, "DOM.querySelector: no page loaded");
        };
        match page.query_selector_all(selector) {
            Ok(elements) => CdpDispatch::ok(json!({
                "nodeId": elements.first().map(|element| element.node_id).unwrap_or(0),
            })),
            Err(err) => CdpDispatch::error(-32602, format!("DOM.querySelector: {err}")),
        }
    }

    fn dom_query_selector_all(&self, req: &CdpRequest) -> CdpDispatch {
        let selector = match req.params.get("selector").and_then(Value::as_str) {
            Some(selector) if !selector.is_empty() => selector,
            _ => return CdpDispatch::error(-32602, "DOM.querySelectorAll: missing `selector`"),
        };
        if let Err(err) = cdp_query_root_node_id(&req.params, "DOM.querySelectorAll") {
            return CdpDispatch::error(-32602, err);
        }
        let Some(page) = self
            .target_for_session(req.session_id.as_deref())
            .and_then(|target| target.page.as_ref())
        else {
            return CdpDispatch::error(-32000, "DOM.querySelectorAll: no page loaded");
        };
        match page.query_selector_all(selector) {
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
        let Some(target) = self.target_for_session(req.session_id.as_deref()) else {
            return CdpDispatch::error(-32000, "DOM.describeNode: no page loaded");
        };
        if node_id == CDP_DOCUMENT_NODE_ID {
            return CdpDispatch::ok(json!({ "node": cdp_document_node(target, 1) }));
        }
        let Some(page) = target.page.as_ref() else {
            return CdpDispatch::error(-32000, "DOM.describeNode: no page loaded");
        };
        let element = match element_info_for_node_id(page, node_id) {
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
            Ok(value) => CdpDispatch::ok(json!({
                "object": self.remote_object_from_js_value(&value, Some(&object_id)),
            })),
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
        let Some(page) = self.targets.first().and_then(|target| target.page.as_ref()) else {
            return CdpDispatch::error(-32000, "DOM.getContentQuads: no page loaded");
        };
        let bbox = match element_bbox_for_node_id(page, node_id) {
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
        let Some(page) = self.targets.first().and_then(|target| target.page.as_ref()) else {
            return CdpDispatch::error(-32000, "DOM.getBoxModel: no page loaded");
        };
        let bbox = match element_bbox_for_node_id(page, node_id) {
            Ok(Some(bbox)) => bbox,
            Ok(None) => return CdpDispatch::error(-32000, "DOM.getBoxModel: node has no box"),
            Err(err) => return CdpDispatch::error(-32603, err),
        };
        CdpDispatch::ok(box_model_from_bbox(bbox))
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
        let object_expr = cdp_object_expr(object_id);
        let probe = format!(
            "(() => {{ const __o = {object_expr}; const __id = __o && __o.__vixenNodeId; return Number.isInteger(__id) && __id > 0 ? __id : null; }})()"
        );
        match self
            .evaluate_js(&probe)
            .map_err(|err| format!("{method}: {err}"))?
        {
            JsValue::Int32(id) if id > 0 => Ok(id as usize),
            JsValue::Number(id)
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
            JsValue::String(json) => {
                let rect: CdpDomRect = serde_json::from_str(&json)
                    .map_err(|err| format!("{method}: invalid object rect: {err}"))?;
                Ok(Some((rect.x, rect.y, rect.width, rect.height)))
            }
            JsValue::Null | JsValue::Undefined => Ok(None),
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
            JsValue::Int32(id) if id > 0 => Ok(Some(id as usize)),
            JsValue::Number(id)
                if id.is_finite() && id.fract() == 0.0 && id > 0.0 && id <= usize::MAX as f64 =>
            {
                Ok(Some(id as usize))
            }
            JsValue::Null | JsValue::Undefined => Ok(None),
            _ => Err("Input.dispatchMouseEvent: point probe returned a non-node id".to_owned()),
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
        let runtime_target_node_id = self.dom_node_id_at_point_from_runtime(x, y).ok().flatten();
        let Some(page) = self
            .targets
            .first_mut()
            .and_then(|target| target.page.as_mut())
        else {
            return CdpDispatch::error(-32000, "Input.dispatchMouseEvent: no page loaded");
        };
        let target_node_id = runtime_target_node_id
            .or_else(|| page.element_at(viewport, x, y).map(|target| target.node_id));
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

        for dom_event in dom_events {
            if let Err(err) = dispatch_dom_mouse_event(
                &mut self.js,
                page,
                dom_event.node_id,
                dom_event.event_type,
                MouseDispatchInit {
                    x,
                    y,
                    button,
                    buttons,
                    detail: click_count,
                    related_node_id: dom_event.related_node_id,
                    bubbles: dom_event.bubbles,
                    ctrl_key: req
                        .params
                        .get("modifiers")
                        .and_then(Value::as_i64)
                        .unwrap_or(0)
                        & 2
                        != 0,
                    shift_key: req
                        .params
                        .get("modifiers")
                        .and_then(Value::as_i64)
                        .unwrap_or(0)
                        & 8
                        != 0,
                    alt_key: req
                        .params
                        .get("modifiers")
                        .and_then(Value::as_i64)
                        .unwrap_or(0)
                        & 1
                        != 0,
                    meta_key: req
                        .params
                        .get("modifiers")
                        .and_then(Value::as_i64)
                        .unwrap_or(0)
                        & 4
                        != 0,
                    delta_x,
                    delta_y,
                },
            ) {
                return CdpDispatch::error(-32603, err);
            }
        }
        self.last_mouse_down = next_mouse_down;
        self.last_mouse_over_node_id = next_mouse_over_node_id;
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

        let Some(page) = self
            .targets
            .first_mut()
            .and_then(|target| target.page.as_mut())
        else {
            return CdpDispatch::error(-32000, "Input.dispatchKeyEvent: no page loaded");
        };

        if let Err(err) = dispatch_dom_key_event(
            &mut self.js,
            page,
            event_type,
            KeyDispatchInit {
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
            return CdpDispatch::error(-32603, err);
        }

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

        let Some(page) = self
            .targets
            .first_mut()
            .and_then(|target| target.page.as_mut())
        else {
            return CdpDispatch::error(-32000, "Input.insertText: no page loaded");
        };

        self.last_key_down_text = None;
        if let Err(err) = dispatch_dom_key_event(
            &mut self.js,
            page,
            "char",
            KeyDispatchInit {
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
            return CdpDispatch::error(
                -32603,
                err.replace("Input.dispatchKeyEvent", "Input.insertText"),
            );
        }

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
        let result = if return_by_value {
            self.evaluate_serialized_value(&format!(
                "globalThis.eval({})",
                serde_json::to_string(&expr).unwrap_or_else(|_| "\"undefined\"".to_owned())
            ))
            .map(|value| (serialized_remote_object(&value), None))
        } else {
            self.evaluate_js(&expr).map(|value| {
                let object_id = if matches!(value, JsValue::Object) {
                    self.store_evaluated_object(&expr).ok()
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
            Err(e) => self
                .legacy_dom_evaluate(&expr)
                .unwrap_or_else(|| self.runtime_exception_result(e, req)),
        }
    }

    fn legacy_dom_evaluate(&self, expr: &str) -> Option<CdpDispatch> {
        if !crate::looks_like_dom_eval(expr) || crate::uses_runtime_dom_eval(expr) {
            return None;
        }
        let result = self
            .targets
            .first()
            .and_then(|target| target.page.as_ref())?
            .evaluate_dom_expression(expr)?;
        Some(match result {
            Ok(value) => CdpDispatch::ok(remote_object_from_text(value)),
            Err(e) => CdpDispatch::ok(json!({
                "result": { "type": "undefined" },
                "exceptionDetails": {
                    "exceptionId": 1,
                    "text": e,
                    "code": "dom.eval",
                }
            })),
        })
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

        let result = if return_by_value {
            self.evaluate_serialized_value(&call_expr)
                .map(|value| serialized_remote_object(&value))
        } else {
            let object_id = self.next_remote_object_id();
            let object_id_json =
                serde_json::to_string(&object_id).unwrap_or_else(|_| "\"\"".into());
            let store_expr = format!(
                "(async () => {{ globalThis.__vixenCdpObjects = globalThis.__vixenCdpObjects || Object.create(null); const __v = await ({call_expr}); globalThis.__vixenCdpObjects[{object_id_json}] = __v; return __v; }})()"
            );
            self.evaluate_js(&store_expr)
                .map(|value| self.remote_object_from_js_value(&value, Some(&object_id)))
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
        let own_properties = req
            .params
            .get("ownProperties")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let object_id = serde_json::to_string(object_id).unwrap_or_else(|_| "\"\"".into());
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
                    const objectId = 'vixen-object-js-' + (++globalThis.__vixenCdpObjectCounter);
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
            Ok(JsValue::String(properties)) => match serde_json::from_str::<Value>(&properties) {
                Ok(result) => CdpDispatch::ok(json!({
                    "result": result,
                    "internalProperties": [],
                    "privateProperties": [],
                })),
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
        let return_by_value = req
            .params
            .get("returnByValue")
            .and_then(Value::as_bool)
            .unwrap_or(false);
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
                "(async () => {{ globalThis.__vixenCdpObjects = globalThis.__vixenCdpObjects || Object.create(null); const __v = await ({await_expr}); globalThis.__vixenCdpObjects[{object_id_json}] = __v; return __v; }})()"
            );
            self.evaluate_js(&store_expr).map(|value| {
                let stored_object_id =
                    matches!(value, JsValue::Object).then_some(object_id.as_str());
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
        if self.js.is_some()
            && let Err(err) = self.install_runtime_binding(name)
        {
            return CdpDispatch::error(-32603, err);
        }
        CdpDispatch::ok(json!({}))
    }

    fn install_runtime_binding(&mut self, name: &str) -> Result<(), String> {
        let Some(rt) = self.js.as_mut() else {
            return Ok(());
        };
        let script = runtime_binding_install_script(name);
        if let Some(page) = self
            .targets
            .first_mut()
            .and_then(|target| target.page.as_mut())
        {
            rt.evaluate_with_page_mut(&script, page)
        } else {
            rt.evaluate(&script)
        }
        .map(|_| ())
        .map_err(|err| format!("runtime binding install failed: {err}"))
    }

    fn runtime_exception_result(&mut self, e: EngineError, req: &CdpRequest) -> CdpDispatch {
        let code = e.code();
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

    fn evaluate_js(&mut self, expr: &str) -> Result<JsValue, EngineError> {
        let viewport = self.current_viewport();
        let media = self.emulated_media.clone();
        if self.js.is_none() {
            self.js = Some(JsRuntime::new()?);
        }
        let binding_scripts = runtime_binding_install_scripts(&self.runtime_bindings);
        let page = self
            .targets
            .first_mut()
            .and_then(|target| target.page.as_mut());
        let rt = self.js.as_mut().expect("runtime just initialised");
        let emulation_script = emulation_override_script(viewport, &media);
        if let Some(page) = page {
            for script in binding_scripts {
                rt.evaluate_with_page_mut(&script, page)?;
            }
            rt.evaluate_with_page_mut(&emulation_script, page)?;
            rt.evaluate_with_page_mut(expr, page)
        } else {
            for script in binding_scripts {
                rt.evaluate(&script)?;
            }
            rt.evaluate(&emulation_script)?;
            rt.evaluate(expr)
        }
    }

    fn current_viewport(&self) -> (u32, u32) {
        self.emulated_viewport.unwrap_or(DEFAULT_CAPTURE_VIEWPORT)
    }

    fn evaluate_serialized_value(&mut self, expr: &str) -> Result<Value, EngineError> {
        let expr = format!(
            "(async () => {{ const __v = await ({expr}); return JSON.stringify({{ t: typeof __v, v: __v === undefined ? null : __v }}); }})()"
        );
        match self.evaluate_js(&expr)? {
            JsValue::String(serialized) => Ok(serde_json::from_str(&serialized)
                .unwrap_or_else(|_| json!({ "t": "undefined", "v": Value::Null }))),
            other => Ok(json!({ "t": "string", "v": other.to_display() })),
        }
    }

    fn store_evaluated_object(&mut self, expr: &str) -> Result<String, EngineError> {
        let object_id = self.next_remote_object_id();
        let object_id_json = serde_json::to_string(&object_id).unwrap_or_else(|_| "\"\"".into());
        let expr_json = serde_json::to_string(expr).unwrap_or_else(|_| "\"undefined\"".into());
        let store_expr = format!(
            "globalThis.__vixenCdpObjects = globalThis.__vixenCdpObjects || Object.create(null); globalThis.__vixenCdpObjects[{object_id_json}] = globalThis.eval({expr_json}); undefined"
        );
        self.evaluate_js(&store_expr)?;
        Ok(object_id)
    }

    fn next_remote_object_id(&mut self) -> String {
        self.next_object_id += 1;
        format!("vixen-object-{}", self.next_object_id)
    }

    fn remote_object_from_js_value(&mut self, value: &JsValue, object_id: Option<&str>) -> Value {
        match value {
            JsValue::Int32(n) => {
                json!({ "type": "number", "value": n, "description": value.to_display() })
            }
            JsValue::Number(n) => {
                json!({ "type": "number", "value": n, "description": value.to_display() })
            }
            JsValue::String(s) => {
                json!({ "type": "string", "value": s, "description": value.to_display() })
            }
            JsValue::Bool(b) => {
                json!({ "type": "boolean", "value": b, "description": value.to_display() })
            }
            JsValue::Null => {
                json!({ "type": "object", "subtype": "null", "value": Value::Null, "description": "null" })
            }
            JsValue::Undefined => json!({ "type": "undefined", "description": "undefined" }),
            JsValue::Object => {
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
            JsValue::String(value) => Some(value),
            _ => None,
        }
    }

    fn seed_initial_target(&mut self, url: String) -> Result<(), String> {
        self.push_loaded_target(url).map(|_| ())
    }

    fn push_loaded_target(&mut self, url: String) -> Result<u64, String> {
        let mut page = load_cdp_page(&url)?;
        if self.targets.is_empty() {
            self.execute_page_scripts_if_needed(&mut page)?;
            self.discard_console_events();
            self.discard_dialog_events();
        }
        let title = page.document().title();
        let id = self.next_target_id.fetch_add(1, Ordering::SeqCst) + 1;
        let session_id = self.next_target_id.fetch_add(1, Ordering::SeqCst) + 1;
        self.targets.push(Target {
            id,
            session_id,
            loader_id: id,
            url,
            title,
            load_fired: false,
            page: Some(page),
        });
        Ok(id)
    }

    fn execute_page_scripts_if_needed(&mut self, page: &mut Page) -> Result<(), String> {
        if self.new_document_scripts.is_empty() && !page.has_classic_scripts() {
            return Ok(());
        }
        if self.js.is_none() {
            let rt = JsRuntime::new().map_err(|e| format!("JS runtime init failed: {e}"))?;
            self.js = Some(rt);
        }
        let viewport = self.current_viewport();
        let media = self.emulated_media.clone();
        let scripts = self.new_document_scripts.clone();
        let binding_scripts = runtime_binding_install_scripts(&self.runtime_bindings);
        let emulation_script = emulation_override_script(viewport, &media);
        let rt = self.js.as_mut().expect("runtime just initialised");
        for script in binding_scripts {
            rt.evaluate_with_page_mut(&script, page)
                .map_err(|e| format!("runtime binding install failed: {e}"))?;
        }
        rt.evaluate_with_page_mut(&emulation_script, page)
            .map_err(|e| format!("new document emulation script failed: {e}"))?;
        for script in scripts {
            rt.evaluate_with_page_mut(&script.source, page)
                .map_err(|e| format!("new document script failed: {e}"))?;
        }
        if !page.has_classic_scripts() {
            return Ok(());
        }
        rt.execute_page_scripts(page)
            .map(|_| ())
            .map_err(|e| format!("page script failed: {e}"))
    }

    fn drain_navigation_notifications(&mut self, req: &CdpRequest) -> Result<Vec<String>, String> {
        let actions = match self.js.as_mut() {
            Some(js) => js.drain_navigation_actions().map_err(|e| e.to_string())?,
            None => Vec::new(),
        };
        self.apply_navigation_actions(actions, req.session_id.as_deref())
    }

    fn apply_navigation_actions(
        &mut self,
        actions: Vec<JsNavigationAction>,
        session_id: Option<&str>,
    ) -> Result<Vec<String>, String> {
        let mut notifications = Vec::new();
        for action in actions {
            match action {
                JsNavigationAction::Navigate { url, replace } => {
                    self.navigate_from_action(url, replace, session_id, &mut notifications)?;
                }
                JsNavigationAction::SetContent { html } => {
                    self.set_content_from_action(html, session_id, &mut notifications)?;
                }
                JsNavigationAction::FormSubmit {
                    form_id,
                    form_node_id,
                    submitter_node_id,
                    action,
                    method,
                    ..
                } => {
                    if let Some(url) = self.form_submission_navigation_url(
                        &form_id,
                        form_node_id,
                        submitter_node_id,
                        &action,
                        &method,
                    )? {
                        self.navigate_from_action(url, false, session_id, &mut notifications)?;
                    }
                }
                JsNavigationAction::HistoryPush {
                    url,
                    state_json,
                    title,
                } => self.apply_history_state_action(
                    url,
                    state_json,
                    title,
                    false,
                    session_id,
                    &mut notifications,
                )?,
                JsNavigationAction::HistoryReplace {
                    url,
                    state_json,
                    title,
                } => self.apply_history_state_action(
                    url,
                    state_json,
                    title,
                    true,
                    session_id,
                    &mut notifications,
                )?,
                JsNavigationAction::HistoryTraverse { delta } => {
                    self.apply_history_traversal(delta, session_id, &mut notifications)?;
                }
            }
        }
        Ok(notifications)
    }

    fn set_content_from_action(
        &mut self,
        html: String,
        session_id: Option<&str>,
        notifications: &mut Vec<String>,
    ) -> Result<(), String> {
        let (url, history) =
            if let Some(page) = self.targets.first().and_then(|target| target.page.as_ref()) {
                (page.url().to_owned(), page.session_history().clone())
            } else {
                let page = Page::from_html("about:blank", "")
                    .map_err(|err| format!("set content fallback page: {err}"))?;
                (page.url().to_owned(), page.session_history().clone())
            };
        let mut page = Page::from_html(url, &html).map_err(|err| format!("set content: {err}"))?;
        page.set_session_history(history);

        self.reset_js_for_navigation();
        self.execute_page_scripts_if_needed(&mut page)?;
        let title = page.document().title();
        let loader_id = now_ms();
        if let Some(target) = self.targets.first_mut() {
            target.loader_id = loader_id;
            target.url = page.url().to_owned();
            target.title = title;
            target.load_fired = true;
            target.page = Some(page);
        } else {
            let id = self.next_target_id.fetch_add(1, Ordering::SeqCst) + 1;
            let session = self.next_target_id.fetch_add(1, Ordering::SeqCst) + 1;
            self.targets.push(Target {
                id,
                session_id: session,
                loader_id,
                url: page.url().to_owned(),
                title,
                load_fired: true,
                page: Some(page),
            });
        }
        notifications.extend(self.current_page_load_notifications(session_id));
        Ok(())
    }

    fn navigate_from_action(
        &mut self,
        url: String,
        replace: bool,
        session_id: Option<&str>,
        notifications: &mut Vec<String>,
    ) -> Result<(), String> {
        let mut page = load_cdp_page(&url)?;
        let final_url = page.url().to_owned();
        let mut history = self
            .targets
            .first()
            .and_then(|target| target.page.as_ref())
            .map(|page| page.session_history().clone())
            .unwrap_or_else(|| page.session_history().clone());
        if self
            .targets
            .first()
            .and_then(|target| target.page.as_ref())
            .is_some()
        {
            let entry = HistoryEntry::navigation(final_url);
            if replace {
                history.replace(entry);
            } else {
                history.push(entry);
            }
            page.set_session_history(history);
        }

        self.reset_js_for_navigation();
        self.execute_page_scripts_if_needed(&mut page)?;
        let title = page.document().title();
        let loader_id = now_ms();
        if let Some(target) = self.targets.first_mut() {
            target.loader_id = loader_id;
            target.url = page.url().to_owned();
            target.title = title;
            target.load_fired = true;
            target.page = Some(page);
        } else {
            let id = self.next_target_id.fetch_add(1, Ordering::SeqCst) + 1;
            let session = self.next_target_id.fetch_add(1, Ordering::SeqCst) + 1;
            self.targets.push(Target {
                id,
                session_id: session,
                loader_id,
                url: page.url().to_owned(),
                title,
                load_fired: true,
                page: Some(page),
            });
        }
        notifications.extend(self.current_page_load_notifications(session_id));
        Ok(())
    }

    fn form_submission_navigation_url(
        &self,
        form_id: &str,
        form_node_id: usize,
        submitter_node_id: Option<usize>,
        action: &str,
        method: &str,
    ) -> Result<Option<String>, String> {
        let Some(page) = self.targets.first().and_then(|target| target.page.as_ref()) else {
            return Err("form submission has no page loaded".to_owned());
        };
        let submission = if form_node_id != 0 {
            Some(page.form_submission_by_node_id(form_node_id, submitter_node_id)?)
        } else if !form_id.is_empty() {
            Some(page.form_submission(form_id)?)
        } else {
            None
        };
        let action = submission
            .as_ref()
            .map(|submission| submission.action.clone())
            .unwrap_or_else(|| {
                page.resolve_url(action)
                    .unwrap_or_else(|| action.to_owned())
            });
        let method = submission
            .as_ref()
            .map(|submission| submission.method.clone())
            .unwrap_or_else(|| method.to_owned())
            .to_ascii_lowercase();
        match method.as_str() {
            "dialog" => Ok(None),
            "post" => Ok(Some(action)),
            _ => {
                let Some(submission) = submission else {
                    return Ok(Some(action));
                };
                Ok(Some(append_form_query(&action, &submission.body)?))
            }
        }
    }

    fn apply_history_state_action(
        &mut self,
        url: String,
        state_json: String,
        title: String,
        replace: bool,
        session_id: Option<&str>,
        notifications: &mut Vec<String>,
    ) -> Result<(), String> {
        let (frame_id, navigated_url) = {
            let Some(target) = self.targets.first_mut() else {
                return Err("history action has no target".to_owned());
            };
            let frame_id = target_frame_id(target);
            let Some(page) = target.page.as_mut() else {
                return Err("history action has no page loaded".to_owned());
            };
            ensure_same_origin_history_url(page.url(), &url)?;
            let mut history = page.session_history().clone();
            let mut entry = HistoryEntry::push_state(url, state_json.into_bytes());
            if !title.is_empty() {
                entry.title = Some(title);
            }
            if replace {
                history.replace(entry);
            } else {
                history.push(entry);
            }
            page.set_session_history(history);
            target.url = page.url().to_owned();
            target.title = page.document().title();
            (frame_id, target.url.clone())
        };

        if let (Some(js), Some(page)) = (
            self.js.as_mut(),
            self.targets.first().and_then(|target| target.page.as_ref()),
        ) {
            js.sync_page_realm_key(page);
        }
        notifications.push(notification(
            "Page.navigatedWithinDocument",
            json!({ "frameId": frame_id, "url": navigated_url }),
            session_id,
        ));
        Ok(())
    }

    fn apply_history_traversal(
        &mut self,
        delta: i32,
        session_id: Option<&str>,
        notifications: &mut Vec<String>,
    ) -> Result<(), String> {
        if delta == 0 {
            return Ok(());
        }
        let Some(page) = self.targets.first().and_then(|target| target.page.as_ref()) else {
            return Err("history traversal has no page loaded".to_owned());
        };
        let mut history = page.session_history().clone();
        let Some(entry) = history.go(delta).cloned() else {
            return Ok(());
        };

        if entry.state.is_some() {
            let (frame_id, navigated_url) = {
                let target = self.targets.first_mut().expect("target checked above");
                let frame_id = target_frame_id(target);
                let page = target.page.as_mut().expect("page checked above");
                page.set_session_history(history);
                target.url = page.url().to_owned();
                target.title = page.document().title();
                (frame_id, target.url.clone())
            };
            self.reset_js_for_navigation();
            notifications.push(notification(
                "Page.navigatedWithinDocument",
                json!({ "frameId": frame_id, "url": navigated_url }),
                session_id,
            ));
            return Ok(());
        }

        let mut page = load_cdp_page(&entry.url)?;
        page.set_session_history(history);
        self.reset_js_for_navigation();
        self.execute_page_scripts_if_needed(&mut page)?;
        let title = page.document().title();
        let loader_id = now_ms();
        if let Some(target) = self.targets.first_mut() {
            target.loader_id = loader_id;
            target.url = page.url().to_owned();
            target.title = title;
            target.load_fired = true;
            target.page = Some(page);
        }
        notifications.extend(self.current_page_load_notifications(session_id));
        Ok(())
    }

    fn current_origin_for_session(&self, session_id: Option<&str>) -> String {
        self.target_for_session(session_id)
            .map(|target| origin_for_url(&target.url))
            .unwrap_or_else(|| "://".to_owned())
    }

    fn current_frame_id(&self) -> String {
        self.targets
            .first()
            .map(target_frame_id)
            .unwrap_or_else(|| "tab-0".to_owned())
    }

    fn current_frame_id_for_session(&self, session_id: Option<&str>) -> String {
        self.target_for_session(session_id)
            .map(target_frame_id)
            .unwrap_or_else(|| "tab-0".to_owned())
    }

    fn current_loader_id_for_session(&self, session_id: Option<&str>) -> u64 {
        self.target_for_session(session_id)
            .map(|target| target.loader_id)
            .unwrap_or(0)
    }

    fn runtime_main_context_created_notification(&self, session_id: Option<&str>) -> String {
        let frame_id = self.current_frame_id_for_session(session_id);
        self.runtime_context_created_notification(RuntimeContextNotification {
            id: 1,
            name: "Vixen",
            unique_prefix: "vixen-main-context",
            is_default: true,
            context_type: "default",
            frame_id: &frame_id,
            session_id,
        })
    }

    fn runtime_utility_context_created_notification(
        &self,
        world_name: &str,
        frame_id: &str,
        session_id: Option<&str>,
    ) -> String {
        self.runtime_context_created_notification(RuntimeContextNotification {
            id: 2,
            name: world_name,
            unique_prefix: "vixen-utility-context",
            is_default: false,
            context_type: "isolated",
            frame_id,
            session_id,
        })
    }

    fn runtime_context_created_notification(
        &self,
        context: RuntimeContextNotification<'_>,
    ) -> String {
        notification(
            "Runtime.executionContextCreated",
            json!({
                "context": {
                    "id": context.id,
                    "origin": self.current_origin_for_session(context.session_id),
                    "name": context.name,
                    "uniqueId": format!("{}-{}", context.unique_prefix, self.current_loader_id_for_session(context.session_id)),
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

    fn current_page_load_notifications(&self, session_id: Option<&str>) -> Vec<String> {
        let Some(target) = self.target_for_session(session_id) else {
            return Vec::new();
        };
        let frame_id = target_frame_id(target);
        let timestamp = now_ms();
        let mut notifications = Vec::new();
        if self.network_enabled {
            notifications.push(network_request_will_be_sent_notification(
                target, &frame_id, timestamp, session_id,
            ));
        }
        notifications.push(notification(
            "Page.frameStartedLoading",
            json!({ "frameId": &frame_id }),
            session_id,
        ));
        if self.lifecycle_events_enabled {
            notifications.push(page_lifecycle_event_notification(
                target, "init", timestamp, session_id,
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
            json!({ "frame": frame_json(target) }),
            session_id,
        ));
        if self.lifecycle_events_enabled {
            notifications.push(page_lifecycle_event_notification(
                target, "commit", timestamp, session_id,
            ));
        }
        if self.network_enabled {
            notifications.push(network_response_received_notification(
                target, &frame_id, timestamp, session_id,
            ));
        }
        if self.runtime_enabled {
            notifications.push(self.runtime_main_context_created_notification(session_id));
            if let Some(world_name) = self.isolated_world_name.as_deref() {
                notifications.push(self.runtime_utility_context_created_notification(
                    world_name, &frame_id, session_id,
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
                target,
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
                target, "load", timestamp, session_id,
            ));
        }
        if self.network_enabled {
            notifications.push(network_loading_finished_notification(
                target, timestamp, session_id,
            ));
        }
        notifications.push(notification(
            "Page.frameStoppedLoading",
            json!({ "frameId": target_frame_id(target) }),
            session_id,
        ));
        notifications
    }

    fn drain_console_notifications(&mut self, req: &CdpRequest) -> Vec<String> {
        let events = match self.js.as_mut() {
            Some(js) => js.drain_console_events().unwrap_or_default(),
            None => Vec::new(),
        };
        if !self.runtime_enabled {
            return Vec::new();
        }
        events
            .into_iter()
            .map(|event| console_notification(event, req.session_id.as_deref()))
            .collect()
    }

    fn drain_dialog_notifications(&mut self, req: &CdpRequest) -> Vec<String> {
        let events = match self.js.as_mut() {
            Some(js) => js.drain_dialog_events().unwrap_or_default(),
            None => Vec::new(),
        };
        events
            .into_iter()
            .map(|event| self.dialog_notification(event, req.session_id.as_deref()))
            .collect()
    }

    fn drain_binding_notifications(&mut self, req: &CdpRequest) -> Vec<String> {
        let events = match self.js.as_mut() {
            Some(js) => js.drain_binding_events().unwrap_or_default(),
            None => Vec::new(),
        };
        if !self.runtime_enabled {
            return Vec::new();
        }
        events
            .into_iter()
            .map(|event| binding_notification(event, req.session_id.as_deref()))
            .collect()
    }

    fn drain_network_notifications(&mut self, req: &CdpRequest) -> Vec<String> {
        let events = match self.js.as_mut() {
            Some(js) => js.drain_network_events().unwrap_or_default(),
            None => Vec::new(),
        };
        if !self.network_enabled {
            return Vec::new();
        }
        let session_id = req.session_id.as_deref();
        let frame_id = self.current_frame_id_for_session(session_id);
        let loader_id = self.current_loader_id_for_session(session_id);
        let document_url = self
            .target_for_session(session_id)
            .map(|target| target.url.as_str().to_owned())
            .unwrap_or_else(|| "about:blank".to_owned());
        let timestamp = now_ms();
        events
            .into_iter()
            .flat_map(|event| {
                network_fetch_notifications(
                    event,
                    &frame_id,
                    loader_id,
                    &document_url,
                    timestamp,
                    session_id,
                )
            })
            .collect()
    }

    fn drain_side_effect_notifications(&mut self, req: &CdpRequest) -> Vec<String> {
        let mut notifications = self.drain_console_notifications(req);
        notifications.extend(self.drain_dialog_notifications(req));
        notifications.extend(self.drain_binding_notifications(req));
        notifications.extend(self.drain_network_notifications(req));
        notifications
    }

    fn dialog_notification(&self, event: JsDialogEvent, session_id: Option<&str>) -> String {
        notification(
            "Page.javascriptDialogOpening",
            json!({
                "url": self.targets.first().map(|target| target.url.as_str()).unwrap_or("about:blank"),
                "frameId": self.current_frame_id(),
                "message": event.message,
                "type": event.kind,
                "hasBrowserHandler": true,
                "defaultPrompt": event.default_prompt,
            }),
            session_id,
        )
    }

    fn discard_console_events(&mut self) {
        if let Some(js) = self.js.as_mut() {
            let _ = js.drain_console_events();
        }
    }

    fn discard_dialog_events(&mut self) {
        if let Some(js) = self.js.as_mut() {
            let _ = js.drain_dialog_events();
        }
    }

    fn reset_js_for_navigation(&mut self) {
        if let Some(js) = self.js.as_mut() {
            js.reset_realm();
        }
        self.last_mouse_down = None;
        self.last_mouse_over_node_id = None;
        self.last_key_down_text = None;
    }
}

impl CdpState {
    /// Construct a state pre-seeded with a JS runtime (used by tests so they
    /// don't pay JS runtime init cost on every call).
    pub fn with_runtime(rt: JsRuntime) -> Self {
        Self {
            next_target_id: AtomicU64::new(0),
            targets: Vec::new(),
            attached_sessions: Vec::new(),
            js: Some(rt),
            runtime_enabled: false,
            network_enabled: false,
            page_enabled: false,
            lifecycle_events_enabled: false,
            isolated_world_name: None,
            log_enabled: false,
            last_mouse_down: None,
            last_mouse_over_node_id: None,
            last_key_down_text: None,
            next_object_id: 0,
            emulated_viewport: None,
            emulated_media: EmulatedMedia::default(),
            next_new_document_script_id: 0,
            new_document_scripts: Vec::new(),
            runtime_bindings: Vec::new(),
            download_behavior: DownloadBehavior::default(),
        }
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
            response: Err(CdpError {
                code,
                message: message.into(),
            }),
            notifications: Vec::new(),
        }
    }
}

fn load_cdp_page(url: &str) -> Result<Page, String> {
    if url == "about:blank" {
        return Page::from_html("about:blank", "").map_err(|e| format!("parse about:blank: {e}"));
    }
    crate::load_page(url)
}

fn append_form_query(action: &str, body: &[u8]) -> Result<String, String> {
    if body.is_empty() {
        return Ok(action.to_owned());
    }
    let mut url =
        url::Url::parse(action).map_err(|err| format!("invalid form action URL: {err}"))?;
    let body = String::from_utf8_lossy(body);
    let query = match url.query() {
        Some(existing) if !existing.is_empty() => format!("{existing}&{body}"),
        _ => body.into_owned(),
    };
    url.set_query(Some(&query));
    Ok(url.to_string())
}

fn ensure_same_origin_history_url(current: &str, next: &str) -> Result<(), String> {
    let current = url::Url::parse(current).map_err(|err| format!("invalid current URL: {err}"))?;
    let next = url::Url::parse(next).map_err(|err| format!("invalid history URL: {err}"))?;
    if current.scheme() == "file" && next.scheme() == "file" {
        return Ok(());
    }
    if current.origin() == next.origin() {
        Ok(())
    } else {
        Err("history state URL must be same-origin".to_owned())
    }
}

fn target_info_json(target: &Target) -> Value {
    json!({
        "targetId": format!("tab-{}", target.id),
        "type": "page",
        "title": target.title.as_deref().unwrap_or(""),
        "url": target.url,
        "attached": false,
        "canAccessOpener": false,
        "browserContextId": "default",
    })
}

fn target_attached_notification(target: &Target, session_id: Option<&str>) -> String {
    notification(
        "Target.attachedToTarget",
        json!({
            "sessionId": format!("sess-{}", target.session_id),
            "targetInfo": target_info_json(target),
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
            "targetId": format!("tab-{}", target.id),
        }),
        session_id,
    )
}

fn target_frame_id(target: &Target) -> String {
    format!("tab-{}", target.id)
}

fn page_lifecycle_event_notification(
    target: &Target,
    name: &str,
    timestamp: u64,
    session_id: Option<&str>,
) -> String {
    notification(
        "Page.lifecycleEvent",
        json!({
            "frameId": target_frame_id(target),
            "loaderId": format!("loader-{}", target.loader_id),
            "name": name,
            "timestamp": timestamp,
        }),
        session_id,
    )
}

fn frame_json(target: &Target) -> Value {
    json!({
        "id": target_frame_id(target),
        "loaderId": format!("loader-{}", target.loader_id),
        "url": target.url,
        "securityOrigin": origin_for_url(&target.url),
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

fn cdp_document_node(target: &Target, depth: i64) -> Value {
    let child = target
        .page
        .as_ref()
        .and_then(|page| page.query_selector_all("*").ok())
        .and_then(|elements| elements.into_iter().next())
        .map(|element| cdp_node_from_element(&element));
    let child_node_count = usize::from(child.is_some());
    let mut node = json!({
        "nodeId": CDP_DOCUMENT_NODE_ID,
        "backendNodeId": CDP_DOCUMENT_NODE_ID,
        "nodeType": 9,
        "nodeName": "#document",
        "localName": "",
        "nodeValue": "",
        "childNodeCount": child_node_count,
        "documentURL": target.url.as_str(),
        "baseURL": target.url.as_str(),
        "xmlVersion": "",
    });
    if depth != 0
        && let Some(child) = child
    {
        node["children"] = json!([child]);
    }
    node
}

fn element_info_for_node_id(
    page: &Page,
    node_id: usize,
) -> Result<Option<vixen_api::ElementInfo>, String> {
    Ok(page
        .query_selector_all_in_viewport("*", DEFAULT_CAPTURE_VIEWPORT)?
        .into_iter()
        .find(|element| element.node_id == node_id))
}

fn element_bbox_for_node_id(
    page: &Page,
    node_id: usize,
) -> Result<Option<(f64, f64, f64, f64)>, String> {
    Ok(element_info_for_node_id(page, node_id)?.and_then(element_cdp_bbox))
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

struct MouseDispatchInit {
    x: f64,
    y: f64,
    button: MouseButton,
    buttons: i64,
    detail: i64,
    related_node_id: Option<usize>,
    bubbles: bool,
    ctrl_key: bool,
    shift_key: bool,
    alt_key: bool,
    meta_key: bool,
    delta_x: f64,
    delta_y: f64,
}

struct KeyDispatchInit {
    key: String,
    code: String,
    text: String,
    apply_text: bool,
    ctrl_key: bool,
    shift_key: bool,
    alt_key: bool,
    meta_key: bool,
    repeat: bool,
    location: i64,
}

fn dispatch_dom_mouse_event(
    js: &mut Option<JsRuntime>,
    page: &mut Page,
    node_id: usize,
    event_type: &str,
    init: MouseDispatchInit,
) -> Result<(), String> {
    if js.is_none() {
        *js = Some(JsRuntime::new().map_err(|e| format!("JS runtime init failed: {e}"))?);
    }
    let event_type = serde_json::to_string(event_type).map_err(|e| e.to_string())?;
    let init = json!({
        "bubbles": init.bubbles,
        "clientX": init.x,
        "clientY": init.y,
        "screenX": init.x,
        "screenY": init.y,
        "button": init.button.dom_button_code(),
        "buttons": init.buttons,
        "detail": init.detail,
        "relatedNodeId": init.related_node_id,
        "ctrlKey": init.ctrl_key,
        "shiftKey": init.shift_key,
        "altKey": init.alt_key,
        "metaKey": init.meta_key,
        "deltaX": init.delta_x,
        "deltaY": init.delta_y,
        "deltaZ": 0,
        "deltaMode": 0,
    });
    let init = serde_json::to_string(&init).map_err(|e| e.to_string())?;
    let src = format!(
        "globalThis.__vixenDispatchMouseEvent ? globalThis.__vixenDispatchMouseEvent({node_id}, {event_type}, {init}) : false"
    );
    js.as_mut()
        .expect("runtime just initialised")
        .evaluate_with_page_mut(&src, page)
        .map(|_| ())
        .map_err(|e| format!("Input.dispatchMouseEvent: {e}"))
}

fn dispatch_dom_key_event(
    js: &mut Option<JsRuntime>,
    page: &mut Page,
    event_type: &str,
    init: KeyDispatchInit,
) -> Result<(), String> {
    if js.is_none() {
        *js = Some(JsRuntime::new().map_err(|e| format!("JS runtime init failed: {e}"))?);
    }
    let event_type = serde_json::to_string(event_type).map_err(|e| e.to_string())?;
    let init = json!({
        "key": init.key,
        "code": init.code,
        "location": init.location,
        "ctrlKey": init.ctrl_key,
        "shiftKey": init.shift_key,
        "altKey": init.alt_key,
        "metaKey": init.meta_key,
        "repeat": init.repeat,
        "isComposing": false,
        "text": init.text,
        "inputText": init.text,
        "applyText": init.apply_text,
    });
    let init = serde_json::to_string(&init).map_err(|e| e.to_string())?;
    let src = format!(
        "globalThis.__vixenDispatchKeyEvent ? globalThis.__vixenDispatchKeyEvent({event_type}, {init}) : false"
    );
    js.as_mut()
        .expect("runtime just initialised")
        .evaluate_with_page_mut(&src, page)
        .map(|_| ())
        .map_err(|e| format!("Input.dispatchKeyEvent: {e}"))
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

fn remote_object_from_text(value: String) -> Value {
    json!({
        "result": {
            "type": "string",
            "value": value,
        }
    })
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
}

#[derive(Debug, Serialize)]
struct CdpNotification {
    method: String,
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    params: Value,
}

fn error_response(id: Option<u64>, code: i32, message: &str) -> String {
    serde_json::to_string(&json!({
        "id": id,
        "error": { "code": code, "message": message }
    }))
    .unwrap_or_else(|_| {
        format!(
            "{{\"id\":{},\"error\":{{\"code\":{code}}}}}",
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

fn console_notification(event: JsConsoleEvent, session_id: Option<&str>) -> String {
    notification(
        "Runtime.consoleAPICalled",
        json!({
            "type": event.kind,
            "args": event.args.iter().map(remote_object_from_console_arg).collect::<Vec<_>>(),
            "executionContextId": 1,
            "timestamp": now_ms(),
            "stackTrace": { "callFrames": [] },
        }),
        session_id,
    )
}

fn binding_notification(event: JsBindingEvent, session_id: Option<&str>) -> String {
    notification(
        "Runtime.bindingCalled",
        json!({
            "name": event.name,
            "payload": event.payload,
            "executionContextId": 1,
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
    target: &Target,
    frame_id: &str,
    timestamp: u64,
    session_id: Option<&str>,
) -> String {
    notification(
        "Network.requestWillBeSent",
        json!({
            "requestId": network_request_id(target),
            "loaderId": format!("loader-{}", target.loader_id),
            "documentURL": target.url.as_str(),
            "request": {
                "url": target.url.as_str(),
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
    target: &Target,
    frame_id: &str,
    timestamp: u64,
    session_id: Option<&str>,
) -> String {
    notification(
        "Network.responseReceived",
        json!({
            "requestId": network_request_id(target),
            "loaderId": format!("loader-{}", target.loader_id),
            "timestamp": timestamp,
            "type": "Document",
            "frameId": frame_id,
            "response": {
                "url": target.url.as_str(),
                "status": 200,
                "statusText": "OK",
                "headers": {},
                "mimeType": "text/html",
                "connectionReused": false,
                "connectionId": 0,
                "encodedDataLength": 0,
                "securityState": "neutral",
                "protocol": network_protocol_for_url(&target.url),
            },
            "hasExtraInfo": false,
        }),
        session_id,
    )
}

fn network_loading_finished_notification(
    target: &Target,
    timestamp: u64,
    session_id: Option<&str>,
) -> String {
    notification(
        "Network.loadingFinished",
        json!({
            "requestId": network_request_id(target),
            "timestamp": timestamp,
            "encodedDataLength": 0,
        }),
        session_id,
    )
}

fn network_fetch_notifications(
    event: JsNetworkEvent,
    frame_id: &str,
    loader_id: u64,
    document_url: &str,
    timestamp: u64,
    session_id: Option<&str>,
) -> Vec<String> {
    match event {
        JsNetworkEvent::Request {
            request_id,
            url,
            method,
        } => vec![network_fetch_request_will_be_sent_notification(
            &request_id,
            &url,
            &method,
            frame_id,
            loader_id,
            document_url,
            timestamp,
            session_id,
        )],
        JsNetworkEvent::Redirect {
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
        JsNetworkEvent::Response {
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
        JsNetworkEvent::Failure {
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

fn network_fetch_request_will_be_sent_notification(
    request_id: &str,
    url: &str,
    method: &str,
    frame_id: &str,
    loader_id: u64,
    document_url: &str,
    timestamp: u64,
    session_id: Option<&str>,
) -> String {
    notification(
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
    )
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

fn network_request_id(target: &Target) -> String {
    format!("request-{}", target.loader_id)
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

fn remote_object_from_console_arg(arg: &JsConsoleArg) -> Value {
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

fn console_value_json(value: &JsConsoleValue) -> Value {
    match value {
        JsConsoleValue::String(value) => json!(value),
        JsConsoleValue::Number(value) => json!(value),
        JsConsoleValue::Bool(value) => json!(value),
        JsConsoleValue::Null => Value::Null,
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

    fn dispatch_one(state: &mut CdpState, method: &str, params: Value) -> Value {
        let req = CdpRequest {
            id: 1,
            session_id: None,
            method: method.into(),
            params,
        };
        state.dispatch(&req).response.expect("success response")
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
        assert_eq!(v["targetInfos"].as_array().unwrap().len(), 1);

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
        assert!(s.targets.is_empty());
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
        assert_eq!(first["targetId"], "tab-1");
        assert_eq!(second["targetId"], "tab-3");

        let attached = dispatch_one(
            &mut s,
            "Target.attachToTarget",
            json!({ "targetId": second["targetId"].as_str().unwrap(), "flatten": true }),
        );
        let session_id = attached["sessionId"].as_str().unwrap();
        assert_eq!(session_id, "sess-5");

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
        assert_eq!(notification["params"]["sessionId"], "sess-5");
        assert_eq!(notification["params"]["targetId"], "tab-3");
        assert_eq!(s.targets.len(), 2, "detach must not close the target");
        assert!(
            s.attached_sessions.is_empty(),
            "detach drops only the routed session"
        );
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
            session_id: Some("sess-2".to_owned()),
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
        assert_eq!(request["sessionId"], "sess-2");
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
    fn capture_screenshot_validates_without_touching_gl() {
        let mut s = CdpState::default();
        let no_page = CdpRequest {
            id: 1,
            session_id: None,
            method: "Page.captureScreenshot".into(),
            params: json!({}),
        };
        let err = s
            .dispatch(&no_page)
            .response
            .expect_err("capture without page must fail");
        assert_eq!(err.code, -32000);

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
    fn input_mouse_event_validates_without_page() {
        let mut s = CdpState::default();
        let req = CdpRequest {
            id: 1,
            session_id: None,
            method: "Input.dispatchMouseEvent".into(),
            params: json!({ "type": "mousePressed", "x": 1, "y": 1, "button": "left" }),
        };
        let err = s
            .dispatch(&req)
            .response
            .expect_err("input without page must fail");
        assert_eq!(err.code, -32000);

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
    fn input_key_event_validates_without_page() {
        let mut s = CdpState::default();
        let req = CdpRequest {
            id: 1,
            session_id: None,
            method: "Input.dispatchKeyEvent".into(),
            params: json!({ "type": "keyDown", "key": "A", "text": "A" }),
        };
        let err = s
            .dispatch(&req)
            .response
            .expect_err("keyboard input without page must fail");
        assert_eq!(err.code, -32000);

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
    fn input_insert_text_validates_without_page() {
        let mut s = CdpState::default();
        let req = CdpRequest {
            id: 1,
            session_id: None,
            method: "Input.insertText".into(),
            params: json!({ "text": "A" }),
        };
        let err = s
            .dispatch(&req)
            .response
            .expect_err("insertText without page must fail");
        assert_eq!(err.code, -32000);

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
        assert_eq!(binding["params"]["executionContextId"], 1);
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
        let node_id = s.targets[0]
            .page
            .as_ref()
            .unwrap()
            .query_selector_all("#hit")
            .unwrap()[0]
            .node_id;

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

        let input_node_id = s.targets[0]
            .page
            .as_ref()
            .unwrap()
            .query_selector_all("#empty")
            .unwrap()[0]
            .node_id;
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
        assert!(s.dispatch(&isolated_world).response.is_ok());

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
                && notif["params"]["context"]["name"] == "playwright-utility"
        }));
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
}
