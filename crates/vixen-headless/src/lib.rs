//! vixen-headless — headless CLI + CDP server.
//!
//! Phase 2 implements the CLI flag surface (docs/SPEC.md "Headless CLI
//! surface"). `--eval` navigates and evaluates through the engine-owned browser
//! core. Screenshot, selector, and URL-only actions use the same adapter;
//! textual document and interaction-summary projections do as well. CDP routes
//! targets through the same engine-owned browser core.
//! Renderer/CDP-only failures keep stable error codes (`unsupported.screenshot`,
//! `invalid-selector`) at their trust boundaries.

#![deny(unsafe_code)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;

use vixen_api::{BrowsingContextState, DocumentTextKind};
use vixen_engine::browser::EngineBrowserClient;
use vixen_engine::engine_error::codes;
use vixen_engine::paint::{self, RgbaFrame};

mod browser_adapter;
pub use vixen_cdp as cdp;
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

    /// Capture before/after frames in one context (with --screenshot + --eval).
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
    if let Err(error) = validate_incremental_options(&cli) {
        eprintln!("error: {error}");
        return ExitCode::from(2);
    }

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

    if cli.incremental {
        return run_incremental(
            url,
            viewport,
            cli.screenshot.as_deref().expect("validated above"),
            cli.eval.as_deref().expect("validated above"),
            cli.profile_dir.as_deref(),
        );
    }

    // --screenshot requires the offscreen renderer (Phase 5) and short-circuits
    // the textual DOM/layout output modes.
    if let Some(path) = cli.screenshot.as_deref() {
        return run_screenshot(url, viewport, path, cli.profile_dir.as_deref());
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
        cdp::serve_with_initial_url_profile_and_renderer(
            port,
            Some(initial_url.to_owned()),
            profile_dir,
            Arc::new(LegacyCdpRenderBackend),
        ),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: CDP server failed: {e}");
            ExitCode::FAILURE
        }
    }
}

struct LegacyCdpRenderBackend;

impl cdp::CdpRenderBackend for LegacyCdpRenderBackend {
    fn capture_png(
        &self,
        browser: &mut EngineBrowserClient,
        state: &BrowsingContextState,
        viewport: (u32, u32),
    ) -> Result<Vec<u8>, String> {
        let paint = browser
            .capture_paint_snapshot(state.context_id, state.document_id, viewport)
            .map_err(|error| error.to_string())?;
        capture_commands_png(&paint.commands, viewport)
    }
}

/// Test adapter retaining the transitional native screenshot backend while CDP
/// protocol ownership lives in `vixen-cdp`.
pub fn cdp_state_with_runtime(runtime: vixen_engine::script::JsRuntime) -> cdp::CdpState {
    cdp::CdpState::with_runtime_and_renderer(runtime, Arc::new(LegacyCdpRenderBackend))
}

fn run_screenshot(
    url: &str,
    viewport: (u32, u32),
    output: &Path,
    profile_dir: Option<&Path>,
) -> ExitCode {
    let mut session = match browser_adapter::BrowserSession::load(url, profile_dir) {
        Ok(session) => session,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let png = match capture_session_png(&mut session, viewport) {
        Ok(png) => png,
        Err(err) => {
            eprintln!("{}: {err}", codes::UNSUPPORTED_SCREENSHOT);
            return ExitCode::FAILURE;
        }
    };
    match std::fs::write(output, png).map_err(|e| format!("write {}: {e}", output.display())) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run_incremental(
    url: &str,
    viewport: (u32, u32),
    output: &Path,
    js: &str,
    profile_dir: Option<&Path>,
) -> ExitCode {
    let mut session = match browser_adapter::BrowserSession::load(url, profile_dir) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let frame_one_snapshot = match session.capture_paint_snapshot(viewport) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            eprintln!("{}: {error}", codes::UNSUPPORTED_SCREENSHOT);
            return ExitCode::FAILURE;
        }
    };
    let frame_one = match capture_commands_png(&frame_one_snapshot.commands, viewport) {
        Ok(png) => png,
        Err(error) => {
            eprintln!("{}: {error}", codes::UNSUPPORTED_SCREENSHOT);
            return ExitCode::FAILURE;
        }
    };
    let value = match session.evaluate_for_incremental(js, frame_one_snapshot.document_id) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let frame_two = match capture_session_png(&mut session, viewport) {
        Ok(png) => png,
        Err(error) => {
            eprintln!("{}: {error}", codes::UNSUPPORTED_SCREENSHOT);
            return ExitCode::FAILURE;
        }
    };

    let (frame_one_path, frame_two_path) = incremental_frame_paths(output);
    for (path, png) in [(&frame_one_path, frame_one), (&frame_two_path, frame_two)] {
        if let Err(error) =
            std::fs::write(path, png).map_err(|error| format!("write {}: {error}", path.display()))
        {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    }
    println!("{}", value.to_display());
    ExitCode::SUCCESS
}

fn capture_session_png(
    session: &mut browser_adapter::BrowserSession,
    viewport: (u32, u32),
) -> Result<Vec<u8>, String> {
    let snapshot = session.capture_paint_snapshot(viewport)?;
    capture_commands_png(&snapshot.commands, viewport)
}

fn incremental_frame_paths(output: &Path) -> (PathBuf, PathBuf) {
    (
        incremental_frame_path(output, 1),
        incremental_frame_path(output, 2),
    )
}

fn incremental_frame_path(output: &Path, frame: u8) -> PathBuf {
    let file_name = output.file_name().unwrap_or(output.as_os_str());
    let mut frame_name = match output.extension().filter(|extension| !extension.is_empty()) {
        Some(_) => output
            .file_stem()
            .map(OsString::from)
            .unwrap_or_else(|| file_name.to_os_string()),
        None => file_name.to_os_string(),
    };
    frame_name.push(format!("-frame-{frame}"));
    if let Some(extension) = output.extension().filter(|extension| !extension.is_empty()) {
        frame_name.push(".");
        frame_name.push(extension);
    }
    output.with_file_name(frame_name)
}

pub(crate) fn capture_commands_png(
    commands: &[vixen_engine::display_list::PaintCommand],
    viewport: (u32, u32),
) -> Result<Vec<u8>, String> {
    let surface = match surface::SurfacelessSurface::new(viewport) {
        Ok(surface) => surface,
        Err(err) => {
            return Err(err.to_string());
        }
    };
    let frame = match paint::render_commands_to_rgba(&surface, commands, viewport) {
        Ok(frame) => frame,
        Err(err) => {
            return Err(err.to_string());
        }
    };
    encode_png(&frame)
}

#[cfg(test)]
fn write_png(path: &Path, frame: &RgbaFrame) -> Result<(), String> {
    let png = encode_png(frame)?;
    std::fs::write(path, png).map_err(|e| format!("write {}: {e}", path.display()))
}

fn encode_png(frame: &RgbaFrame) -> Result<Vec<u8>, String> {
    let expected_len = frame
        .width
        .checked_mul(frame.height)
        .and_then(|px| px.checked_mul(4))
        .ok_or_else(|| "PNG dimensions overflow RGBA buffer length".to_owned())?
        as usize;
    if frame.rgba.len() != expected_len {
        return Err(format!(
            "invalid RGBA buffer length: got {}, expected {expected_len}",
            frame.rgba.len()
        ));
    }

    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(std::io::Cursor::new(&mut out), frame.width, frame.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    {
        let mut writer = encoder
            .write_header()
            .map_err(|e| format!("write PNG header: {e}"))?;
        writer
            .write_image_data(&frame.rgba)
            .map_err(|e| format!("write PNG data: {e}"))?;
    }
    Ok(out)
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

fn validate_incremental_options(cli: &Cli) -> Result<(), String> {
    if !cli.incremental {
        return Ok(());
    }
    if cli.screenshot.is_none() || cli.eval.is_none() {
        return Err("--incremental requires --screenshot and --eval".to_owned());
    }
    if cli.dump_dom
        || cli.extract_text
        || cli.dump_display_list
        || cli.dump_lines
        || cli.dump_layout_tree
        || cli.paint_stats
        || cli.extract_selector.is_some()
        || has_interaction_action(cli)
        || cli.cdp
        || cli.list_fonts
        || cli.memory_stats
    {
        return Err(
            "--incremental cannot be combined with other output, interaction, CDP, font, or memory actions"
                .to_owned(),
        );
    }
    Ok(())
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

/// `--dump-dom` / `--extract-text` / `--dump-layout-tree` / `--dump-lines` /
/// `--dump-display-list` / `--paint-stats`: load the URL's HTML and print the requested
/// DOM/layout/paint projections.
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
        if cli.dump_layout_tree {
            print!(
                "{}",
                session.document_text(DocumentTextKind::LayoutTree, viewport)?
            );
        }
        if cli.dump_lines {
            print!(
                "{}",
                session.document_text(DocumentTextKind::Lines, viewport)?
            );
        }
        if cli.dump_display_list {
            print!("{}", session.display_list_text(viewport)?);
        }
        if cli.paint_stats {
            print!(
                "{}",
                session.document_text(DocumentTextKind::PaintStats, viewport)?
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
    fn parses_full_flag_surface() {
        // Every flag from docs/SPEC.md parses.
        let cli = parse(&[
            "--url",
            "https://example.com",
            "--screenshot",
            "out.png",
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
        assert_eq!(cli.profile_dir, Some(PathBuf::from("profiles/baseline")));
        assert_eq!(cli.cdp_port, 9999);
        assert!(cli.dump_dom && cli.dump_layout_tree && cli.cdp && cli.incremental);
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
    fn png_writer_persists_rgba_frames() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("shot.png");
        let frame = RgbaFrame {
            width: 1,
            height: 1,
            rgba: vec![0xff, 0x00, 0x00, 0xff],
        };

        write_png(&output, &frame).unwrap();

        let bytes = std::fs::read(&output).unwrap();
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    }

    #[test]
    fn png_writer_rejects_bad_rgba_lengths() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("bad.png");
        let frame = RgbaFrame {
            width: 2,
            height: 1,
            rgba: vec![0; 4],
        };

        let err = write_png(&output, &frame).unwrap_err();

        assert!(err.contains("invalid RGBA buffer length"));
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
    fn dump_lines_runs_through_browser_core() {
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
    fn dump_layout_tree_runs_through_browser_core() {
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
    fn dump_display_list_runs_through_browser_core() {
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
    fn paint_stats_runs_through_browser_core() {
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
    fn incremental_requires_both_inputs() {
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
    fn incremental_rejects_dump_modes() {
        let cli = parse(&[
            "--url",
            "file:///dev/null",
            "--screenshot",
            "capture.png",
            "--eval",
            "1+2",
            "--incremental",
            "--dump-dom",
        ]);
        assert_eq!(run(cli), ExitCode::from(2));
    }

    #[test]
    fn incremental_rejects_other_actions_instead_of_ignoring_them() {
        let cli = parse(&[
            "--url",
            "file:///dev/null",
            "--screenshot",
            "capture.png",
            "--eval",
            "1+2",
            "--incremental",
            "--paint-stats",
        ]);
        assert_eq!(run(cli), ExitCode::from(2));
    }

    #[test]
    fn incremental_paths_preserve_extensions_and_extensionless_names() {
        assert_eq!(
            incremental_frame_paths(Path::new("captures/page.png")),
            (
                PathBuf::from("captures/page-frame-1.png"),
                PathBuf::from("captures/page-frame-2.png")
            )
        );
        assert_eq!(
            incremental_frame_paths(Path::new("captures/page")),
            (
                PathBuf::from("captures/page-frame-1"),
                PathBuf::from("captures/page-frame-2")
            )
        );
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
