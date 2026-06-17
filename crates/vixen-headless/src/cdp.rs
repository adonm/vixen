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
use vixen_core::script::{JsRuntime, JsValue};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// CDP server entry point. Binds `127.0.0.1:{port}` and serves the
/// WebSocket CDP protocol until the process is killed.
///
/// **Single-threaded.** SpiderMonkey is `!Send + !Sync`, so the whole
/// server runs on one tokio `LocalSet`. Connections are handled serially:
/// CDP clients (Chrome DevTools, Puppeteer, Playwright) maintain a single
/// WebSocket per browser instance, so this is not a bottleneck in practice.
pub async fn serve(port: u16) -> std::io::Result<()> {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let listener = TcpListener::bind(addr).await?;
    // The state is `Rc<RefCell<>>` so it can move across `await` points in
    // a single-threaded `LocalSet` without going through `Arc<Mutex>`.
    let state: Rc<RefCell<CdpState>> = Rc::new(RefCell::new(CdpState::default()));
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

#[derive(Debug, Clone)]
#[allow(dead_code)] // Bookkeeping fields; required for future per-target routing.
struct Target {
    id: u64,
    session_id: u64,
    url: String,
    title: Option<String>,
    load_fired: bool,
}

impl CdpState {
    /// Dispatch a single JSON request (synchronous — no await while state
    /// is borrowed). Returns a list of outgoing lines: zero or more
    /// notifications followed by the response.
    pub fn handle_text_sync(&mut self, raw: &str) -> Vec<String> {
        let req: CdpRequest = match serde_json::from_str(raw) {
            Ok(r) => r,
            Err(e) => {
                return vec![error_response(None, -32700, &e.to_string())];
            }
        };
        let id = req.id;
        let (result, side_effects) = self.dispatch(&req);
        let mut out: Vec<String> = side_effects;
        let resp = CdpResponse {
            id,
            result,
            error: None,
        };
        match serde_json::to_string(&resp) {
            Ok(s) => out.push(s),
            Err(e) => out.push(error_response(Some(id), -32603, &e.to_string())),
        }
        out
    }

    /// Pure dispatch on the method name. Returns `(result_value, notifications)`.
    fn dispatch(&mut self, req: &CdpRequest) -> (Value, Vec<String>) {
        match req.method.as_str() {
            "Browser.getVersion" => (self.browser_get_version(), vec![]),
            "Target.createTarget" => self.target_create(req),
            "Target.attachToTarget" => self.target_attach(req),
            "Page.navigate" => self.page_navigate(req),
            "Page.loadEventFired" => (json!({}), vec![]),
            "Runtime.evaluate" => self.runtime_evaluate(req),
            _ => (
                Value::Null,
                vec![error_response(
                    Some(req.id),
                    -32601,
                    &format!("method not found: {}", req.method),
                )],
            ),
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

    fn target_create(&mut self, req: &CdpRequest) -> (Value, Vec<String>) {
        let url = req
            .params
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("about:blank")
            .to_owned();
        let id = self.next_target_id.fetch_add(1, Ordering::SeqCst) + 1;
        let session_id = self.next_target_id.fetch_add(1, Ordering::SeqCst) + 1;
        let target = Target {
            id,
            session_id,
            url,
            title: None,
            load_fired: false,
        };
        self.targets.push(target);
        (json!({ "targetId": format!("tab-{id}") }), vec![])
    }

    fn target_attach(&self, req: &CdpRequest) -> (Value, Vec<String>) {
        // CDP attaches to a target and returns a session. We mint a
        // deterministic id even if the requested targetId doesn't exist —
        // CDP clients treat attach as a fairly thin session bootstrap.
        let _ = req
            .params
            .get("targetId")
            .and_then(Value::as_str)
            .unwrap_or("");
        let session_id = self.next_target_id.fetch_add(1, Ordering::SeqCst) + 1;
        (json!({ "sessionId": format!("sess-{session_id}") }), vec![])
    }

    fn page_navigate(&mut self, req: &CdpRequest) -> (Value, Vec<String>) {
        let url = req
            .params
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if let Some(t) = self.targets.first_mut() {
            t.url = url.clone();
            t.load_fired = true;
        }
        // Notify: loadEventFired after navigate. This mirrors what real
        // browsers do — `Page.navigate` resolves, then `loadEventFired`
        // is delivered as a separate notification.
        let notif = serde_json::to_string(&CdpNotification {
            method: "Page.loadEventFired".into(),
            params: json!({ "timestamp": now_ms() }),
        })
        .unwrap_or_else(|_| "{}".into());
        (
            json!({ "frameId": "main", "loaderId": format!("loader-{}", now_ms()) }),
            vec![notif],
        )
    }

    fn runtime_evaluate(&mut self, req: &CdpRequest) -> (Value, Vec<String>) {
        let expr = match req.params.get("expression").and_then(Value::as_str) {
            Some(e) => e.to_owned(),
            None => {
                return (
                    Value::Null,
                    vec![error_response(
                        Some(req.id),
                        -32602,
                        "Runtime.evaluate: missing `expression`",
                    )],
                );
            }
        };
        // Lazily init the JS runtime per process. Errors at this point are
        // surfaced as CDP error responses (stable code, fail closed).
        if self.js.is_none() {
            match JsRuntime::new() {
                Ok(rt) => self.js = Some(rt),
                Err(e) => {
                    return (
                        Value::Null,
                        vec![error_response(
                            Some(req.id),
                            -32603,
                            &format!("SpiderMonkey init failed: {e}"),
                        )],
                    );
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
                (
                    json!({
                        "result": {
                            "type": type_str,
                            "value": value_json,
                            "description": value.to_display(),
                        }
                    }),
                    vec![],
                )
            }
            Err(e) => {
                let code = e.code();
                let msg = e.to_string();
                (
                    json!({
                        "result": { "type": "undefined" },
                        "exceptionDetails": {
                            "exceptionId": 1,
                            "text": msg,
                            "code": code,
                        }
                    }),
                    vec![],
                )
            }
        }
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
    // CDP has `result` OR `error`, never both. We always set `result` to a
    // JSON object (possibly `{}`), and let `error` be `None` on success.
    result: Value,
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
// failures carry (one of the codes from vixen_core::engine_error::codes).
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
        let (result, _) = state.dispatch(&req);
        result
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

        // Page.navigate — fires loadEventFired as a side-effect notification.
        let req = CdpRequest {
            id: 1,
            method: "Page.navigate".into(),
            params: json!({ "url": "https://example.test/" }),
        };
        let (result, side_effects) = s.dispatch(&req);
        assert_eq!(result["frameId"], "main");
        let notif: Value = serde_json::from_str(&side_effects[0]).expect("notification JSON");
        assert_eq!(notif["method"], "Page.loadEventFired");
        assert!(notif["params"]["timestamp"].as_u64().is_some());

        // Unknown method → JSON-RPC -32601.
        let req = CdpRequest {
            id: 99,
            method: "Foo.bar".into(),
            params: json!({}),
        };
        let (_, side_effects) = s.dispatch(&req);
        let parsed: Value = serde_json::from_str(&side_effects[0]).unwrap();
        assert_eq!(parsed["id"], 99);
        assert_eq!(parsed["error"]["code"], -32601);

        // Malformed JSON → JSON-RPC -32700.
        let out = s.handle_text_sync("not json");
        let parsed: Value = serde_json::from_str(&out[0]).unwrap();
        assert_eq!(parsed["error"]["code"], -32700);
    }
}
