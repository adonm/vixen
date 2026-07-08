//! `Runtime.evaluate` end-to-end against a real `JsRuntime`.
//!
//! Lives in its own integration-test binary so CDP's `Runtime.evaluate` owns a
//! focused runtime lifecycle independent of the CLI lib tests.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
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

fn dispatch_lines(state: &mut CdpState, method: &str, params: Value) -> Vec<Value> {
    let req = json!({
        "id": 1,
        "method": method,
        "params": params,
    });
    state
        .handle_text_sync(&req.to_string())
        .into_iter()
        .map(|line| serde_json::from_str(&line).unwrap())
        .collect()
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
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nX-CDP-Test: yes\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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

/// All `Runtime.evaluate` checks in one test, sharing one `JsRuntime`.
#[test]
fn runtime_evaluate_surface() {
    let (fetch_url, fetch_config, fetch_server) =
        spawn_fetch_server("vixen-cdp-fetch.com", "cdp fetch");
    let rt = JsRuntime::with_network_config(fetch_config).expect("JS init");
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

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "globalThis.__cdpPersist = 41" }),
    );
    assert_eq!(v["result"]["type"], "number");
    assert_eq!(v["result"]["value"], 41);

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "__cdpPersist + 1" }),
    );
    assert_eq!(v["result"]["type"], "number");
    assert_eq!(v["result"]["value"], 42);

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

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "Window.prototype instanceof EventTarget && typeof HTMLCanvasElement === 'function' && typeof GPUDevice === 'function'" }),
    );
    assert_eq!(v["result"]["type"], "boolean");
    assert_eq!(v["result"]["value"], true);

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "new EventTarget().dispatchEvent(new Event('ready'))" }),
    );
    assert_eq!(v["result"]["type"], "boolean");
    assert_eq!(v["result"]["value"], true);

    let lines = dispatch_lines(&mut s, "Runtime.enable", json!({}));
    assert_eq!(lines[0]["result"], json!({}));
    assert_eq!(lines[1]["method"], "Runtime.executionContextCreated");

    let lines = dispatch_lines(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "console.log('cdp console', 7); 'ok'" }),
    );
    assert_eq!(lines[0]["result"]["result"]["value"], "ok");
    assert_eq!(lines[1]["method"], "Runtime.consoleAPICalled");
    assert_eq!(lines[1]["params"]["type"], "log");
    assert_eq!(lines[1]["params"]["args"][0]["value"], "cdp console");
    assert_eq!(lines[1]["params"]["args"][1]["value"], 7.0);

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "new DOMMatrix().translate(10, 20).transformPoint(new DOMPoint(1, 2)).y" }),
    );
    assert_eq!(v["result"]["type"], "number");
    assert_eq!(v["result"]["value"], 22);

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "new Headers([['X-Test', 'a'], ['X-Test', 'b']]).get('x-test')" }),
    );
    assert_eq!(v["result"]["type"], "string");
    assert_eq!(v["result"]["value"], "a, b");

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "(() => { localStorage.setItem('mode', 'dark'); return localStorage.getItem('mode') + ':' + localStorage.length + ':' + localStorage.key(0); })()" }),
    );
    assert_eq!(v["result"]["type"], "string");
    assert_eq!(v["result"]["value"], "dark:1:mode");

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "localStorage.getItem('mode')" }),
    );
    assert_eq!(v["result"]["type"], "string");
    assert_eq!(v["result"]["value"], "dark");

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": format!("fetch({fetch_url:?}).then((response) => response.text().then((body) => response.status + ':' + response.headers.get('x-cdp-test') + ':' + body))") }),
    );
    assert_eq!(v["result"]["type"], "string");
    assert_eq!(v["result"]["value"], "200:yes:cdp fetch");
    fetch_server.join().unwrap();

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "fetch('http://127.0.0.1:9/').then(() => false, (err) => err instanceof TypeError && /blocked host/.test(err.message))" }),
    );
    assert_eq!(v["result"]["type"], "boolean");
    assert_eq!(v["result"]["value"], true);

    // Phase 6 DOM host-object backbone: after navigation, Runtime.evaluate runs
    // against deno_core with a real `document` snapshot in the global.
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("cdp-external.js");
    std::fs::write(
        &script,
        "globalThis.__cdpExternal = globalThis.__cdpInline + 1; localStorage.setItem('cdp-external', 'ran');",
    )
    .unwrap();
    let html = dir.path().join("cdp-dom-host.html");
    std::fs::write(
        &html,
        "<html><head><title>CDP DOM</title><style>body { margin: 0; } #hit { width: 80px; height: 30px; display: block; } #lead { color: blue; font-size: 20px !important; } p { margin-left: 4px; } #box { width: 40px; height: 20px; }</style><link id='theme' rel='stylesheet alternate'><script>globalThis.__cdpInline = 40; localStorage.setItem('cdp-inline', 'ran');</script><script>globalThis.__cdpInline += 2;</script><script src='cdp-external.js'></script></head><body><button id='hit'>Hit</button><script>document.querySelector('#hit').addEventListener('click', () => { globalThis.__cdpClicked = (globalThis.__cdpClicked || 0) + 1; console.log('clicked', document.querySelector('#hit').id); });</script><p id='lead' class='note note callout' data-role='copy' data-author-name='ada' style='font-size: 18px; margin-left: 10px'>Hello <b>CDP</b></p><div id='box'>Box</div><form id='contact'><input name='name' value='Ada'></form><iframe id='frame' sandbox='allow-scripts allow-same-origin'></iframe></body></html>",
    )
    .unwrap();
    let url = format!("file://{}", html.display());
    let v = dispatch_one(&mut s, "Page.navigate", json!({ "url": url }));
    assert_eq!(v["frameId"], "main");

    let v = dispatch_one(
        &mut s,
        "Page.captureScreenshot",
        json!({ "format": "png", "clip": { "x": 0, "y": 0, "width": 160, "height": 120, "scale": 1 } }),
    );
    let png = BASE64_STANDARD
        .decode(v["data"].as_str().expect("base64 screenshot"))
        .expect("valid base64 png");
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));

    let lines = dispatch_lines(
        &mut s,
        "Input.dispatchMouseEvent",
        json!({ "type": "mousePressed", "x": 10, "y": 10, "button": "left", "buttons": 1 }),
    );
    assert_eq!(lines[0]["result"], json!({}));
    let lines = dispatch_lines(
        &mut s,
        "Input.dispatchMouseEvent",
        json!({ "type": "mouseReleased", "x": 10, "y": 10, "button": "left", "buttons": 0 }),
    );
    assert_eq!(lines[0]["result"], json!({}));
    assert_eq!(lines[1]["method"], "Runtime.consoleAPICalled");
    assert_eq!(lines[1]["params"]["args"][0]["value"], "clicked");
    assert_eq!(lines[1]["params"]["args"][1]["value"], "hit");

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "__cdpClicked" }),
    );
    assert_eq!(v["result"]["type"], "number");
    assert_eq!(v["result"]["value"], 1);

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "__cdpInline + ':' + __cdpExternal + ':' + localStorage.getItem('cdp-inline') + ':' + localStorage.getItem('cdp-external')" }),
    );
    assert_eq!(v["result"]["type"], "string");
    assert_eq!(v["result"]["value"], "42:43:ran:ran");

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
        json!({ "expression": "HTMLElement.prototype instanceof Element && XMLDocument.prototype instanceof Document" }),
    );
    assert_eq!(v["result"]["type"], "boolean");
    assert_eq!(v["result"]["value"], true);

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
        json!({ "expression": "document.styleSheets[0].cssRules[2].selectorText" }),
    );
    assert_eq!(v["result"]["type"], "string");
    assert_eq!(v["result"]["value"], "#lead");

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.styleSheets[0].cssRules[2].style.length" }),
    );
    assert_eq!(v["result"]["type"], "number");
    assert_eq!(v["result"]["value"], 2);

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.querySelector('#box').getBoundingClientRect().width" }),
    );
    assert_eq!(v["result"]["type"], "number");
    assert_eq!(v["result"]["value"], 40);

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.querySelector('#box').getBoundingClientRect().right" }),
    );
    assert_eq!(v["result"]["type"], "number");
    assert_eq!(v["result"]["value"], 40);

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.querySelector('#box').getClientRects().length" }),
    );
    assert_eq!(v["result"]["type"], "number");
    assert_eq!(v["result"]["value"], 1);

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "new FormData(document.getElementById('contact')).get('name')" }),
    );
    assert_eq!(v["result"]["type"], "string");
    assert_eq!(v["result"]["value"], "Ada");

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.createRange().collapsed && window.getSelection().rangeCount === 0" }),
    );
    assert_eq!(v["result"]["type"], "boolean");
    assert_eq!(v["result"]["value"], true);

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "(() => document.createTreeWalker(document.body, NodeFilter.SHOW_ELEMENT).firstChild() !== null)()" }),
    );
    assert_eq!(v["result"]["type"], "boolean");
    assert_eq!(v["result"]["value"], true);

    let blocked_html = dir.path().join("cdp-csp-blocked.html");
    std::fs::write(
        &blocked_html,
        "<meta http-equiv='Content-Security-Policy' content=\"script-src 'self'\"><script>globalThis.__cdpBlockedInline = true;</script>",
    )
    .unwrap();
    let blocked_url = format!("file://{}", blocked_html.display());
    let v = dispatch_one(&mut s, "Page.navigate", json!({ "url": blocked_url }));
    assert_eq!(v["frameId"], "main");

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "typeof __cdpBlockedInline" }),
    );
    assert_eq!(v["result"]["type"], "string");
    assert_eq!(v["result"]["value"], "undefined");

    // Script error carries the stable code. The text is the generic
    // "script error: ..." from `EngineError`.
    let lines = dispatch_lines(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "throw new Error('boom')" }),
    );
    let details = &lines[0]["result"]["exceptionDetails"];
    assert_eq!(
        details["code"],
        codes::SCRIPT_EVAL,
        "stable code: {details}",
    );
    assert!(
        details["text"].as_str().unwrap().contains("script"),
        "expected script-error text: {details}",
    );
    assert_eq!(lines[1]["method"], "Runtime.exceptionThrown");
    assert_eq!(
        lines[1]["params"]["exceptionDetails"]["code"],
        codes::SCRIPT_EVAL
    );
}

#[test]
fn page_navigate_same_url_resets_page_realm() {
    let dir = tempfile::tempdir().unwrap();
    let html = dir.path().join("same-page.html");
    std::fs::write(
        &html,
        "<html><head><style>body { margin: 0; } #hit { display: block; width: 80px; height: 30px; }</style><script>globalThis.__clicks = 0;</script></head><body><button id='hit'>Hit</button><script>document.querySelector('#hit').addEventListener('click', () => { globalThis.__clicks += 1; console.log('same-page-click', globalThis.__clicks); });</script></body></html>",
    )
    .unwrap();
    let url = format!("file://{}", html.display());
    let mut s = CdpState::default();

    dispatch_one(&mut s, "Runtime.enable", json!({}));
    dispatch_one(&mut s, "Page.navigate", json!({ "url": url }));
    dispatch_one(
        &mut s,
        "Input.dispatchMouseEvent",
        json!({ "type": "mousePressed", "x": 10, "y": 10, "button": "left", "buttons": 1 }),
    );
    dispatch_one(
        &mut s,
        "Input.dispatchMouseEvent",
        json!({ "type": "mouseReleased", "x": 10, "y": 10, "button": "left", "buttons": 0 }),
    );
    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "__clicks" }),
    );
    assert_eq!(v["result"]["value"], 1);

    let url = format!("file://{}", html.display());
    dispatch_one(&mut s, "Page.navigate", json!({ "url": url }));
    dispatch_one(
        &mut s,
        "Input.dispatchMouseEvent",
        json!({ "type": "mousePressed", "x": 10, "y": 10, "button": "left", "buttons": 1 }),
    );
    dispatch_one(
        &mut s,
        "Input.dispatchMouseEvent",
        json!({ "type": "mouseReleased", "x": 10, "y": 10, "button": "left", "buttons": 0 }),
    );
    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "__clicks" }),
    );
    assert_eq!(v["result"]["value"], 1);
}
