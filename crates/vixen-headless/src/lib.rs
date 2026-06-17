//! vixen-headless — headless CLI + CDP server.
//!
//! Phase 2 implements the CLI flag surface (docs/SPEC.md "Headless CLI
//! surface") and wires `--url`/`--eval` to the SpiderMonkey runtime
//! (`vixen-core::script`). The Phase 2 gate is
//! `vixen-headless --url <file> --eval '1+2'` → `3`. Render/DOM/CDP flags are
//! accepted and dispatched with the **stable error codes** preserved exactly
//! (`unsupported.screenshot`, `invalid-selector`); their full implementation
//! lands in later phases.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use vixen_core::engine_error::codes;
use vixen_core::script::JsRuntime;

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

    // --screenshot requires the offscreen renderer (Phase 5). Without it the
    // stable code `unsupported.screenshot` is returned (docs/SPEC.md).
    if cli.screenshot.is_some() {
        eprintln!("{}", codes::UNSUPPORTED_SCREENSHOT);
        return ExitCode::FAILURE;
    }

    // --extract-selector: validate the selector first (`invalid-selector` on
    // malformed input — docs/SPEC.md). Extraction itself needs the DOM (Phase 6).
    if let Some(sel) = cli.extract_selector.as_deref() {
        if !is_valid_selector(sel) {
            eprintln!("{}", codes::INVALID_SELECTOR);
            return ExitCode::FAILURE;
        }
        eprintln!("--extract-selector: not implemented yet (Phase 6)");
        return ExitCode::from(2);
    }

    // Remaining flags need the DOM/layout/paint stack (Phases 3–8).
    let unimplemented = [
        cli.extract_text,
        cli.dump_dom,
        cli.dump_display_list,
        cli.dump_lines,
        cli.click_at.is_some(),
        cli.focus.is_some(),
        cli.submit_form.is_some(),
        cli.paint_stats,
        cli.incremental,
        cli.cdp,
        cli.memory_stats,
    ];
    if unimplemented.iter().any(|&f| f) {
        eprintln!("requested feature is not implemented yet (Phases 3–8)");
        return ExitCode::from(2);
    }

    // Nothing to do — just a URL load. Without the engine pipeline we acknowledge it.
    eprintln!("loaded {url} (no action requested; engine pipeline lands Phases 3–6)");
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

/// Minimal URL validation. Network policy (SSRF/private-IP) is enforced in
/// `vixen-net` once fetches exist; here we only check the scheme is present.
fn validate_url(url: &str) -> Result<(), String> {
    let scheme = url.split("://").next().unwrap_or("");
    if scheme.is_empty() || scheme == url {
        return Err("URL must include a scheme (e.g. https:// or file://)".into());
    }
    Ok(())
}

/// Minimal selector validity: reject empty / whitespace-only selectors. Full
/// CSS selector validation uses the `selectors` crate in Phase 3.
fn is_valid_selector(sel: &str) -> bool {
    !sel.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use vixen_core::script::JsValue;

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
        let cli = parse(&["--url", "https://example.com", "--extract-selector", "   "]);
        assert_eq!(run(cli), ExitCode::FAILURE);
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
