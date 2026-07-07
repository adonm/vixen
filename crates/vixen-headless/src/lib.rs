//! vixen-headless — headless CLI + CDP server.
//!
//! Phase 2 implements the CLI flag surface (docs/SPEC.md "Headless CLI
//! surface") and wires `--url`/`--eval` to the SpiderMonkey runtime
//! (`vixen-engine::script`). Phase 3+ DOM/selector/layout/paint paths run
//! through `vixen_engine::page::Page`; broad host-binding smoke still uses that
//! facade while the first focused `document` / `Element` evals run in
//! SpiderMonkey with a Page snapshot.
//! Renderer/CDP-only flags keep the stable error codes (`unsupported.screenshot`,
//! `invalid-selector`) until their later phases land.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use vixen_engine::engine_error::codes;
use vixen_engine::page::Page;
use vixen_engine::script::JsRuntime;
use vixen_net::{CookieJar, Method, Network};

pub mod cdp;
mod interactions;
pub mod surface;

/// The `vixen-headless` CLI (docs/SPEC.md "Headless CLI surface").
/// Flags and stable error codes are a public contract — automation depends on them.
#[derive(Parser, Debug)]
#[command(name = "vixen-headless", version, about = "Vixen headless engine")]
pub struct Cli {
    /// Load a URL (required, except with --list-fonts).
    #[arg(long)]
    pub url: Option<String>,

    /// Save a PNG screenshot.
    #[arg(long)]
    pub screenshot: Option<PathBuf>,

    /// Viewport size (default 800x600).
    #[arg(long, default_value = "800x600")]
    pub viewport: String,

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

    /// Dump paint commands.
    #[arg(long)]
    pub dump_display_list: bool,

    /// Dump inline layout lines.
    #[arg(long)]
    pub dump_lines: bool,

    /// Dump the Vixen layout tree.
    #[arg(long)]
    pub dump_layout_tree: bool,

    /// Dispatch a MouseEvent at coordinates (X,Y).
    #[arg(long)]
    pub click_at: Option<String>,

    /// Focus an element by id.
    #[arg(long)]
    pub focus: Option<String>,

    /// Submit a form by id.
    #[arg(long)]
    pub submit_form: Option<String>,

    /// Print paint statistics.
    #[arg(long)]
    pub paint_stats: bool,

    /// Two-frame incremental repaint demo (with --screenshot + --eval).
    #[arg(long)]
    pub incremental: bool,

    /// Start CDP WebSocket server on 127.0.0.1.
    #[arg(long)]
    pub cdp: bool,

    /// CDP port (default 9222, with --cdp).
    #[arg(long, default_value_t = 9222)]
    pub cdp_port: u16,

    /// List system fonts and exit.
    #[arg(long)]
    pub list_fonts: bool,

    /// Print memory statistics.
    #[arg(long)]
    pub memory_stats: bool,
}

/// Run the CLI. Returns a process exit code.
pub fn run(cli: Cli) -> ExitCode {
    // `--list-fonts` short-circuits and needs no URL.
    if cli.list_fonts {
        return run_list_fonts();
    }

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

    // `--incremental` is a two-frame screenshot workflow. Validate the required
    // flag combination now, then fail on the same stable renderer code as
    // `--screenshot` until the Phase 5 offscreen path lands.
    if cli.incremental {
        if cli.screenshot.is_none() || cli.eval.is_none() {
            eprintln!("error: --incremental requires --screenshot and --eval");
            return ExitCode::from(2);
        }
        return run_screenshot(url, viewport);
    }

    // --screenshot requires the offscreen renderer (Phase 5). Probe the
    // concrete headless `GlContext` creation boundary before all other page
    // actions so combinations fail closed consistently with the stable code.
    if cli.screenshot.is_some() {
        return run_screenshot(url, viewport);
    }

    if has_interaction_action(&cli) {
        let code = interactions::run(url, &cli);
        if code != ExitCode::SUCCESS || !has_non_interaction_action(&cli) {
            return code;
        }
    }

    // --eval: the Phase 2 gate path.
    if let Some(js) = cli.eval.as_deref() {
        return run_eval(url, js);
    }

    // --dump-dom / --extract-text / --dump-layout-tree / --dump-lines /
    // --dump-display-list / --paint-stats: load the URL's HTML and print.
    if cli.dump_dom
        || cli.extract_text
        || cli.dump_layout_tree
        || cli.dump_lines
        || cli.dump_display_list
        || cli.paint_stats
    {
        return run_dom_outputs(url, &cli, viewport);
    }

    // --extract-selector: validate the selector first (`invalid-selector` on
    // malformed input — docs/SPEC.md), then walk the parsed DOM and print
    // each match as JSON. Selector matching runs through Stylo (Phase 3).
    if let Some(sel) = cli.extract_selector.as_deref() {
        return run_extract_selector(url, sel, viewport);
    }

    // `--cdp` starts the WebSocket CDP server (Phase 8 step 4). It runs
    // until the process is killed.
    if cli.cdp {
        return run_cdp_server(url, cli.cdp_port);
    }

    // Nothing else to do: still perform the load so URL-only runs exercise the
    // same file/HTTP trust boundary as the other page actions.
    match load_page(url) {
        Ok(page) => {
            eprintln!("loaded {}", page.url());
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
/// SpiderMonkey is `!Send + !Sync`, so the server runs on a single-threaded
/// tokio runtime + `LocalSet`. CDP clients keep one long-lived WebSocket
/// connection per browser instance, so single-threaded serving is not a
/// bottleneck in practice.
fn run_cdp_server(initial_url: &str, port: u16) -> ExitCode {
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
        cdp::serve_with_initial_url(port, Some(initial_url.to_owned())),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: CDP server failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_screenshot(_url: &str, viewport: (u32, u32)) -> ExitCode {
    match surface::SurfacelessSurface::new(viewport) {
        Ok(_surface) => {
            // The EGL context constructor is the first Phase 5 boundary; PNG
            // capture still waits for the single WebRender paint path.
            eprintln!("{}", codes::UNSUPPORTED_SCREENSHOT);
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("{}", err.stable_code());
            ExitCode::FAILURE
        }
    }
}

fn has_non_memory_action(cli: &Cli) -> bool {
    cli.screenshot.is_some()
        || cli.extract_text
        || cli.extract_selector.is_some()
        || cli.eval.is_some()
        || cli.dump_dom
        || cli.dump_display_list
        || cli.dump_lines
        || cli.dump_layout_tree
        || cli.click_at.is_some()
        || cli.focus.is_some()
        || cli.submit_form.is_some()
        || cli.paint_stats
        || cli.incremental
        || cli.cdp
}

fn has_interaction_action(cli: &Cli) -> bool {
    cli.click_at.is_some() || cli.focus.is_some() || cli.submit_form.is_some()
}

fn has_non_interaction_action(cli: &Cli) -> bool {
    cli.extract_text
        || cli.extract_selector.is_some()
        || cli.eval.is_some()
        || cli.dump_dom
        || cli.dump_display_list
        || cli.dump_lines
        || cli.dump_layout_tree
        || cli.paint_stats
        || cli.cdp
}

fn run_list_fonts() -> ExitCode {
    for path in collect_font_files() {
        println!("{}", path.display());
    }
    ExitCode::SUCCESS
}

fn collect_font_files() -> Vec<PathBuf> {
    let mut roots = vec![
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".local/share/fonts"));
    }

    let mut fonts = Vec::new();
    for root in roots {
        collect_font_files_under(&root, 0, &mut fonts);
    }
    fonts.sort();
    fonts.dedup();
    fonts
}

fn collect_font_files_under(root: &Path, depth: u8, fonts: &mut Vec<PathBuf>) {
    if depth > 8 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_font_files_under(&path, depth + 1, fonts);
        } else if is_font_file(&path) {
            fonts.push(path);
        }
    }
}

fn is_font_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|ext| matches!(ext.as_str(), "ttf" | "otf" | "ttc" | "woff" | "woff2"))
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

/// `--extract-selector <css>`: parse the URL's HTML, walk the DOM, and
/// print every element matching `css` as a JSON object (one per line).
/// Returns the stable `invalid-selector` code on malformed selectors
/// (docs/SPEC.md). Selector matching uses Stylo via `vixen_engine::style_dom`.
fn run_extract_selector(url: &str, sel: &str, viewport: (u32, u32)) -> ExitCode {
    use vixen_engine::style_dom::Selector;

    let _parsed = match Selector::parse(sel) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("{}", codes::INVALID_SELECTOR);
            return ExitCode::FAILURE;
        }
    };

    let page = match load_page(url) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let matches = match page.query_selector_all_in_viewport(sel, viewport) {
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
fn run_eval(url: &str, js: &str) -> ExitCode {
    let page = match load_page(url) {
        Ok(page) => page,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    if let Some(result) = run_dom_eval_on_page(&page, js) {
        return match result {
            Ok(value) => {
                println!("{value}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }

    let mut rt = match JsRuntime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to start JS engine: {e}");
            return ExitCode::FAILURE;
        }
    };

    // `--url` is the page context. Legacy broad DOM smoke expressions are
    // handled above; the first DOM host-object slice falls through here so
    // SpiderMonkey sees a real `document` snapshot in the global.
    match rt.evaluate_with_page(js, &page) {
        Ok(value) => {
            println!("{}", value.to_display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
fn run_dom_eval(url: &str, js: &str) -> Option<Result<String, String>> {
    if !looks_like_dom_eval(js) {
        return None;
    }
    match load_page(url) {
        Ok(page) => run_dom_eval_on_page(&page, js),
        Err(e) => Some(Err(e)),
    }
}

fn run_dom_eval_on_page(page: &Page, js: &str) -> Option<Result<String, String>> {
    if uses_spidermonkey_dom_eval(js) {
        return None;
    }
    if !looks_like_dom_eval(js) {
        return None;
    }
    page.evaluate_dom_expression(js)
}

pub(crate) fn uses_spidermonkey_dom_eval(js: &str) -> bool {
    let js = js.trim_start();
    matches!(
        js,
        "document.title" | "document.body.textContent" | "document.body.innerText"
    ) || simple_query_selector_eval(js, "document.querySelector(")
        || simple_get_element_by_id_eval(js)
        || simple_query_selector_all_length_eval(js)
}

pub(crate) fn looks_like_dom_eval(js: &str) -> bool {
    let js = js.trim_start();
    js.starts_with("document.")
        || js.starts_with("location.")
        || js.starts_with("window.location.")
        || js.starts_with("history.")
        || js.starts_with("window.history.")
        || js.starts_with("getComputedStyle(")
        || js.starts_with("window.getComputedStyle(")
        || js.starts_with("performance.")
        || js.starts_with("window.performance.")
        || js.starts_with("typeof performance.")
        || js.starts_with("typeof window.performance.")
        || js.starts_with("matchMedia(")
        || js.starts_with("window.matchMedia(")
        || js.starts_with("window.getSelection()")
        || js.starts_with("structuredClone(")
        || js.starts_with("new MutationObserver(")
        || js.starts_with("new Headers(")
        || js.starts_with("new AbortController()")
        || js.starts_with("AbortSignal.")
        || js.starts_with("new URL(")
        || js.starts_with("new URLPattern(")
        || js.starts_with("new URLSearchParams(")
}

fn simple_query_selector_eval(js: &str, prefix: &str) -> bool {
    let Some((selector, tail)) = single_string_arg_call_tail(js, prefix) else {
        return false;
    };
    let Some(member) = tail.strip_prefix(')') else {
        return false;
    };
    is_simple_dom_host_selector(selector) && is_simple_dom_host_member(member)
}

fn simple_get_element_by_id_eval(js: &str) -> bool {
    let Some((id, tail)) = single_string_arg_call_tail(js, "document.getElementById(") else {
        return false;
    };
    let Some(member) = tail.strip_prefix(')') else {
        return false;
    };
    is_simple_dom_host_name(id) && is_simple_dom_host_member(member)
}

fn simple_query_selector_all_length_eval(js: &str) -> bool {
    let Some((selector, tail)) = single_string_arg_call_tail(js, "document.querySelectorAll(")
    else {
        return false;
    };
    tail == ").length" && is_simple_dom_host_selector(selector)
}

fn is_simple_dom_host_member(member: &str) -> bool {
    matches!(
        member,
        " === null"
            | " !== null"
            | ".id"
            | ".className"
            | ".tagName"
            | ".nodeName"
            | ".localName"
            | ".nodeType"
            | ".isConnected"
            | ".ownerDocument === document"
            | ".textContent"
            | ".innerText"
    ) || simple_dom_host_string_method(member, ".getAttribute(")
        || simple_dom_host_string_method(member, ".hasAttribute(")
}

fn simple_dom_host_string_method(member: &str, prefix: &str) -> bool {
    let Some((name, tail)) = single_string_arg_call_tail(member, prefix) else {
        return false;
    };
    tail == ")" && is_simple_dom_host_name(name)
}

fn single_string_arg_call_tail<'a>(input: &'a str, prefix: &str) -> Option<(&'a str, &'a str)> {
    let rest = input.strip_prefix(prefix)?;
    let bytes = rest.as_bytes();
    let quote = *bytes.first()?;
    if quote != b'\'' && quote != b'"' {
        return None;
    }
    let mut escaped = false;
    for index in 1..bytes.len() {
        let byte = bytes[index];
        if escaped {
            escaped = false;
            continue;
        }
        if byte == b'\\' {
            escaped = true;
            continue;
        }
        if byte == quote {
            return Some((&rest[1..index], &rest[index + 1..]));
        }
    }
    None
}

fn is_simple_dom_host_selector(selector: &str) -> bool {
    if selector == "*" {
        return true;
    }
    if let Some(id) = selector.strip_prefix('#') {
        return is_simple_dom_host_selector_atom(id);
    }
    if let Some(class) = selector.strip_prefix('.') {
        return is_simple_dom_host_selector_atom(class);
    }
    is_simple_dom_host_selector_atom(selector)
        && selector
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic())
}

fn is_simple_dom_host_selector_atom(name: &str) -> bool {
    let Some(first) = name.bytes().next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn is_simple_dom_host_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':'))
}

/// `--dump-dom` / `--extract-text` / `--dump-layout-tree` / `--dump-lines` /
/// `--dump-display-list` / `--paint-stats`: load the URL's HTML and print the requested
/// DOM/layout/paint projections.
fn run_dom_outputs(url: &str, cli: &Cli, viewport: (u32, u32)) -> ExitCode {
    let page = match load_page(url) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    if cli.dump_dom {
        print!("{}", page.dump_dom());
    }
    if cli.extract_text {
        println!("{}", page.text_content());
    }
    if cli.dump_layout_tree {
        print!("{}", page.dump_layout_tree(viewport));
    }
    if cli.dump_lines {
        print!("{}", page.dump_lines(viewport));
    }
    if cli.dump_display_list {
        print!("{}", page.dump_display_list(viewport));
    }
    if cli.paint_stats {
        print!("{}", page.dump_paint_stats(viewport));
    }
    ExitCode::SUCCESS
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

/// Load and parse a page through the shared engine facade. This is the single
/// vertical integration entry for headless DOM/selector surfaces while the full
/// network/style/layout/paint pipeline grows behind `vixen_engine::page::Page`.
fn load_page(url: &str) -> Result<Page, String> {
    let source = load_url_source(url)?;
    Page::from_html(source.final_url, &source.html).map_err(|e| format!("parse failed: {e}"))
}

#[derive(Debug)]
struct LoadedSource {
    final_url: String,
    html: String,
}

/// Read a page's source. `file://` is direct filesystem I/O; HTTP(S) crosses
/// the `vixen-net` trust boundary so URL policy, redirects, cookies, timeouts,
/// and body-size limits are all enforced in one place.
fn load_url_source(url: &str) -> Result<LoadedSource, String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("invalid URL: {e}"))?;
    match parsed.scheme() {
        "file" => parsed
            .to_file_path()
            .map_err(|_| "file:// URL has no local path".to_string())
            .and_then(|p| {
                let html = std::fs::read_to_string(&p)
                    .map_err(|e| format!("read {}: {e}", p.display()))?;
                Ok(LoadedSource {
                    final_url: parsed.to_string(),
                    html,
                })
            }),
        "http" | "https" => fetch_http_source(parsed),
        scheme => Err(format!(
            "{scheme}: URLs are not supported by the headless source loader"
        )),
    }
}

fn fetch_http_source(url: url::Url) -> Result<LoadedSource, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("network runtime failed: {e}"))?;
    rt.block_on(async move {
        let mut network =
            Network::with_defaults().map_err(|e| format!("network client failed: {e}"))?;
        let mut jar = CookieJar::default();
        let response = network
            .get_text_with_cookies(&mut jar, &url, false, Method::Get)
            .await
            .map_err(|e| format!("fetch {url}: {e}"))?;
        Ok(LoadedSource {
            final_url: response.final_url,
            html: response.body,
        })
    })
}

/// Minimal URL validation. Network policy (SSRF/private-IP) is enforced in
/// `vixen-net` for HTTP(S); here we only check the scheme is present.
fn validate_url(url: &str) -> Result<(), String> {
    let scheme = url.split("://").next().unwrap_or("");
    if scheme.is_empty() || scheme == url {
        return Err("URL must include a scheme (e.g. https:// or file://)".into());
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
    fn parses_full_flag_surface() {
        // Every flag from docs/SPEC.md parses.
        let cli = parse(&[
            "--url",
            "https://example.com",
            "--screenshot",
            "out.png",
            "--viewport",
            "1280x720",
            "--extract-text",
            "--extract-selector",
            "div.main",
            "--eval",
            "1+2",
            "--dump-dom",
            "--dump-display-list",
            "--dump-lines",
            "--dump-layout-tree",
            "--click-at",
            "10,20",
            "--focus",
            "q",
            "--submit-form",
            "f",
            "--paint-stats",
            "--incremental",
            "--cdp",
            "--cdp-port",
            "9999",
            "--memory-stats",
        ]);
        assert_eq!(cli.url.as_deref(), Some("https://example.com"));
        assert_eq!(cli.viewport, "1280x720");
        assert_eq!(cli.cdp_port, 9999);
        assert!(cli.dump_dom && cli.dump_layout_tree && cli.cdp && cli.incremental);
    }

    #[test]
    fn url_required_unless_list_fonts() {
        // No URL, no --list-fonts → error exit 2.
        let cli = parse(&["--eval", "1+2"]);
        assert_eq!(run(cli), ExitCode::from(2));
    }

    #[test]
    fn list_fonts_needs_no_url() {
        let cli = parse(&["--list-fonts"]);
        assert_eq!(run(cli), ExitCode::SUCCESS);
    }

    #[test]
    fn memory_stats_runs_as_standalone_action() {
        let cli = parse(&["--url", "file:///dev/null", "--memory-stats"]);
        assert_eq!(run(cli), ExitCode::SUCCESS);
    }

    #[test]
    fn screenshot_without_renderer_returns_stable_code() {
        let cli = parse(&["--url", "https://example.com", "--screenshot", "o.png"]);
        let code = run(cli);
        assert_eq!(code, ExitCode::FAILURE);
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
    fn dump_lines_runs_through_page_layout_facade() {
        let dir = tempfile::tempdir().unwrap();
        let html = dir.path().join("lines.html");
        std::fs::write(
            &html,
            "<html><head><title>Hidden</title></head><body><p>one two three four</p></body></html>",
        )
        .unwrap();
        let url = format!("file://{}", html.display());
        let cli = parse(&[
            "--url",
            url.as_str(),
            "--viewport",
            "56x200",
            "--dump-lines",
        ]);
        assert_eq!(run(cli), ExitCode::SUCCESS);
    }

    #[test]
    fn dump_layout_tree_runs_through_page_layout_facade() {
        let dir = tempfile::tempdir().unwrap();
        let html = dir.path().join("layout-tree.html");
        std::fs::write(
            &html,
            "<html><head><title>Hidden</title></head><body><main id='root'><p>one two</p></main></body></html>",
        )
        .unwrap();
        let url = format!("file://{}", html.display());
        let cli = parse(&[
            "--url",
            url.as_str(),
            "--viewport",
            "120x200",
            "--dump-layout-tree",
        ]);
        assert_eq!(run(cli), ExitCode::SUCCESS);
    }

    #[test]
    fn dump_display_list_runs_through_page_paint_facade() {
        let dir = tempfile::tempdir().unwrap();
        let html = dir.path().join("paint.html");
        std::fs::write(
            &html,
            "<html><head><title>Hidden</title></head><body><p>one two three four</p></body></html>",
        )
        .unwrap();
        let url = format!("file://{}", html.display());
        let cli = parse(&[
            "--url",
            url.as_str(),
            "--viewport",
            "56x200",
            "--dump-display-list",
        ]);
        assert_eq!(run(cli), ExitCode::SUCCESS);
    }

    #[test]
    fn paint_stats_runs_through_page_paint_facade() {
        let dir = tempfile::tempdir().unwrap();
        let html = dir.path().join("stats.html");
        std::fs::write(
            &html,
            "<html><head><title>Hidden</title></head><body><p>one two three four</p></body></html>",
        )
        .unwrap();
        let url = format!("file://{}", html.display());
        let cli = parse(&[
            "--url",
            url.as_str(),
            "--viewport",
            "56x200",
            "--paint-stats",
        ]);
        assert_eq!(run(cli), ExitCode::SUCCESS);
    }

    #[test]
    fn interaction_flags_run_through_page_facade() {
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
            "--viewport",
            "120x80",
            "--click-at",
            "10,10",
            "--focus",
            "name",
            "--submit-form",
            "contact",
        ]);
        assert_eq!(run(cli), ExitCode::SUCCESS);
    }

    #[test]
    fn click_at_rejects_malformed_coordinates() {
        let cli = parse(&["--url", "file:///dev/null", "--click-at", "10"]);
        assert_eq!(run(cli), ExitCode::from(2));
    }

    #[test]
    fn incremental_is_not_silently_ignored_when_eval_is_present() {
        let cli = parse(&[
            "--url",
            "file:///dev/null",
            "--eval",
            "1+2",
            "--incremental",
        ]);
        assert_eq!(run(cli), ExitCode::from(2));
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
    fn focused_document_eval_uses_spidermonkey_host_objects() {
        let dir = tempfile::tempdir().unwrap();
        let html = dir.path().join("title.html");
        std::fs::write(
            &html,
            "<html><head><title>DOM title</title></head><body><p id='lead'>body</p></body></html>",
        )
        .unwrap();
        let url = format!("file://{}", html.display());
        assert!(looks_like_dom_eval("document.title"));
        assert!(uses_spidermonkey_dom_eval("document.title"));
        assert!(uses_spidermonkey_dom_eval(
            "document.querySelector('#lead').textContent"
        ));
        assert!(uses_spidermonkey_dom_eval(
            "document.getElementById('lead').tagName"
        ));
        assert!(uses_spidermonkey_dom_eval(
            "document.querySelectorAll('p').length"
        ));
        assert!(!uses_spidermonkey_dom_eval(
            "document.querySelector('#lead').getBoundingClientRect().x"
        ));
        assert_eq!(run_dom_eval(&url, "document.title"), None);
        assert_eq!(
            run_dom_eval(&url, "document.readyState"),
            Some(Ok("complete".into()))
        );
    }

    #[test]
    fn encoding_eval_uses_spidermonkey_host_constructors() {
        assert!(!looks_like_dom_eval("new TextEncoder().encoding"));
        assert!(!looks_like_dom_eval(
            "new TextDecoder('utf-8', { fatal: true }).fatal"
        ));
    }

    #[test]
    fn load_url_source_reads_file_with_final_url() {
        let dir = tempfile::tempdir().unwrap();
        let html = dir.path().join("source.html");
        std::fs::write(&html, "<title>file source</title>").unwrap();
        let url = format!("file://{}", html.display());

        let source = load_url_source(&url).unwrap();

        assert_eq!(source.final_url, url);
        assert_eq!(source.html, "<title>file source</title>");
    }

    #[test]
    fn http_loads_fail_closed_on_private_hosts() {
        let err = load_url_source("http://127.0.0.1/").unwrap_err();

        assert!(
            err.contains("URL rejected by policy"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn jsvalue_display_matches_scalars() {
        assert_eq!(JsValue::Int32(3).to_display(), "3");
        assert_eq!(JsValue::Number(2.5).to_display(), "2.5");
    }
}
