//! `Runtime.evaluate` end-to-end against a real `JsRuntime`.
//!
//! Lives in its own integration-test binary because SpiderMonkey is a
//! process-singleton (one `JS_Init`/`JS_ShutDown` per process). The
//! `vixen-headless` lib test binary already exercises `JsRuntime` via
//! `eval_gate_returns_three`; running CDP's `Runtime.evaluate` there too
//! would conflict on init/shutdown. This file is its own binary, so it
//! owns its own `JsRuntime` lifecycle.

use serde_json::{Value, json};
use vixen_core::engine_error::codes;
use vixen_core::script::JsRuntime;
use vixen_headless::cdp::CdpState;

fn dispatch_one(state: &mut CdpState, method: &str, params: Value) -> Value {
    // Round-trip through JSON so the test exercises the actual wire path
    // (handle_text_sync), not just the dispatcher fn.
    let req = json!({
        "id": 1,
        "method": method,
        "params": params,
    });
    let lines = state.handle_text_sync(&req.to_string());
    // Last line is the response (notifications, if any, come first).
    let last = lines.last().expect("at least one response line");
    let v: Value = serde_json::from_str(last).unwrap();
    v["result"].clone()
}

/// All `Runtime.evaluate` checks in one test, sharing one `JsRuntime`.
/// (SpiderMonkey is a process-singleton — see file header.)
#[test]
fn runtime_evaluate_surface() {
    let rt = JsRuntime::new().expect("JS init");
    let mut s = CdpState::with_runtime(rt);

    // Scalar result.
    let v = dispatch_one(&mut s, "Runtime.evaluate", json!({ "expression": "1 + 2" }));
    assert_eq!(v["result"]["type"], "number");
    assert_eq!(v["result"]["value"], 3);
    assert_eq!(v["result"]["description"], "3");

    // String result.
    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "'hello ' + 'world'" }),
    );
    assert_eq!(v["result"]["type"], "string");
    assert_eq!(v["result"]["value"], "hello world");

    // Script error carries the stable code. (Phase 6 will surface the
    // actual exception message via `JS_GetPendingException`; until then
    // the text is the generic "script error: ..." from `EngineError`.)
    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "throw new Error('boom')" }),
    );
    let details = &v["exceptionDetails"];
    assert_eq!(
        details["code"],
        codes::SCRIPT_EVAL,
        "stable code: {details}",
    );
    assert!(
        details["text"].as_str().unwrap().contains("script"),
        "expected script-error text: {details}",
    );
}
