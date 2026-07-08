//! Chrome DevTools Protocol (CDP) server — Phase 8 step 1 (docs/PLAN.md).
//!
//! Implements the v1.0-required CDP method surface (docs/SPEC.md "CDP
//! methods required") over a tokio + tokio-tungstenite WebSocket server:
//!
//! - `Browser.getVersion`
//! - `Target.createTarget`, `Target.attachToTarget`, `Target.getTargets`
//! - `Page.enable`, `Page.navigate`, `Page.loadEventFired`
//! - `Page.captureScreenshot`
//! - `Runtime.enable`, `Runtime.evaluate`, `Runtime.consoleAPICalled`
//! - `Input.dispatchMouseEvent`
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
use vixen_engine::engine_error::codes;
use vixen_engine::page::Page;
use vixen_engine::script::{JsConsoleArg, JsConsoleEvent, JsConsoleValue, JsRuntime, JsValue};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const DEFAULT_CAPTURE_VIEWPORT: (u32, u32) = (800, 600);

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
    js: Option<JsRuntime>,
    runtime_enabled: bool,
    page_enabled: bool,
    log_enabled: bool,
    last_mouse_down: Option<MouseDownTarget>,
}

#[allow(dead_code)] // Bookkeeping fields; required for future per-target routing.
struct Target {
    id: u64,
    session_id: u64,
    url: String,
    title: Option<String>,
    load_fired: bool,
    page: Option<Page>,
}

#[derive(Debug, Clone)]
struct MouseDownTarget {
    node_id: usize,
    button: MouseButton,
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
        let mut out = Vec::with_capacity(1 + outcome.notifications.len());
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
            "Browser.close" | "Browser.setDownloadBehavior" => CdpDispatch::ok(json!({})),
            "Target.createTarget" => self.target_create(req),
            "Target.attachToTarget" => self.target_attach(req),
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
            "Page.captureScreenshot" => self.page_capture_screenshot(req),
            "Page.getFrameTree" => CdpDispatch::ok(self.page_get_frame_tree()),
            "Page.setLifecycleEventsEnabled" | "Page.bringToFront" => CdpDispatch::ok(json!({})),
            "Runtime.enable" => self.runtime_enable(req),
            "Runtime.disable" => {
                self.runtime_enabled = false;
                CdpDispatch::ok(json!({}))
            }
            "Runtime.evaluate" => self.runtime_evaluate(req),
            "Runtime.releaseObjectGroup" | "Runtime.runIfWaitingForDebugger" => {
                CdpDispatch::ok(json!({}))
            }
            "Log.enable" => {
                self.log_enabled = true;
                CdpDispatch::ok(json!({}))
            }
            "Log.disable" => {
                self.log_enabled = false;
                CdpDispatch::ok(json!({}))
            }
            "Network.enable"
            | "Network.disable"
            | "DOM.enable"
            | "DOM.disable"
            | "Emulation.setDeviceMetricsOverride"
            | "Emulation.clearDeviceMetricsOverride"
            | "Emulation.setTouchEmulationEnabled"
            | "Emulation.setFocusEmulationEnabled" => CdpDispatch::ok(json!({})),
            "Input.dispatchMouseEvent" => self.input_dispatch_mouse_event(req),
            _ => CdpDispatch::error(-32601, format!("method not found: {}", req.method)),
        }
    }

    // --- Method handlers ------------------------------------------------

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

    fn target_create(&mut self, req: &CdpRequest) -> CdpDispatch {
        let url = req
            .params
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("about:blank")
            .to_owned();
        match self.push_loaded_target(url) {
            Ok(id) => CdpDispatch::ok(json!({ "targetId": format!("tab-{id}") })),
            Err(e) => CdpDispatch::error(-32602, e),
        }
    }

    fn target_attach(&self, req: &CdpRequest) -> CdpDispatch {
        // CDP attaches to a target and returns a session. We mint a
        // deterministic id even if the requested targetId doesn't exist —
        // CDP clients treat attach as a fairly thin session bootstrap.
        let _ = req
            .params
            .get("targetId")
            .and_then(Value::as_str)
            .unwrap_or("");
        let session_id = self.next_target_id.fetch_add(1, Ordering::SeqCst) + 1;
        CdpDispatch::ok(json!({ "sessionId": format!("sess-{session_id}") }))
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
            .map(|target| {
                notification(
                    "Target.attachedToTarget",
                    json!({
                        "sessionId": format!("sess-{}", target.session_id),
                        "targetInfo": target_info_json(target),
                        "waitingForDebugger": false,
                    }),
                    req.session_id.as_deref(),
                )
            })
            .collect();
        CdpDispatch::ok_with_notifications(json!({}), notifications)
    }

    fn page_enable(&mut self, req: &CdpRequest) -> CdpDispatch {
        self.page_enabled = true;
        let Some(target) = self.targets.first() else {
            return CdpDispatch::ok(json!({}));
        };
        CdpDispatch::ok_with_notifications(
            json!({}),
            vec![
                notification(
                    "Page.frameStartedLoading",
                    json!({ "frameId": "main" }),
                    req.session_id.as_deref(),
                ),
                notification(
                    "Page.frameStoppedLoading",
                    json!({ "frameId": "main" }),
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

    fn page_navigate(&mut self, req: &CdpRequest) -> CdpDispatch {
        let Some(url) = req.params.get("url").and_then(Value::as_str) else {
            return CdpDispatch::error(-32602, "Page.navigate: missing `url`");
        };
        let url = url.to_owned();
        let page = match load_cdp_page(&url) {
            Ok(page) => page,
            Err(e) => return CdpDispatch::error(-32602, e),
        };
        self.reset_js_for_navigation();
        if let Err(e) = self.execute_page_scripts_if_needed(&page) {
            return CdpDispatch::error(-32603, e);
        }
        let mut notifications = self.drain_console_notifications(req);
        let title = page.document().title();
        if let Some(t) = self.targets.first_mut() {
            t.url = url.clone();
            t.title = title;
            t.load_fired = true;
            t.page = Some(page);
        } else {
            let id = self.next_target_id.fetch_add(1, Ordering::SeqCst) + 1;
            let session_id = self.next_target_id.fetch_add(1, Ordering::SeqCst) + 1;
            self.targets.push(Target {
                id,
                session_id,
                url: url.clone(),
                title,
                load_fired: true,
                page: Some(page),
            });
        }
        // Notify: loadEventFired after navigate. This mirrors what real
        // browsers do — `Page.navigate` resolves, then `loadEventFired`
        // is delivered as a separate notification.
        notifications.push(notification(
            "Page.loadEventFired",
            json!({ "timestamp": now_ms() }),
            req.session_id.as_deref(),
        ));
        CdpDispatch::ok_with_notifications(
            json!({ "frameId": "main", "loaderId": format!("loader-{}", now_ms()) }),
            notifications,
        )
    }

    fn page_capture_screenshot(&self, req: &CdpRequest) -> CdpDispatch {
        if let Some(format) = req.params.get("format").and_then(Value::as_str)
            && !format.eq_ignore_ascii_case("png")
        {
            return CdpDispatch::error(-32602, "Page.captureScreenshot: only png is supported");
        }
        let viewport = match capture_viewport(&req.params) {
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

    fn page_get_frame_tree(&self) -> Value {
        let frame = self.targets.first().map(frame_json).unwrap_or_else(|| {
            json!({
                "id": "main",
                "loaderId": "loader-0",
                "url": "about:blank",
                "securityOrigin": "://",
                "mimeType": "text/html",
            })
        });
        json!({ "frameTree": { "frame": frame } })
    }

    fn runtime_enable(&mut self, req: &CdpRequest) -> CdpDispatch {
        self.runtime_enabled = true;
        CdpDispatch::ok_with_notifications(
            json!({}),
            vec![notification(
                "Runtime.executionContextCreated",
                json!({
                    "context": {
                        "id": 1,
                        "origin": self.current_origin(),
                        "name": "Vixen",
                        "uniqueId": "vixen-main-context",
                        "auxData": {
                            "isDefault": true,
                            "type": "default",
                            "frameId": "main",
                        }
                    }
                }),
                req.session_id.as_deref(),
            )],
        )
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

        let Some(page) = self.targets.first().and_then(|target| target.page.as_ref()) else {
            return CdpDispatch::error(-32000, "Input.dispatchMouseEvent: no page loaded");
        };
        let target = page.element_at(DEFAULT_CAPTURE_VIEWPORT, x, y);
        let mut dom_events = Vec::new();
        let mut next_mouse_down = self.last_mouse_down.clone();

        match event_type {
            "mouseMoved" => {
                if let Some(target) = target.as_ref() {
                    dom_events.push((target.node_id, "mousemove"));
                }
            }
            "mousePressed" => {
                if let Some(target) = target.as_ref() {
                    dom_events.push((target.node_id, "mousedown"));
                    next_mouse_down = Some(MouseDownTarget {
                        node_id: target.node_id,
                        button,
                    });
                } else {
                    next_mouse_down = None;
                }
            }
            "mouseReleased" => {
                if let Some(target) = target.as_ref() {
                    dom_events.push((target.node_id, "mouseup"));
                    if self.last_mouse_down.as_ref().is_some_and(|down| {
                        down.node_id == target.node_id
                            && down.button == MouseButton::Left
                            && button == MouseButton::Left
                    }) {
                        dom_events.push((target.node_id, "click"));
                    }
                }
                next_mouse_down = None;
            }
            _ => {
                return CdpDispatch::error(
                    -32602,
                    "Input.dispatchMouseEvent: unsupported mouse event type",
                );
            }
        }

        for (node_id, dom_type) in dom_events {
            if let Err(err) = dispatch_dom_mouse_event(
                &mut self.js,
                page,
                node_id,
                dom_type,
                MouseDispatchInit {
                    x,
                    y,
                    button,
                    buttons,
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
                },
            ) {
                return CdpDispatch::error(-32603, err);
            }
        }
        self.last_mouse_down = next_mouse_down;
        CdpDispatch::ok_with_notifications(json!({}), self.drain_console_notifications(req))
    }

    fn runtime_evaluate(&mut self, req: &CdpRequest) -> CdpDispatch {
        let expr = match req.params.get("expression").and_then(Value::as_str) {
            Some(e) => e.to_owned(),
            None => {
                return CdpDispatch::error(-32602, "Runtime.evaluate: missing `expression`");
            }
        };
        if crate::looks_like_dom_eval(&expr)
            && !crate::uses_runtime_dom_eval(&expr)
            && let Some(page) = self.targets.first().and_then(|target| target.page.as_ref())
            && let Some(result) = page.evaluate_dom_expression(&expr)
        {
            return match result {
                Ok(value) => CdpDispatch::ok(remote_object_from_text(value)),
                Err(e) => CdpDispatch::ok(json!({
                    "result": { "type": "undefined" },
                    "exceptionDetails": {
                        "exceptionId": 1,
                        "text": e,
                        "code": "dom.eval",
                    }
                })),
            };
        }
        // Lazily init the JS runtime per process. Errors at this point are
        // surfaced as CDP error responses (stable code, fail closed).
        if self.js.is_none() {
            match JsRuntime::new() {
                Ok(rt) => self.js = Some(rt),
                Err(e) => {
                    return CdpDispatch::error(-32603, format!("JS runtime init failed: {e}"));
                }
            }
        }
        let page = self.targets.first().and_then(|target| target.page.as_ref());
        let rt = self.js.as_mut().expect("just-initialised");
        let result = if let Some(page) = page {
            rt.evaluate_with_page(&expr, page)
        } else {
            rt.evaluate(&expr)
        };
        match result {
            Ok(value) => {
                let type_str = match &value {
                    JsValue::Int32(_) | JsValue::Number(_) => "number",
                    JsValue::String(_) => "string",
                    JsValue::Bool(_) => "boolean",
                    JsValue::Null => "object",
                    JsValue::Undefined => "undefined",
                    JsValue::Object => "object",
                };
                let value_json = match &value {
                    JsValue::Int32(n) => json!(n),
                    JsValue::Number(n) => json!(n),
                    JsValue::String(s) => json!(s),
                    JsValue::Bool(b) => json!(b),
                    JsValue::Null => Value::Null,
                    JsValue::Undefined => Value::Null,
                    JsValue::Object => json!({}),
                };
                CdpDispatch::ok_with_notifications(
                    json!({
                        "result": {
                            "type": type_str,
                            "value": value_json,
                            "description": value.to_display(),
                        }
                    }),
                    self.drain_console_notifications(req),
                )
            }
            Err(e) => {
                let code = e.code();
                let msg = e.to_string();
                let mut notifications = self.drain_console_notifications(req);
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
        }
    }

    fn seed_initial_target(&mut self, url: String) -> Result<(), String> {
        self.push_loaded_target(url).map(|_| ())
    }

    fn push_loaded_target(&mut self, url: String) -> Result<u64, String> {
        let page = load_cdp_page(&url)?;
        self.execute_page_scripts_if_needed(&page)?;
        self.discard_console_events();
        let title = page.document().title();
        let id = self.next_target_id.fetch_add(1, Ordering::SeqCst) + 1;
        let session_id = self.next_target_id.fetch_add(1, Ordering::SeqCst) + 1;
        self.targets.push(Target {
            id,
            session_id,
            url,
            title,
            load_fired: false,
            page: Some(page),
        });
        Ok(id)
    }

    fn execute_page_scripts_if_needed(&mut self, page: &Page) -> Result<(), String> {
        if !page.has_classic_scripts() {
            return Ok(());
        }
        if self.js.is_none() {
            let rt = JsRuntime::new().map_err(|e| format!("JS runtime init failed: {e}"))?;
            self.js = Some(rt);
        }
        self.js
            .as_mut()
            .expect("runtime just initialised")
            .execute_page_scripts(page)
            .map(|_| ())
            .map_err(|e| format!("page script failed: {e}"))
    }

    fn current_origin(&self) -> String {
        self.targets
            .first()
            .map(|target| origin_for_url(&target.url))
            .unwrap_or_else(|| "://".to_owned())
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

    fn discard_console_events(&mut self) {
        if let Some(js) = self.js.as_mut() {
            let _ = js.drain_console_events();
        }
    }

    fn reset_js_for_navigation(&mut self) {
        if let Some(js) = self.js.as_mut() {
            js.reset_realm();
        }
        self.last_mouse_down = None;
    }
}

impl CdpState {
    /// Construct a state pre-seeded with a JS runtime (used by tests so they
    /// don't pay JS runtime init cost on every call).
    pub fn with_runtime(rt: JsRuntime) -> Self {
        Self {
            next_target_id: AtomicU64::new(0),
            targets: Vec::new(),
            js: Some(rt),
            runtime_enabled: false,
            page_enabled: false,
            log_enabled: false,
            last_mouse_down: None,
        }
    }
}

struct CdpDispatch {
    response: Result<Value, CdpError>,
    notifications: Vec<String>,
}

impl CdpDispatch {
    fn ok(result: Value) -> Self {
        Self {
            response: Ok(result),
            notifications: Vec::new(),
        }
    }

    fn ok_with_notifications(result: Value, notifications: Vec<String>) -> Self {
        Self {
            response: Ok(result),
            notifications,
        }
    }

    fn error(code: i32, message: impl Into<String>) -> Self {
        Self {
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

fn frame_json(target: &Target) -> Value {
    json!({
        "id": "main",
        "loaderId": format!("loader-{}", target.id),
        "url": target.url,
        "securityOrigin": origin_for_url(&target.url),
        "mimeType": "text/html",
    })
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

struct MouseDispatchInit {
    x: f64,
    y: f64,
    button: MouseButton,
    buttons: i64,
    ctrl_key: bool,
    shift_key: bool,
    alt_key: bool,
    meta_key: bool,
}

fn dispatch_dom_mouse_event(
    js: &mut Option<JsRuntime>,
    page: &Page,
    node_id: usize,
    event_type: &str,
    init: MouseDispatchInit,
) -> Result<(), String> {
    if js.is_none() {
        *js = Some(JsRuntime::new().map_err(|e| format!("JS runtime init failed: {e}"))?);
    }
    let event_type = serde_json::to_string(event_type).map_err(|e| e.to_string())?;
    let init = json!({
        "clientX": init.x,
        "clientY": init.y,
        "screenX": init.x,
        "screenY": init.y,
        "button": init.button.dom_button_code(),
        "buttons": init.buttons,
        "ctrlKey": init.ctrl_key,
        "shiftKey": init.shift_key,
        "altKey": init.alt_key,
        "metaKey": init.meta_key,
    });
    let init = serde_json::to_string(&init).map_err(|e| e.to_string())?;
    let src = format!(
        "globalThis.__vixenDispatchMouseEvent ? globalThis.__vixenDispatchMouseEvent({node_id}, {event_type}, {init}) : false"
    );
    js.as_mut()
        .expect("runtime just initialised")
        .evaluate_with_page(&src, page)
        .map(|_| ())
        .map_err(|e| format!("Input.dispatchMouseEvent: {e}"))
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

fn capture_viewport(params: &Value) -> Result<(u32, u32), String> {
    let Some(clip) = params.get("clip") else {
        return Ok(DEFAULT_CAPTURE_VIEWPORT);
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
        assert!(v["sessionId"].as_str().unwrap().starts_with("sess-"));

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

        // Page.navigate — returns success and queues loadEventFired.
        let req = CdpRequest {
            id: 1,
            session_id: None,
            method: "Page.navigate".into(),
            params: json!({ "url": "about:blank" }),
        };
        let outcome = s.dispatch(&req);
        let result = outcome.response.expect("navigate response");
        assert_eq!(result["frameId"], "main");
        let notif: Value =
            serde_json::from_str(&outcome.notifications[0]).expect("notification JSON");
        assert_eq!(notif["method"], "Page.loadEventFired");
        assert!(notif["params"]["timestamp"].as_u64().is_some());

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
        assert_eq!(capture_viewport(&json!({})).unwrap(), (800, 600));
        assert_eq!(
            capture_viewport(
                &json!({ "clip": { "x": 0, "y": 0, "width": 160, "height": 120, "scale": 1 } })
            )
            .unwrap(),
            (160, 120)
        );
        assert!(
            capture_viewport(
                &json!({ "clip": { "x": 10, "y": 0, "width": 160, "height": 120, "scale": 1 } })
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

        assert_eq!(out.len(), 2);
        let response: Value = serde_json::from_str(&out[0]).unwrap();
        let notification: Value = serde_json::from_str(&out[1]).unwrap();
        assert_eq!(response["id"], 7);
        assert_eq!(response["result"]["frameId"], "main");
        assert_eq!(notification["method"], "Page.loadEventFired");
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
        assert_eq!(response["result"]["frameId"], "main");

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
