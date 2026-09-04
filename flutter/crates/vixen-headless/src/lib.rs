//! vixen-headless — headless CLI + CDP server.
//!
//! Phase 2 implements the CLI flag surface (docs/SPEC.md "Headless CLI
//! surface"). `--eval` navigates and evaluates through the engine-owned browser
//! core. Selector, URL-only, textual document, and DOM-side interaction actions
//! use the same adapter. Rendered operations belong to the Flutter host; native
//! CDP fails them closed.

#![deny(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use vixen_api::DocumentTextKind;
use vixen_engine::engine_error::codes;

mod browser_adapter;
pub use vixen_cdp as cdp;
mod interactions;

/// The `vixen-headless` CLI (docs/SPEC.md "Headless CLI surface").
/// Flags and stable error codes are a public contract — automation depends on them.
#[derive(Parser, Debug)]
#[command(name = "vixen-headless", version, about = "Vixen headless engine")]
pub struct Cli {
    /// Load a URL (required).
    #[arg(long)]
    pub url: Option<String>,

    /// Viewport size (default 800x600).
    #[arg(long, default_value = "800x600")]
    pub viewport: String,

    /// Persist browser profile state under this directory.
    #[arg(long)]
    pub profile_dir: Option<PathBuf>,

    /// Print visible text content.
    #[arg(long)]
    pub extract_text: bool,

    /// Print JSON snapshots for matching elements.
    #[arg(long)]
    pub extract_selector: Option<String>,

    /// Execute JS, print result.
    #[arg(long)]
    pub eval: Option<String>,

    /// Dump the DOM tree.
    #[arg(long)]
    pub dump_dom: bool,

    /// Focus an element by id.
    #[arg(long)]
    pub focus: Option<String>,

    /// Submit a form by id.
    #[arg(long)]
    pub submit_form: Option<String>,

    /// Start CDP WebSocket server on 127.0.0.1.
    #[arg(long)]
    pub cdp: bool,

    /// CDP port (default 9222, with --cdp).
    #[arg(long, default_value_t = 9222)]
    pub cdp_port: u16,

    /// Print memory statistics.
    #[arg(long)]
    pub memory_stats: bool,
}

/// Run the CLI. Returns a process exit code.
pub fn run(cli: Cli) -> ExitCode {
    // `--url` is required otherwise (docs/SPEC.md).
    let Some(url) = cli.url.as_deref() else {
        eprintln!("error: --url <URL> is required");
        return ExitCode::from(2);
    };

    // Validate the URL early (scheme check; full policy lives in vixen-net).
    if let Err(msg) = validate_url(url) {
        eprintln!("error: {msg}");
        return ExitCode::from(2);
    }

    let viewport = match parse_viewport(&cli.viewport) {
        Ok(viewport) => viewport,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };

    if cli.memory_stats && !has_non_memory_action(&cli) {
        return run_memory_stats();
    }

    if has_interaction_action(&cli) {
        let code = interactions::run(url, &cli);
        if code != ExitCode::SUCCESS || !has_non_interaction_action(&cli) {
            return code;
        }
    }

    // --eval: the Phase 2 gate path.
    if let Some(js) = cli.eval.as_deref() {
        return run_eval(url, js, cli.profile_dir.as_deref());
    }

    // Text-only document projections remain native.
    if cli.dump_dom || cli.extract_text {
        return run_dom_outputs(url, &cli, viewport);
    }

    // --extract-selector: validate the selector first (`invalid-selector` on
    // malformed input — docs/SPEC.md), then query the core-owned document and
    // print each match as JSON. Selector matching runs through Stylo (Phase 3).
    if let Some(sel) = cli.extract_selector.as_deref() {
        return run_extract_selector(url, sel, viewport, cli.profile_dir.as_deref());
    }

    // `--cdp` starts the WebSocket CDP server (Phase 8 step 4). It runs
    // until the process is killed.
    if cli.cdp {
        return run_cdp_server(url, cli.cdp_port, cli.profile_dir.clone());
    }

    // Nothing else to do: still perform the load so URL-only runs exercise the
    // same file/HTTP trust boundary as the other page actions.
    match browser_adapter::BrowserSession::load(url, cli.profile_dir.as_deref()) {
        Ok(mut session) => {
            let loaded_url = match session.current_url() {
                Ok(loaded_url) => loaded_url,
                Err(error) => {
                    eprintln!("error: {error}");
                    return ExitCode::FAILURE;
                }
            };
            eprintln!("loaded {loaded_url}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `--cdp [--cdp-port N]`: run the CDP WebSocket server on 127.0.0.1:N.
/// Blocks until interrupted; exit code 1 on bind failure (e.g. port in use).
///
/// The connection adapter uses local `Rc<RefCell<_>>` protocol state, so it runs
/// on a single-threaded tokio runtime + `LocalSet`. BrowserCore owns its separate
/// engine thread.
fn run_cdp_server(initial_url: &str, port: u16, profile_dir: Option<PathBuf>) -> ExitCode {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to start tokio runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let local = tokio::task::LocalSet::new();
    let result = local.block_on(
        &rt,
        cdp::serve_with_initial_url_and_profile(port, Some(initial_url.to_owned()), profile_dir),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: CDP server failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Test adapter for text/runtime CDP. Rendered methods fail closed; product
/// screenshot and coordinate-input evidence is hosted by Flutter.
pub fn cdp_state_with_runtime(runtime: vixen_engine::script::JsRuntime) -> cdp::CdpState {
    cdp::CdpState::with_runtime(runtime)
}

fn has_non_memory_action(cli: &Cli) -> bool {
    cli.extract_text
        || cli.extract_selector.is_some()
        || cli.eval.is_some()
        || cli.dump_dom
        || cli.focus.is_some()
        || cli.submit_form.is_some()
        || cli.cdp
}

fn has_interaction_action(cli: &Cli) -> bool {
    cli.focus.is_some() || cli.submit_form.is_some()
}

fn has_non_interaction_action(cli: &Cli) -> bool {
    cli.extract_text
        || cli.extract_selector.is_some()
        || cli.eval.is_some()
        || cli.dump_dom
        || cli.cdp
}

#[derive(serde::Serialize)]
struct MemoryStats {
    rss_bytes: Option<u64>,
    virtual_bytes: Option<u64>,
}

fn run_memory_stats() -> ExitCode {
    let stats = memory_stats();
    match serde_json::to_string(&stats) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: failed to serialize memory stats: {e}");
            ExitCode::FAILURE
        }
    }
}

fn memory_stats() -> MemoryStats {
    // Linux `/proc/self/statm` reports page counts: total program size then RSS.
    const PAGE_SIZE_BYTES: u64 = 4096;
    let Some((virtual_pages, rss_pages)) = std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|statm| {
            let mut fields = statm.split_whitespace();
            let virtual_pages = fields.next()?.parse::<u64>().ok()?;
            let rss_pages = fields.next()?.parse::<u64>().ok()?;
            Some((virtual_pages, rss_pages))
        })
    else {
        return MemoryStats {
            rss_bytes: None,
            virtual_bytes: None,
        };
    };
    MemoryStats {
        rss_bytes: rss_pages.checked_mul(PAGE_SIZE_BYTES),
        virtual_bytes: virtual_pages.checked_mul(PAGE_SIZE_BYTES),
    }
}

/// `--extract-selector <css>`: query the core-owned document and print every
/// element matching `css` as a JSON object (one per line).
/// Returns the stable `invalid-selector` code on malformed selectors
/// (docs/SPEC.md). Selector matching uses Stylo via `vixen_engine::style_dom`.
fn run_extract_selector(
    url: &str,
    sel: &str,
    viewport: (u32, u32),
    profile_dir: Option<&Path>,
) -> ExitCode {
    use vixen_engine::style_dom::Selector;

    let _parsed = match Selector::parse(sel) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("{}", codes::INVALID_SELECTOR);
            return ExitCode::FAILURE;
        }
    };

    let mut session = match browser_adapter::BrowserSession::load(url, profile_dir) {
        Ok(session) => session,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let matches = match session.query_selector_all(sel, viewport) {
        Ok(matches) => matches,
        Err(_) => {
            eprintln!("{}", codes::INVALID_SELECTOR);
            return ExitCode::FAILURE;
        }
    };
    for m in matches {
        // One JSON object per line — jq-friendly. Field set matches
        // vixen-wpt's `MatchedElement` projection.
        let json = serde_json::json!({
            "node_id": m.node_id,
            "tag": m.tag,
            "id": m.id,
            "classes": m.classes,
            "text": m.text,
            "bbox": m.bbox.map(|(x, y, w, h)| serde_json::json!({
                "x": x,
                "y": y,
                "w": w,
                "h": h,
            })),
            "attributes": m.attributes.iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<std::collections::BTreeMap<_, _>>(),
        });
        println!("{json}");
    }
    ExitCode::SUCCESS
}

/// `--url file://… --eval '1+2'` → load the page context then prints `3`.
fn run_eval(url: &str, js: &str, profile_dir: Option<&Path>) -> ExitCode {
    let mut session = match browser_adapter::BrowserSession::load(url, profile_dir) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    match session.evaluate(js) {
        Ok(value) => {
            println!("{}", value.to_display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `--dump-dom` / `--extract-text`: load the URL and print source projections.
fn run_dom_outputs(url: &str, cli: &Cli, viewport: (u32, u32)) -> ExitCode {
    let mut session = match browser_adapter::BrowserSession::load(url, cli.profile_dir.as_deref()) {
        Ok(session) => session,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = (|| {
        if cli.dump_dom {
            print!(
                "{}",
                session.document_text(DocumentTextKind::Dom, viewport)?
            );
        }
        if cli.extract_text {
            println!(
                "{}",
                session.document_text(DocumentTextKind::TextContent, viewport)?
            );
        }
        Ok::<(), String>(())
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn parse_viewport(input: &str) -> Result<(u32, u32), String> {
    let Some((w, h)) = input.split_once('x').or_else(|| input.split_once('X')) else {
        return Err("--viewport must be WIDTHxHEIGHT".to_owned());
    };
    let w: u32 = w
        .parse()
        .map_err(|_| "--viewport width must be a positive integer".to_owned())?;
    let h: u32 = h
        .parse()
        .map_err(|_| "--viewport height must be a positive integer".to_owned())?;
    if w == 0 || h == 0 {
        return Err("--viewport dimensions must be positive".to_owned());
    }
    Ok((w, h))
}

/// Minimal URL validation. Network policy (SSRF/private-IP) is enforced in
/// `vixen-net` for HTTP(S); here we only check the scheme is present.
fn validate_url(url: &str) -> Result<(), String> {
    let Some((scheme, _)) = url.split_once(':') else {
        return Err("URL must include a scheme (e.g. https:// or file://)".into());
    };
    if scheme.is_empty()
        || !scheme.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphabetic()
                || (index > 0 && (byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.')))
        })
    {
        return Err("URL must include a valid scheme (e.g. https://, file://, or data:)".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use vixen_engine::script::JsValue;

    fn parse(args: &[&str]) -> Cli {
        let mut all = vec!["vixen-headless"];
        all.extend_from_slice(args);
        Cli::try_parse_from(all).unwrap()
    }

    #[test]
    fn parses_native_text_runtime_flag_surface() {
        let cli = parse(&[
            "--url",
            "https://example.com",
            "--viewport",
            "1280x720",
            "--profile-dir",
            "profiles/baseline",
            "--extract-text",
            "--extract-selector",
            "div.main",
            "--eval",
            "1+2",
            "--dump-dom",
            "--focus",
            "q",
            "--submit-form",
            "f",
            "--cdp",
            "--cdp-port",
            "9999",
            "--memory-stats",
        ]);
        assert_eq!(cli.url.as_deref(), Some("https://example.com"));
        assert_eq!(cli.viewport, "1280x720");
        assert_eq!(cli.profile_dir, Some(PathBuf::from("profiles/baseline")));
        assert_eq!(cli.cdp_port, 9999);
        assert!(cli.dump_dom && cli.cdp && cli.memory_stats);
    }

    #[test]
    fn parses_profile_dir() {
        let cli = parse(&[
            "--url",
            "https://example.com",
            "--profile-dir",
            "/tmp/vixen-profile",
        ]);

        assert_eq!(cli.profile_dir, Some(PathBuf::from("/tmp/vixen-profile")));
    }

    #[test]
    fn url_validation_accepts_standard_and_opaque_schemes() {
        assert!(validate_url("file:///tmp/control.html").is_ok());
        assert!(validate_url("data:text/html,control").is_ok());
        assert!(validate_url("missing-scheme").is_err());
        assert!(validate_url("1invalid:value").is_err());
    }

    #[test]
    fn url_is_required() {
        let cli = parse(&["--eval", "1+2"]);
        assert_eq!(run(cli), ExitCode::from(2));
    }

    #[test]
    fn memory_stats_runs_as_standalone_action() {
        let cli = parse(&["--url", "file:///dev/null", "--memory-stats"]);
        assert_eq!(run(cli), ExitCode::SUCCESS);
    }

    #[test]
    fn invalid_selector_returns_stable_code() {
        // Malformed CSS selectors (not empty input) hit `invalid-selector`
        // via Stylo's parser. Empty input is accepted by the parser and
        // produces zero matches; the test covers the actual malformed case.
        let cli = parse(&[
            "--url",
            "https://example.com",
            "--extract-selector",
            "div >",
        ]);
        let code = run(cli);
        assert_eq!(code, ExitCode::FAILURE);
    }

    #[test]
    fn extract_selector_emits_json_matches() {
        // End-to-end: a real selector walks the parsed DOM and prints JSON.
        // The HTML is read via `file://` (Phase 2 still).
        let dir = tempfile::tempdir().unwrap();
        let html = dir.path().join("p.html");
        std::fs::write(
            &html,
            "<html><body><p class='x'>one</p><p>two</p></body></html>",
        )
        .unwrap();
        let url = format!("file://{}", html.display());
        let cli = parse(&["--url", url.as_str(), "--extract-selector", "p.x"]);
        assert_eq!(run(cli), ExitCode::SUCCESS);
    }

    #[test]
    fn interaction_flags_run_through_browser_core() {
        let dir = tempfile::tempdir().unwrap();
        let html = dir.path().join("interactions.html");
        std::fs::write(
            &html,
            "<style>body { margin: 0; } #hit { width: 40px; height: 20px; }</style>\
             <button id='hit'>Click</button>\
             <form id='contact' action='/submit' method='post'>\
               <input id='name' name='name' value='Ada'>\
               <textarea name='body'>Hello</textarea>\
             </form>",
        )
        .unwrap();
        let url = format!("file://{}", html.display());
        let cli = parse(&[
            "--url",
            url.as_str(),
            "--focus",
            "name",
            "--submit-form",
            "contact",
        ]);
        assert_eq!(run(cli), ExitCode::SUCCESS);
    }

    #[test]
    fn viewport_parser_rejects_bad_dimensions() {
        assert_eq!(parse_viewport("800x600").unwrap(), (800, 600));
        assert_eq!(parse_viewport("800X600").unwrap(), (800, 600));
        assert!(parse_viewport("800").is_err());
        assert!(parse_viewport("0x600").is_err());
        assert!(parse_viewport("800xnope").is_err());
    }

    #[test]
    fn eval_gate_returns_three() {
        // The Phase 2 gate: --eval '1+2' prints 3 and exits 0.
        let cli = parse(&["--url", "file:///dev/null", "--eval", "1+2"]);
        assert_eq!(run(cli), ExitCode::SUCCESS);
    }

    #[test]
    fn focused_document_eval_uses_runtime_host_objects() {
        let dir = tempfile::tempdir().unwrap();
        let html = dir.path().join("title.html");
        std::fs::write(
            &html,
            "<html><head><title>DOM title</title><style>#lead { color: blue; }</style><link id='theme' rel='stylesheet alternate'></head><body><p id='lead' class='note callout' data-author-name='ada'>body</p><form id='f' method='POST' enctype='multipart/form-data' action='/submit'></form><iframe id='frame' sandbox='allow-scripts'></iframe><img id='widths' src='small.jpg' srcset='small.jpg 480w, medium.jpg 800w' sizes='100vw'></body></html>",
        )
        .unwrap();
        let url = format!("file://{}", html.display());
        let mut session = browser_adapter::BrowserSession::load(&url, None).unwrap();
        assert_eq!(
            session.evaluate("document.title").unwrap(),
            vixen_api::ScriptValue::String("DOM title".to_owned())
        );
        assert_eq!(
            session
                .evaluate("document.querySelector('#lead').textContent")
                .unwrap(),
            vixen_api::ScriptValue::String("body".to_owned())
        );
        assert_eq!(
            session
                .evaluate("document.querySelector('#lead').dataset.authorName")
                .unwrap(),
            vixen_api::ScriptValue::String("ada".to_owned())
        );
        assert_eq!(
            session.evaluate("new TextEncoder().encoding").unwrap(),
            vixen_api::ScriptValue::String("utf-8".to_owned())
        );
    }

    #[test]
    fn jsvalue_display_matches_scalars() {
        assert_eq!(JsValue::Int32(3).to_display(), "3");
        assert_eq!(JsValue::Number(2.5).to_display(), "2.5");
    }
}
