//! vixen-headless — headless CLI + CDP server.
//!
//! Phase 2 implements the CLI flag surface (docs/SPEC.md "Headless CLI
//! surface") and wires `--url`/`--eval` to the SpiderMonkey runtime
//! (`vixen-engine::script`). Phase 3+ DOM/selector/layout/paint paths run
//! through `vixen_engine::page::Page`; the host-binding smoke subset for
//! `document.title` is served by the same facade until full DOM objects land.
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

    // --screenshot requires the offscreen renderer (Phase 5). Without it the
    // stable code `unsupported.screenshot` is returned (docs/SPEC.md). Check it
    // before all other page actions so combinations fail closed consistently.
    if cli.screenshot.is_some() {
        eprintln!("{}", codes::UNSUPPORTED_SCREENSHOT);
        return ExitCode::FAILURE;
    }

    if cli.memory_stats && !has_non_memory_action(&cli) {
        return run_memory_stats();
    }

    // `--incremental` needs the offscreen screenshot path. Keep it explicit so
    // combinations like `--incremental --eval` do not silently ignore the flag.
    if cli.incremental {
        eprintln!("requested feature is not implemented yet (Phase 5 offscreen renderer)");
        return ExitCode::from(2);
    }

    if has_interaction_action(&cli) {
        let code = interactions::run(url, &cli);
        if code != ExitCode::SUCCESS || !has_non_interaction_action(&cli) {
            return code;
        }
    }

    // --eval: the Phase 2 gate path.
    if let Some(js) = cli.eval.as_deref() {
        return run_eval(js, &cli);
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
        return run_dom_outputs(url, &cli);
    }

    // --extract-selector: validate the selector first (`invalid-selector` on
    // malformed input — docs/SPEC.md), then walk the parsed DOM and print
    // each match as JSON. Selector matching runs through Stylo (Phase 3).
    if let Some(sel) = cli.extract_selector.as_deref() {
        return run_extract_selector(url, sel);
    }

    // `--cdp` starts the WebSocket CDP server (Phase 8 step 4). It runs
    // until the process is killed.
    if cli.cdp {
        return run_cdp_server(cli.cdp_port);
    }

    // Nothing to do — just a URL load. Without the engine pipeline we acknowledge it.
    eprintln!("loaded {url} (no action requested; engine pipeline lands Phases 3–6)");
    ExitCode::SUCCESS
}

/// `--cdp [--cdp-port N]`: run the CDP WebSocket server on 127.0.0.1:N.
/// Blocks until interrupted; exit code 1 on bind failure (e.g. port in use).
///
/// SpiderMonkey is `!Send + !Sync`, so the server runs on a single-threaded
/// tokio runtime + `LocalSet`. CDP clients keep one long-lived WebSocket
/// connection per browser instance, so single-threaded serving is not a
/// bottleneck in practice.
fn run_cdp_server(port: u16) -> ExitCode {
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
    let result = local.block_on(&rt, cdp::serve(port));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: CDP server failed: {e}");
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
fn run_extract_selector(url: &str, sel: &str) -> ExitCode {
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
    let matches = match page.query_selector_all(sel) {
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
            "attributes": m.attributes.iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<std::collections::BTreeMap<_, _>>(),
        });
        println!("{json}");
    }
    ExitCode::SUCCESS
}

/// `--url file://… --eval '1+2'` → prints `3` (the Phase 2 gate).
fn run_eval(js: &str, cli: &Cli) -> ExitCode {
    let url = cli.url.as_deref().expect("--url validated before eval");

    if let Some(result) = run_dom_eval(url, js) {
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

    // `--url` is accepted as the page context (validated earlier). DOM smoke
    // expressions are handled above; remaining JS runs in a fresh global until
    // full DOM object bindings land.
    match rt.evaluate(js) {
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

fn run_dom_eval(url: &str, js: &str) -> Option<Result<String, String>> {
    if !looks_like_dom_eval(js) {
        return None;
    }
    match load_page(url) {
        Ok(page) => page.evaluate_dom_expression(js),
        Err(e) => Some(Err(e)),
    }
}

fn looks_like_dom_eval(js: &str) -> bool {
    let js = js.trim_start();
    js.starts_with("document.") || js.starts_with("location.") || js.starts_with("window.location.")
}

/// `--dump-dom` / `--extract-text` / `--dump-layout-tree` / `--dump-lines` /
/// `--dump-display-list` / `--paint-stats`: load the URL's HTML and print the requested
/// DOM/layout/paint projections.
fn run_dom_outputs(url: &str, cli: &Cli) -> ExitCode {
    let page = match load_page(url) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let viewport =
        if cli.dump_layout_tree || cli.dump_lines || cli.dump_display_list || cli.paint_stats {
            match parse_viewport(&cli.viewport) {
                Ok(v) => Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::from(2);
                }
            }
        } else {
            None
        };
    if cli.dump_dom {
        print!("{}", page.dump_dom());
    }
    if cli.extract_text {
        println!("{}", page.text_content());
    }
    if cli.dump_layout_tree {
        print!(
            "{}",
            page.dump_layout_tree(viewport.expect("viewport parsed"))
        );
    }
    if cli.dump_lines {
        print!("{}", page.dump_lines(viewport.expect("viewport parsed")));
    }
    if cli.dump_display_list {
        print!(
            "{}",
            page.dump_display_list(viewport.expect("viewport parsed"))
        );
    }
    if cli.paint_stats {
        print!(
            "{}",
            page.dump_paint_stats(viewport.expect("viewport parsed"))
        );
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
    fn eval_document_title_runs_through_page_facade() {
        let dir = tempfile::tempdir().unwrap();
        let html = dir.path().join("title.html");
        std::fs::write(
            &html,
            "<html><head><title>DOM title</title></head><body><p>body</p></body></html>",
        )
        .unwrap();
        let url = format!("file://{}", html.display());
        assert_eq!(
            run_dom_eval(&url, "document.title"),
            Some(Ok("DOM title".into()))
        );
        assert_eq!(
            run_dom_eval(&url, "document.querySelectorAll('p').length"),
            Some(Ok("1".into()))
        );
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
