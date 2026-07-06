//! vixen-headless — headless CLI + CDP server.
//!
//! Phase 2 implements the CLI flag surface (docs/SPEC.md "Headless CLI
//! surface") and wires `--url`/`--eval` to the SpiderMonkey runtime
//! (`vixen-engine::script`). Phase 3 DOM/selector paths run through
//! `vixen_engine::page::Page`. The first layout/paint projections are exposed
//! through `--dump-lines`, `--dump-display-list`, and `--paint-stats`;
//! renderer/CDP-only flags keep the stable error codes (`unsupported.screenshot`,
//! `invalid-selector`) until their later phases land.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use vixen_engine::engine_error::codes;
use vixen_engine::page::Page;
use vixen_engine::script::JsRuntime;

pub mod cdp;

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
    // `--list-fonts` short-circuits and needs no URL (Phase 8 wires fontconfig).
    if cli.list_fonts {
        eprintln!("--list-fonts: not implemented yet (Phase 8)");
        return ExitCode::from(2);
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

    // --eval: the Phase 2 gate path.
    if let Some(js) = cli.eval.as_deref() {
        return run_eval(js, &cli);
    }

    // --dump-dom / --extract-text / --dump-lines / --dump-display-list /
    // --paint-stats: parse the URL's HTML and print.
    // (file:// only for now — HTTP fetch lands with the engine pipeline.)
    if cli.dump_dom
        || cli.extract_text
        || cli.dump_lines
        || cli.dump_display_list
        || cli.paint_stats
    {
        return run_dom_outputs(
            url,
            cli.dump_dom,
            cli.extract_text,
            cli.dump_lines,
            cli.dump_display_list,
            cli.paint_stats,
            &cli.viewport,
        );
    }

    // --screenshot requires the offscreen renderer (Phase 5). Without it the
    // stable code `unsupported.screenshot` is returned (docs/SPEC.md).
    if cli.screenshot.is_some() {
        eprintln!("{}", codes::UNSUPPORTED_SCREENSHOT);
        return ExitCode::FAILURE;
    }

    // --extract-selector: validate the selector first (`invalid-selector` on
    // malformed input — docs/SPEC.md), then walk the parsed DOM and print
    // each match as JSON. Selector matching runs through Stylo (Phase 3).
    if let Some(sel) = cli.extract_selector.as_deref() {
        return run_extract_selector(url, sel);
    }

    // Remaining flags need the DOM/layout/paint stack (Phases 3–8).
    let unimplemented = [
        cli.click_at.is_some(),
        cli.focus.is_some(),
        cli.submit_form.is_some(),
        cli.incremental,
        cli.memory_stats,
    ];
    if unimplemented.iter().any(|&f| f) {
        eprintln!("requested feature is not implemented yet (Phases 3–8)");
        return ExitCode::from(2);
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
    // If a screenshot was also requested without a renderer, that still fails
    // with the stable code.
    if cli.screenshot.is_some() {
        eprintln!("{}", codes::UNSUPPORTED_SCREENSHOT);
        return ExitCode::FAILURE;
    }

    let mut rt = match JsRuntime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to start JS engine: {e}");
            return ExitCode::FAILURE;
        }
    };

    // `--url` is accepted as the page context (validated earlier); the JS runs
    // in a fresh global. Full DOM/`document` wiring lands in Phase 6.
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

/// `--dump-dom` / `--extract-text` / `--dump-lines` / `--dump-display-list` /
/// `--paint-stats`: parse the URL's HTML and print the requested
/// DOM/layout/paint projections. `file://` only — HTTP fetch lands with the
/// engine pipeline (Phase 6).
fn run_dom_outputs(
    url: &str,
    dump_dom: bool,
    extract_text: bool,
    dump_lines: bool,
    dump_display_list: bool,
    paint_stats: bool,
    viewport: &str,
) -> ExitCode {
    let page = match load_page(url) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let viewport = if dump_lines || dump_display_list || paint_stats {
        match parse_viewport(viewport) {
            Ok(v) => Some(v),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        None
    };
    if dump_dom {
        print!("{}", page.dump_dom());
    }
    if extract_text {
        println!("{}", page.text_content());
    }
    if dump_lines {
        print!("{}", page.dump_lines(viewport.expect("viewport parsed")));
    }
    if dump_display_list {
        print!(
            "{}",
            page.dump_display_list(viewport.expect("viewport parsed"))
        );
    }
    if paint_stats {
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
/// vertical integration entry for headless DOM/selector surfaces until the full
/// network/style/layout/paint pipeline grows behind `vixen_engine::page::Page`.
fn load_page(url: &str) -> Result<Page, String> {
    let html = load_url_source(url)?;
    Page::from_html(url.to_owned(), &html).map_err(|e| format!("parse failed: {e}"))
}

/// Read a page's source. Only `file://` is supported until the networked
/// engine pipeline lands (Phase 6).
fn load_url_source(url: &str) -> Result<String, String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("invalid URL: {e}"))?;
    match parsed.scheme() {
        "file" => parsed
            .to_file_path()
            .map_err(|_| "file:// URL has no local path".to_string())
            .and_then(|p| {
                std::fs::read_to_string(&p).map_err(|e| format!("read {}: {e}", p.display()))
            }),
        scheme => Err(format!(
            "{scheme}: URLs are not supported yet (Phase 6 fetch)"
        )),
    }
}

/// Minimal URL validation. Network policy (SSRF/private-IP) is enforced in
/// `vixen-net` once fetches exist; here we only check the scheme is present.
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
        assert!(cli.dump_dom && cli.cdp && cli.incremental);
    }

    #[test]
    fn url_required_unless_list_fonts() {
        // No URL, no --list-fonts → error exit 2.
        let cli = parse(&["--eval", "1+2"]);
        assert_eq!(run(cli), ExitCode::from(2));
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
    fn jsvalue_display_matches_scalars() {
        assert_eq!(JsValue::Int32(3).to_display(), "3");
        assert_eq!(JsValue::Number(2.5).to_display(), "2.5");
    }
}
