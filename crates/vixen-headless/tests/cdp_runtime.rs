//! `Runtime.evaluate` end-to-end against a real `JsRuntime`.
//!
//! Lives in its own integration-test binary so CDP's `Runtime.evaluate` owns a
//! focused runtime lifecycle independent of the CLI lib tests.

use serde_json::{Value, json};
use vixen_engine::engine_error::codes;
use vixen_engine::script::JsRuntime;
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
    // First line is the response; notifications, if any, follow it.
    let first = lines.first().expect("at least one response line");
    let v: Value = serde_json::from_str(first).unwrap();
    v["result"].clone()
}

/// All `Runtime.evaluate` checks in one test, sharing one `JsRuntime`.
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

    // Phase 6 pilot host constructors: CDP Runtime.evaluate reaches the real
    // JS global, whose Encoding API methods use vixen-engine's compatibility
    // surface.
    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "new TextEncoder().encode('é').length" }),
    );
    assert_eq!(v["result"]["type"], "number");
    assert_eq!(v["result"]["value"], 2);

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "new TextDecoder('UTF-8', { fatal: true }).fatal" }),
    );
    assert_eq!(v["result"]["type"], "boolean");
    assert_eq!(v["result"]["value"], true);

    // Phase 6 DOM host-object backbone: after navigation, Runtime.evaluate runs
    // against deno_core with a real `document` snapshot in the global.
    let dir = tempfile::tempdir().unwrap();
    let html = dir.path().join("cdp-dom-host.html");
    std::fs::write(
        &html,
        "<html><head><title>CDP DOM</title><style>#lead { color: blue; font-size: 20px !important; } p { margin-left: 4px; }</style><link id='theme' rel='stylesheet alternate'></head><body><p id='lead' class='note note callout' data-role='copy' data-author-name='ada' style='font-size: 18px; margin-left: 10px'>Hello <b>CDP</b></p><iframe id='frame' sandbox='allow-scripts allow-same-origin'></iframe></body></html>",
    )
    .unwrap();
    let url = format!("file://{}", html.display());
    let v = dispatch_one(&mut s, "Page.navigate", json!({ "url": url }));
    assert_eq!(v["frameId"], "main");

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.title" }),
    );
    assert_eq!(v["result"]["type"], "string");
    assert_eq!(v["result"]["value"], "CDP DOM");

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.querySelector('#lead').textContent" }),
    );
    assert_eq!(v["result"]["type"], "string");
    assert_eq!(v["result"]["value"], "Hello CDP");

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.querySelector('.callout').tagName" }),
    );
    assert_eq!(v["result"]["type"], "string");
    assert_eq!(v["result"]["value"], "P");

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.querySelector('#lead').matches('.note')" }),
    );
    assert_eq!(v["result"]["type"], "boolean");
    assert_eq!(v["result"]["value"], true);

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.getElementById('lead').getAttribute('data-role')" }),
    );
    assert_eq!(v["result"]["type"], "string");
    assert_eq!(v["result"]["value"], "copy");

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.querySelector('#lead').classList.length" }),
    );
    assert_eq!(v["result"]["type"], "number");
    assert_eq!(v["result"]["value"], 2);

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.querySelector('#lead').classList.contains('callout')" }),
    );
    assert_eq!(v["result"]["type"], "boolean");
    assert_eq!(v["result"]["value"], true);

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.querySelector('#theme').relList.contains('alternate')" }),
    );
    assert_eq!(v["result"]["type"], "boolean");
    assert_eq!(v["result"]["value"], true);

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.querySelector('#frame').sandbox.item(1)" }),
    );
    assert_eq!(v["result"]["type"], "string");
    assert_eq!(v["result"]["value"], "allow-same-origin");

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.querySelector('#lead').dataset.authorName" }),
    );
    assert_eq!(v["result"]["type"], "string");
    assert_eq!(v["result"]["value"], "ada");

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "CSS.supports('display', 'grid')" }),
    );
    assert_eq!(v["result"]["type"], "boolean");
    assert_eq!(v["result"]["value"], true);

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "getComputedStyle(document.querySelector('#lead')).color" }),
    );
    assert_eq!(v["result"]["type"], "string");
    assert_eq!(v["result"]["value"], "blue");

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "window.getComputedStyle(document.querySelector('#lead')).getPropertyValue('margin-left')" }),
    );
    assert_eq!(v["result"]["type"], "string");
    assert_eq!(v["result"]["value"], "10px");

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.styleSheets[0].cssRules[0].selectorText" }),
    );
    assert_eq!(v["result"]["type"], "string");
    assert_eq!(v["result"]["value"], "#lead");

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.styleSheets[0].cssRules[0].style.length" }),
    );
    assert_eq!(v["result"]["type"], "number");
    assert_eq!(v["result"]["value"], 2);

    // Script error carries the stable code. The text is the generic
    // "script error: ..." from `EngineError`.
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
