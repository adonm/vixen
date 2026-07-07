//! Chrome DevTools Protocol (CDP) server — Phase 8 step 1 (docs/PLAN.md).
//!
//! Implements the v1.0-required CDP method surface (docs/SPEC.md "CDP
//! methods required") over a tokio + tokio-tungstenite WebSocket server:
//!
//! - `Browser.getVersion`
//! - `Target.createTarget`, `Target.attachToTarget`
//! - `Page.navigate`, `Page.loadEventFired`
//! - `Runtime.evaluate`
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

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use vixen_engine::page::Page;
use vixen_engine::script::{JsRuntime, JsValue};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// CDP server entry point. Binds `127.0.0.1:{port}` and serves the
/// WebSocket CDP protocol until the process is killed.
///
/// **Single-threaded.** SpiderMonkey is `!Send + !Sync`, so the whole
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
/// `Runtime.evaluate`. Lives behind `Rc<RefCell<>>` because SpiderMonkey is
/// `!Send + !Sync`; the whole server runs on a single `LocalSet`.
#[derive(Default)]
pub struct CdpState {
    next_target_id: AtomicU64,
    targets: Vec<Target>,
    js: Option<JsRuntime>,
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
                result: Some(result),
                error: None,
            },
            Err(error) => CdpResponse {
                id,
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
            "Target.createTarget" => self.target_create(req),
            "Target.attachToTarget" => self.target_attach(req),
            "Page.navigate" => self.page_navigate(req),
            "Page.loadEventFired" => CdpDispatch::ok(json!({})),
            "Runtime.evaluate" => self.runtime_evaluate(req),
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
            "jsVersion": "SpiderMonkey (mozjs)"
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

    fn page_navigate(&mut self, req: &CdpRequest) -> CdpDispatch {
        let Some(url) = req.params.get("url").and_then(Value::as_str) else {
            return CdpDispatch::error(-32602, "Page.navigate: missing `url`");
        };
        let url = url.to_owned();
        let page = match load_cdp_page(&url) {
            Ok(page) => page,
            Err(e) => return CdpDispatch::error(-32602, e),
        };
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
        let notif = serde_json::to_string(&CdpNotification {
            method: "Page.loadEventFired".into(),
            params: json!({ "timestamp": now_ms() }),
        })
        .unwrap_or_else(|_| "{}".into());
        CdpDispatch::ok_with_notifications(
            json!({ "frameId": "main", "loaderId": format!("loader-{}", now_ms()) }),
            vec![notif],
        )
    }

    fn runtime_evaluate(&mut self, req: &CdpRequest) -> CdpDispatch {
        let expr = match req.params.get("expression").and_then(Value::as_str) {
            Some(e) => e.to_owned(),
            None => {
                return CdpDispatch::error(-32602, "Runtime.evaluate: missing `expression`");
            }
        };
        if crate::looks_like_dom_eval(&expr)
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
                    return CdpDispatch::error(-32603, format!("SpiderMonkey init failed: {e}"));
                }
            }
        }
        let rt = self.js.as_mut().expect("just-initialised");
        match rt.evaluate(&expr) {
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
                CdpDispatch::ok(json!({
                    "result": {
                        "type": type_str,
                        "value": value_json,
                        "description": value.to_display(),
                    }
                }))
            }
            Err(e) => {
                let code = e.code();
                let msg = e.to_string();
                CdpDispatch::ok(json!({
                    "result": { "type": "undefined" },
                    "exceptionDetails": {
                        "exceptionId": 1,
                        "text": msg,
                        "code": code,
                    }
                }))
            }
        }
    }

    fn seed_initial_target(&mut self, url: String) -> Result<(), String> {
        self.push_loaded_target(url).map(|_| ())
    }

    fn push_loaded_target(&mut self, url: String) -> Result<u64, String> {
        let page = load_cdp_page(&url)?;
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
}

impl CdpState {
    /// Construct a state pre-seeded with a JS runtime (used by tests so they
    /// don't pay SpiderMonkey init cost on every call).
    pub fn with_runtime(rt: JsRuntime) -> Self {
        Self {
            next_target_id: AtomicU64::new(0),
            targets: Vec::new(),
            js: Some(rt),
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
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct CdpResponse {
    id: u64,
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
            method: method.into(),
            params,
        };
        state.dispatch(&req).response.expect("success response")
    }

    /// All CDP dispatcher checks (except `Runtime.evaluate`, which lives in
    /// `tests/cdp_runtime.rs` to avoid the SpiderMonkey process-singleton
    /// conflict with the `eval_gate_returns_three` test in this binary).
    #[test]
    fn cdp_dispatcher_surface() {
        // No JsRuntime: the dispatcher lazily inits one when first needed,
        // and none of these methods need it.
        let mut s = CdpState::default();

        // Browser.getVersion — stable product string.
        let v = dispatch_one(&mut s, "Browser.getVersion", json!({}));
        assert_eq!(v["protocolVersion"], "1.3");
        assert!(v["product"].as_str().unwrap().starts_with("Vixen/"));
        assert_eq!(v["jsVersion"], "SpiderMonkey (mozjs)");

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

        // Page.navigate — returns success and queues loadEventFired.
        let req = CdpRequest {
            id: 1,
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
            method: "Foo.bar".into(),
            params: json!({}),
        };
        let err = s.dispatch(&req).response.expect_err("unknown method error");
        assert_eq!(err.code, -32601);

        // Malformed JSON → JSON-RPC -32700.
        let out = s.handle_text_sync("not json");
        let parsed: Value = serde_json::from_str(&out[0]).unwrap();
        assert_eq!(parsed["error"]["code"], -32700);
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
    fn runtime_evaluate_can_read_navigated_page_dom() {
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
                "params": { "expression": "document.title" }
            })
            .to_string(),
        );
        let response: Value = serde_json::from_str(&eval[0]).unwrap();
        assert_eq!(response["result"]["result"]["type"], "string");
        assert_eq!(response["result"]["result"]["value"], "CDP title");
    }
}
