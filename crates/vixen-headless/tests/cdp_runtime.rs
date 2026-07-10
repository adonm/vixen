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

fn dispatch_session(
    state: &mut CdpState,
    session_id: Option<&str>,
    method: &str,
    params: Value,
) -> Value {
    let req = json!({
        "id": 91,
        "sessionId": session_id,
        "method": method,
        "params": params,
    });
    state
        .handle_text_sync(&req.to_string())
        .into_iter()
        .map(|line| serde_json::from_str::<Value>(&line).unwrap())
        .find(|message| message["id"] == 91)
        .unwrap_or_else(|| panic!("{method} returned no response"))
}

fn spawn_page_server(
    host: &str,
    requests: usize,
) -> (
    String,
    vixen_net::NetworkConfig,
    std::thread::JoinHandle<()>,
) {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        for _ in 0..requests {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let read = stream.read(&mut request).unwrap_or(0);
            let request = String::from_utf8_lossy(&request[..read]);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/");
            let body = format!("<!doctype html><title>{path}</title><main>{path}</main>");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
    });
    let mut config = vixen_net::NetworkConfig::default();
    config.dns_overrides.push((host.to_owned(), vec![addr]));
    (format!("http://{host}:{}", addr.port()), config, handle)
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

fn spawn_header_echo_server(
    host: &str,
) -> (
    String,
    vixen_net::NetworkConfig,
    std::thread::JoinHandle<()>,
) {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 2048];
        let read = stream.read(&mut request).unwrap_or(0);
        let headers = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
        let body = if headers.contains("\r\nx-cdp-token: abc\r\n") {
            "header-seen"
        } else {
            "header-missing"
        };
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

fn spawn_sequential_fetch_server(
    host: &str,
    bodies: [&'static str; 2],
) -> (
    String,
    vixen_net::NetworkConfig,
    std::thread::JoinHandle<()>,
) {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        for body in bodies {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 512];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
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
        "<html><head><title>CDP DOM</title><style>body { margin: 0; } #hit { width: 80px; height: 30px; display: block; } #lead { color: blue; font-size: 20px !important; } p { margin-left: 4px; } #box { width: 40px; height: 20px; }</style><link id='theme' rel='stylesheet alternate'><script>globalThis.__cdpInline = 40; localStorage.setItem('cdp-inline', 'ran');</script><script>globalThis.__cdpInline += 2;</script><script src='cdp-external.js'></script></head><body><button id='hit'>Hit</button><p id='status'>waiting</p><div id='dynamic-root'></div><script>document.querySelector('#hit').addEventListener('click', () => { globalThis.__cdpClicked = (globalThis.__cdpClicked || 0) + 1; const status = document.querySelector('#status'); status.textContent = 'clicked:' + globalThis.__cdpClicked; status.classList.add('clicked'); status.setAttribute('data-clicked', String(globalThis.__cdpClicked)); status.style.width = '140px'; const root = document.querySelector('#dynamic-root'); const dynamic = document.createElement('span'); dynamic.id = 'dynamic'; dynamic.className = 'badge'; dynamic.textContent = 'dynamic:' + globalThis.__cdpClicked; const gone = document.createElement('em'); gone.id = 'gone'; gone.textContent = 'gone'; root.appendChild(gone); root.removeChild(gone); root.replaceChildren(dynamic, ' ready'); console.log('clicked', document.querySelector('#hit').id); });</script><p id='lead' class='note note callout' data-role='copy' data-author-name='ada' style='font-size: 18px; margin-left: 10px'>Hello <b>CDP</b></p><div id='box'>Box</div><form id='contact'><input name='name' value='Ada'></form><iframe id='frame' sandbox='allow-scripts allow-same-origin'></iframe></body></html>",
    )
    .unwrap();
    let url = format!("file://{}", html.display());
    let v = dispatch_one(&mut s, "Page.navigate", json!({ "url": url }));
    assert_eq!(v["frameId"], "tab-1");

    dispatch_one(
        &mut s,
        "Emulation.setDeviceMetricsOverride",
        json!({ "width": 500, "height": 320, "deviceScaleFactor": 1, "mobile": false }),
    );
    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "`${innerWidth}x${innerHeight}:${document.documentElement.clientWidth}x${document.documentElement.clientHeight}:${matchMedia('(max-width: 600px)').matches}`" }),
    );
    assert_eq!(v["result"]["type"], "string");
    assert_eq!(v["result"]["value"], "500x320:500x320:true");
    dispatch_one(&mut s, "Emulation.clearDeviceMetricsOverride", json!({}));

    dispatch_one(
        &mut s,
        "Emulation.setEmulatedMedia",
        json!({
            "media": "print",
            "features": [{ "name": "prefers-color-scheme", "value": "dark" }]
        }),
    );
    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "`${matchMedia('screen').matches}:${matchMedia('print').matches}:${matchMedia('(prefers-color-scheme: dark)').matches}:${matchMedia('(prefers-color-scheme: light)').matches}`" }),
    );
    assert_eq!(v["result"]["type"], "string");
    assert_eq!(v["result"]["value"], "false:true:true:false");
    dispatch_one(&mut s, "Emulation.setEmulatedMedia", json!({}));

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
        json!({ "expression": "document.querySelector('#status').textContent" }),
    );
    assert_eq!(v["result"]["type"], "string");
    assert_eq!(v["result"]["value"], "clicked:1");

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "(() => document.querySelector('#status').classList.contains('clicked') + ':' + document.querySelector('#status').getAttribute('data-clicked') + ':' + document.querySelector('#status').style.width)()" }),
    );
    assert_eq!(v["result"]["type"], "string");
    assert_eq!(v["result"]["value"], "true:1:140px");

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "(() => document.querySelector('#dynamic').textContent + ':' + document.querySelector('#dynamic').className + ':' + (document.querySelector('#gone') === null) + ':' + document.querySelector('#dynamic-root').textContent)()" }),
    );
    assert_eq!(v["result"]["type"], "string");
    assert_eq!(v["result"]["value"], "dynamic:1:badge:true:dynamic:1 ready");

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

    dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "(() => { const field = document.querySelector('input[name=name]'); globalThis.__keyEvents = []; field.addEventListener('keydown', (event) => __keyEvents.push('keydown:' + event.key)); field.addEventListener('input', (event) => __keyEvents.push('input:' + event.inputType + ':' + event.data)); field.addEventListener('change', () => __keyEvents.push('change')); field.addEventListener('keyup', (event) => __keyEvents.push('keyup:' + event.key)); field.focus(); field.select(); return 'ready'; })()" }),
    );
    let v = dispatch_one(
        &mut s,
        "Input.dispatchKeyEvent",
        json!({ "type": "keyDown", "key": "B", "code": "KeyB", "text": "B" }),
    );
    assert_eq!(v, json!({}));
    let v = dispatch_one(
        &mut s,
        "Input.dispatchKeyEvent",
        json!({ "type": "keyUp", "key": "B", "code": "KeyB" }),
    );
    assert_eq!(v, json!({}));
    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "(() => { const form = document.getElementById('contact'); const field = document.querySelector('input[name=name]'); return field.value + ':' + new FormData(form).get('name') + ':' + __keyEvents.join('>'); })()" }),
    );
    assert_eq!(v["result"]["type"], "string");
    assert_eq!(
        v["result"]["value"],
        "B:B:keydown:B>input:insertText:B>change>keyup:B"
    );

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
    assert_eq!(v["frameId"], "tab-1");

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
fn network_extra_http_headers_apply_to_runtime_fetch() {
    let (url, network_config, server) = spawn_header_echo_server("vixen-cdp-headers.com");
    let rt = JsRuntime::with_network_config(network_config).expect("JS init");
    let mut s = CdpState::with_runtime(rt);

    dispatch_one(
        &mut s,
        "Network.setExtraHTTPHeaders",
        json!({ "headers": { "X-CDP-Token": "abc" } }),
    );
    let value = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": format!("fetch({url:?}).then((response) => response.text())") }),
    );
    assert_eq!(value["result"]["type"], "string");
    assert_eq!(value["result"]["value"], "header-seen");
    server.join().unwrap();
}

#[test]
fn network_cache_disabled_bypasses_runtime_fetch_cache() {
    let (url, network_config, server) =
        spawn_sequential_fetch_server("vixen-cdp-cache.com", ["first", "second"]);
    let rt = JsRuntime::with_network_config(network_config).expect("JS init");
    let mut s = CdpState::with_runtime(rt);

    let first = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": format!("fetch({url:?}).then((response) => response.text())") }),
    );
    assert_eq!(first["result"]["value"], "first");

    dispatch_one(
        &mut s,
        "Network.setCacheDisabled",
        json!({ "cacheDisabled": true }),
    );
    let second = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": format!("fetch({url:?}, {{ cache: 'force-cache' }}).then((response) => response.text())") }),
    );
    assert_eq!(second["result"]["type"], "string");
    assert_eq!(second["result"]["value"], "second");
    server.join().unwrap();
}

#[test]
fn runtime_navigation_history_and_form_actions_update_page() {
    let dir = tempfile::tempdir().unwrap();
    let one = dir.path().join("one.html");
    let two = dir.path().join("two.html");
    std::fs::write(
        &one,
        "<title>One</title><form id='f' action='two.html'><input name='q' value='rust'><textarea name='body'></textarea><button id='go'>Go</button></form>",
    )
    .unwrap();
    std::fs::write(&two, "<title>Two</title><p id='dest'>Arrived</p>").unwrap();

    let one_url = format!("file://{}", one.display());
    let mut s = CdpState::default();
    dispatch_one(&mut s, "Runtime.enable", json!({}));
    dispatch_one(&mut s, "Page.navigate", json!({ "url": one_url }));

    let lines = dispatch_lines(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "location.assign('two.html'); 'queued'" }),
    );
    assert_eq!(lines[0]["result"]["result"]["value"], "queued");
    assert!(
        lines
            .iter()
            .any(|line| line["method"] == "Page.loadEventFired")
    );
    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.title" }),
    );
    assert_eq!(v["result"]["value"], "Two");

    dispatch_one(&mut s, "Page.reload", json!({}));
    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.title" }),
    );
    assert_eq!(v["result"]["value"], "Two");

    let history = dispatch_one(&mut s, "Page.getNavigationHistory", json!({}));
    assert_eq!(history["currentIndex"], 1);
    assert_eq!(history["entries"].as_array().unwrap().len(), 2);
    let one_entry_id = history["entries"][0]["id"].as_u64().unwrap();
    let two_entry_id = history["entries"][1]["id"].as_u64().unwrap();

    dispatch_one(
        &mut s,
        "Page.navigateToHistoryEntry",
        json!({ "entryId": one_entry_id }),
    );
    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.title" }),
    );
    assert_eq!(v["result"]["value"], "One");
    dispatch_one(
        &mut s,
        "Page.navigateToHistoryEntry",
        json!({ "entryId": two_entry_id }),
    );
    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.title" }),
    );
    assert_eq!(v["result"]["value"], "Two");

    dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "history.back(); 'back'" }),
    );
    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.title" }),
    );
    assert_eq!(v["result"]["value"], "One");

    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "history.pushState({ ok: 7 }, '', 'state.html'); history.length + ':' + history.state.ok + ':' + location.href.endsWith('/state.html')" }),
    );
    assert_eq!(v["result"]["value"], "2:7:true");
    let targets = dispatch_one(&mut s, "Target.getTargets", json!({}));
    assert!(
        targets["targetInfos"][0]["url"]
            .as_str()
            .unwrap()
            .ends_with("/state.html")
    );

    dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "const q = document.querySelector('input[name=q]'); q.focus(); q.select(); 'focused'" }),
    );
    for ch in ["f", "e", "r", "r", "i", "s"] {
        dispatch_one(
            &mut s,
            "Input.dispatchKeyEvent",
            json!({ "type": "keyDown", "key": ch, "text": ch }),
        );
        dispatch_one(
            &mut s,
            "Input.dispatchKeyEvent",
            json!({ "type": "keyUp", "key": ch }),
        );
    }
    dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "const body = document.querySelector('textarea[name=body]'); body.focus(); body.select(); 'body-focused'" }),
    );
    for ch in ["h", "e", "l", "l", "o", " ", "c", "d", "p"] {
        dispatch_one(
            &mut s,
            "Input.dispatchKeyEvent",
            json!({ "type": "keyDown", "key": ch, "text": ch }),
        );
        dispatch_one(
            &mut s,
            "Input.dispatchKeyEvent",
            json!({ "type": "keyUp", "key": ch }),
        );
    }

    dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.querySelector('#go').click(); 'submitted'" }),
    );
    let targets = dispatch_one(&mut s, "Target.getTargets", json!({}));
    let final_url = targets["targetInfos"][0]["url"].as_str().unwrap();
    assert!(
        final_url.ends_with("/two.html?q=ferris&body=hello+cdp"),
        "{final_url}"
    );
    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.title" }),
    );
    assert_eq!(v["result"]["value"], "Two");
}

#[test]
fn runtime_form_submit_uses_node_id_submitter_and_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let page_path = dir.path().join("idless.html");
    let override_path = dir.path().join("override.html");
    std::fs::write(
        &page_path,
        "<title>Form</title><form action='default.html' method='post' enctype='text/plain'><input name='q' value='rust'><button id='go' name='via' value='button' formaction='override.html' formmethod='get' formenctype='application/x-www-form-urlencoded'>Go</button></form>",
    )
    .unwrap();
    std::fs::write(&override_path, "<title>Override</title><p>ok</p>").unwrap();

    let mut s = CdpState::default();
    dispatch_one(&mut s, "Runtime.enable", json!({}));
    dispatch_one(
        &mut s,
        "Page.navigate",
        json!({ "url": format!("file://{}", page_path.display()) }),
    );

    dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.querySelector('#go').click(); 'submitted'" }),
    );

    let targets = dispatch_one(&mut s, "Target.getTargets", json!({}));
    let final_url = targets["targetInfos"][0]["url"].as_str().unwrap();
    assert!(
        final_url.ends_with("/override.html?q=rust&via=button"),
        "{final_url}"
    );
    let v = dispatch_one(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "document.title" }),
    );
    assert_eq!(v["result"]["value"], "Override");
}

#[test]
fn fetch_policy_failure_surfaces_cdp_loading_failed() {
    let mut s = CdpState::default();
    dispatch_one(&mut s, "Runtime.enable", json!({}));
    dispatch_one(&mut s, "Network.enable", json!({}));

    let lines = dispatch_lines(
        &mut s,
        "Runtime.evaluate",
        json!({ "expression": "fetch('http://127.0.0.1:9/').then(() => false, (err) => /blocked host|URL rejected/.test(err.message))" }),
    );

    assert_eq!(lines[0]["result"]["result"]["value"], true);
    assert!(
        lines
            .iter()
            .any(|line| line["method"] == "Network.requestWillBeSent"),
        "expected request notification: {lines:#?}"
    );
    let failed = lines
        .iter()
        .find(|line| line["method"] == "Network.loadingFailed")
        .expect("loadingFailed notification");
    assert_eq!(failed["params"]["blockedReason"], "url-policy");
    assert!(
        failed["params"]["errorText"]
            .as_str()
            .unwrap()
            .contains("URL rejected"),
        "{failed:#?}"
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

#[test]
fn two_targets_route_to_independent_core_contexts() {
    let (origin, network, server) = spawn_page_server("cdp-contexts.com", 3);
    let runtime = JsRuntime::with_network_config(network).expect("JS init");
    let mut state = CdpState::with_runtime(runtime);

    let first_target = dispatch_session(&mut state, None, "Target.getTargets", json!({}))["result"]
        ["targetInfos"][0]["targetId"]
        .as_str()
        .unwrap()
        .to_owned();
    let first_session = dispatch_session(
        &mut state,
        None,
        "Target.attachToTarget",
        json!({ "targetId": first_target, "flatten": true }),
    )["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    let create_second = dispatch_session(
        &mut state,
        None,
        "Target.createTarget",
        json!({ "url": format!("{origin}/b") }),
    );
    let second_target = create_second["result"]["targetId"]
        .as_str()
        .unwrap_or_else(|| panic!("create second target failed: {create_second}"))
        .to_owned();
    let second_session = dispatch_session(
        &mut state,
        None,
        "Target.attachToTarget",
        json!({ "targetId": second_target, "flatten": true }),
    )["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();

    assert!(
        dispatch_session(
            &mut state,
            Some(&first_session),
            "Page.navigate",
            json!({ "url": format!("{origin}/a") }),
        )["error"]
            .is_null()
    );
    let seeded = dispatch_session(
        &mut state,
        Some(&first_session),
        "Runtime.evaluate",
        json!({
            "expression": "globalThis.onlyFirst = 7; sessionStorage.setItem('session', 'first'); localStorage.setItem('shared', 'from-first'); 'seeded'"
        }),
    );
    assert_eq!(seeded["result"]["result"]["value"], "seeded");

    let isolated = dispatch_session(
        &mut state,
        Some(&second_session),
        "Runtime.evaluate",
        json!({
            "expression": "`${typeof globalThis.onlyFirst}:${sessionStorage.getItem('session')}:${localStorage.getItem('shared')}`"
        }),
    );
    assert_eq!(
        isolated["result"]["result"]["value"],
        "undefined:null:from-first"
    );

    dispatch_session(
        &mut state,
        Some(&first_session),
        "Page.navigate",
        json!({ "url": format!("{origin}/next") }),
    );
    let second_unchanged = dispatch_session(
        &mut state,
        Some(&second_session),
        "Runtime.evaluate",
        json!({ "expression": "document.title + ':' + localStorage.getItem('shared')" }),
    );
    assert_eq!(
        second_unchanged["result"]["result"]["value"],
        "/b:from-first"
    );

    assert_eq!(
        dispatch_session(
            &mut state,
            None,
            "Target.closeTarget",
            json!({ "targetId": first_target }),
        )["result"]["success"],
        true
    );
    let second_after_close = dispatch_session(
        &mut state,
        Some(&second_session),
        "Runtime.evaluate",
        json!({ "expression": "document.title" }),
    );
    assert_eq!(second_after_close["result"]["result"]["value"], "/b");

    server.join().unwrap();
}
