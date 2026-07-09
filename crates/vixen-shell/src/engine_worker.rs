//! Per-tab navigation worker for the GTK shell.
//!
//! `vixen_engine::page::Page` is intentionally kept on the GTK main thread
//! because the current DOM representation is `Rc`-backed and therefore not
//! `Send`. This worker owns the slow navigation/fetch/history side and sends
//! loaded HTML back to the tab component, which constructs the visible `Page`
//! beside its `gtk4::GLArea`.

#![forbid(unsafe_code)]

use std::net::IpAddr;
use std::path::Path;

use relm4::{ComponentSender, Worker};
use vixen_api::{EngineDiagnostic, EngineDiagnosticCategory};
use vixen_engine::data_url::parse_data_url;
use vixen_net::{CookieJar, Method, Network};

pub const START_URI: &str = "about:vixen";
const SEARCH_URL_PREFIX: &str = "https://duckduckgo.com/?";
pub const START_HTML: &str = r#"<!doctype html>
<html>
  <head>
    <meta charset="utf-8">
    <title>Vixen</title>
    <style>
      body { margin: 48px; font: 18px sans-serif; color: #f8fafc; background: #0f172a; }
      main { max-width: 760px; }
      h1 { font-size: 40px; margin-bottom: 12px; color: #93c5fd; }
      p { line-height: 1.45; }
      code { color: #fbbf24; }
    </style>
  </head>
  <body>
    <main>
      <h1>Vixen</h1>
      <p>GTK shell vertical is live: tab lifecycle, URL entry, navigation controls, diagnostics, and WebRender in a GLArea.</p>
      <p>Try a <code>file://</code>, <code>data:text/html,...</code>, <code>https://</code>, or <code>about:blank</code> URL.</p>
    </main>
  </body>
</html>"#;
const BLANK_HTML: &str =
    "<!doctype html><html><head><title>Blank</title></head><body></body></html>";

#[derive(Debug)]
pub struct EngineWorker {
    state: NavigationState,
}

#[derive(Debug)]
pub enum EngineCommand {
    Navigate(String),
    Reload,
    Stop,
    Back,
    Forward,
}

#[derive(Debug, Clone)]
pub struct WorkerState {
    pub current_uri: Option<String>,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub is_loading: bool,
    pub progress: f64,
}

#[derive(Debug)]
pub enum EngineEvent {
    Progress(WorkerState),
    Loaded {
        state: WorkerState,
        final_uri: String,
        html: String,
    },
    Failed {
        state: WorkerState,
        attempted_uri: String,
        message: String,
        diagnostics: Vec<EngineDiagnostic>,
    },
    Stopped(WorkerState),
}

impl Worker for EngineWorker {
    type Init = ();
    type Input = EngineCommand;
    type Output = EngineEvent;

    fn init(_init: Self::Init, _sender: ComponentSender<Self>) -> Self {
        Self {
            state: NavigationState::default(),
        }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>) {
        match message {
            EngineCommand::Navigate(input) => {
                self.state.navigate(&input, HistoryMode::Push, sender)
            }
            EngineCommand::Reload => {
                if let Some(uri) = self.state.current_uri.clone() {
                    self.state.navigate(&uri, HistoryMode::Replace, sender);
                }
            }
            EngineCommand::Stop => {
                self.state.is_loading = false;
                self.state.progress = self.state.progress.min(1.0);
                let _ = sender.output(EngineEvent::Stopped(self.state.snapshot()));
            }
            EngineCommand::Back => {
                if self.state.can_go_back() {
                    self.state.history_index -= 1;
                    let uri = self.state.history[self.state.history_index].clone();
                    self.state.navigate(&uri, HistoryMode::Keep, sender);
                }
            }
            EngineCommand::Forward => {
                if self.state.can_go_forward() {
                    self.state.history_index += 1;
                    let uri = self.state.history[self.state.history_index].clone();
                    self.state.navigate(&uri, HistoryMode::Keep, sender);
                }
            }
        }
    }
}

#[derive(Debug, Default)]
struct NavigationState {
    current_uri: Option<String>,
    history: Vec<String>,
    history_index: usize,
    is_loading: bool,
    progress: f64,
}

impl NavigationState {
    fn navigate(
        &mut self,
        input: &str,
        history_mode: HistoryMode,
        sender: ComponentSender<EngineWorker>,
    ) {
        let uri = normalize_address(input);
        self.is_loading = true;
        self.progress = 0.1;
        let _ = sender.output(EngineEvent::Progress(self.snapshot()));

        match load_html_document(&uri) {
            Ok(loaded) => {
                self.current_uri = Some(loaded.final_uri.clone());
                self.record_history(loaded.final_uri.clone(), history_mode);
                self.is_loading = false;
                self.progress = 1.0;
                let _ = sender.output(EngineEvent::Loaded {
                    state: self.snapshot(),
                    final_uri: loaded.final_uri,
                    html: loaded.html,
                });
            }
            Err(message) => {
                self.is_loading = false;
                self.progress = 0.0;
                let diagnostics = vec![EngineDiagnostic::new(
                    EngineDiagnosticCategory::Network,
                    "shell.load",
                    message.clone(),
                )];
                let _ = sender.output(EngineEvent::Failed {
                    state: self.snapshot(),
                    attempted_uri: uri,
                    message,
                    diagnostics,
                });
            }
        }
    }

    fn snapshot(&self) -> WorkerState {
        WorkerState {
            current_uri: self.current_uri.clone(),
            can_go_back: self.can_go_back(),
            can_go_forward: self.can_go_forward(),
            is_loading: self.is_loading,
            progress: self.progress,
        }
    }

    fn can_go_back(&self) -> bool {
        !self.history.is_empty() && self.history_index > 0
    }

    fn can_go_forward(&self) -> bool {
        self.history_index + 1 < self.history.len()
    }

    fn record_history(&mut self, uri: String, mode: HistoryMode) {
        match mode {
            HistoryMode::Push => {
                if self.history.get(self.history_index) == Some(&uri) {
                    return;
                }
                if !self.history.is_empty() {
                    self.history.truncate(self.history_index + 1);
                }
                self.history.push(uri);
                self.history_index = self.history.len().saturating_sub(1);
            }
            HistoryMode::Replace => {
                if self.history.is_empty() {
                    self.history.push(uri);
                    self.history_index = 0;
                } else if let Some(slot) = self.history.get_mut(self.history_index) {
                    *slot = uri;
                }
            }
            HistoryMode::Keep => {}
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum HistoryMode {
    Push,
    Replace,
    Keep,
}

#[derive(Debug)]
struct LoadedDocument {
    final_uri: String,
    html: String,
}

fn load_html_document(uri: &str) -> Result<LoadedDocument, String> {
    if uri.eq_ignore_ascii_case("about:blank") {
        return html_document("about:blank", BLANK_HTML);
    }
    if uri.eq_ignore_ascii_case(START_URI) {
        return html_document(START_URI, START_HTML);
    }
    if uri
        .as_bytes()
        .get(..5)
        .is_some_and(|p| p.eq_ignore_ascii_case(b"data:"))
    {
        let data = parse_data_url(uri).map_err(|e| format!("parse data URL: {e}"))?;
        let html = String::from_utf8_lossy(&data.data).into_owned();
        return html_document(uri, &html);
    }

    let parsed = url::Url::parse(uri).map_err(|e| format!("invalid URL: {e}"))?;
    match parsed.scheme() {
        "file" => {
            let path = parsed
                .to_file_path()
                .map_err(|_| "file:// URL has no local path".to_owned())?;
            let html = std::fs::read_to_string(&path)
                .map_err(|e| format!("read {}: {e}", path.display()))?;
            html_document(parsed.as_str(), &html)
        }
        "http" | "https" => fetch_http_document(parsed),
        scheme => Err(format!(
            "{scheme}: URLs are not supported by the GTK shell loader"
        )),
    }
}

fn fetch_http_document(uri: url::Url) -> Result<LoadedDocument, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("network runtime failed: {e}"))?;
    rt.block_on(async move {
        let mut network =
            Network::with_defaults().map_err(|e| format!("network client failed: {e}"))?;
        let mut jar = CookieJar::default();
        let response = network
            .get_text_with_cookies(&mut jar, &uri, false, Method::Get)
            .await
            .map_err(|e| format!("fetch {uri}: {e}"))?;
        html_document(&response.final_url, &response.body)
    })
}

fn html_document(uri: &str, html: &str) -> Result<LoadedDocument, String> {
    Ok(LoadedDocument {
        final_uri: uri.to_owned(),
        html: html.to_owned(),
    })
}

fn normalize_address(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return "about:blank".to_owned();
    }
    let path = Path::new(trimmed);
    if path.exists()
        && let Ok(abs) = path.canonicalize()
        && let Ok(uri) = url::Url::from_file_path(abs)
    {
        return uri.to_string();
    }
    if is_probable_web_address(trimmed) {
        return format!("https://{trimmed}");
    }
    if has_url_scheme(trimmed) {
        return trimmed.to_owned();
    }
    search_url(trimmed)
}

fn search_url(query: &str) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(SEARCH_URL_PREFIX.to_owned());
    serializer.append_pair("q", query);
    serializer.finish()
}

fn is_probable_web_address(input: &str) -> bool {
    if input.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return false;
    }
    let authority_end = input
        .find(|ch| matches!(ch, '/' | '?' | '#'))
        .unwrap_or(input.len());
    let authority = &input[..authority_end];
    if authority.is_empty() || authority.starts_with('.') || authority.ends_with('.') {
        return false;
    }
    let host_port = authority
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(authority);
    let Some((host, port_ok)) = host_and_optional_port(host_port) else {
        return false;
    };
    port_ok && is_probable_host(host)
}

fn host_and_optional_port(input: &str) -> Option<(&str, bool)> {
    if let Some(rest) = input.strip_prefix('[') {
        let (host, suffix) = rest.split_once(']')?;
        let port_ok = suffix
            .strip_prefix(':')
            .is_none_or(|port| !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()));
        return Some((host, port_ok));
    }
    if let Some((host, port)) = input.rsplit_once(':') {
        return Some((
            host,
            !host.is_empty() && !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()),
        ));
    }
    Some((input, true))
}

fn is_probable_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok() {
        return true;
    }
    host.contains('.')
        && host
            .split('.')
            .all(|label| !label.is_empty() && label.bytes().all(host_label_byte))
}

fn host_label_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-'
}

fn has_url_scheme(input: &str) -> bool {
    let Some((scheme, _)) = input.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_address_keeps_explicit_urls_and_special_pages() {
        assert_eq!(
            normalize_address(" https://example.com/a "),
            "https://example.com/a"
        );
        assert_eq!(
            normalize_address("file:///tmp/page.html"),
            "file:///tmp/page.html"
        );
        assert_eq!(normalize_address("about:blank"), "about:blank");
        assert_eq!(
            normalize_address("data:text/html,<h1>Hello world</h1>"),
            "data:text/html,<h1>Hello world</h1>"
        );
    }

    #[test]
    fn normalize_address_detects_bare_web_addresses() {
        assert_eq!(normalize_address("example.com"), "https://example.com");
        assert_eq!(
            normalize_address("example.com/docs?q=rust"),
            "https://example.com/docs?q=rust"
        );
        assert_eq!(
            normalize_address("localhost:8080"),
            "https://localhost:8080"
        );
        assert_eq!(
            normalize_address("127.0.0.1:8080"),
            "https://127.0.0.1:8080"
        );
    }

    #[test]
    fn normalize_address_turns_search_like_input_into_search_urls() {
        assert_eq!(
            normalize_address("rust browser"),
            "https://duckduckgo.com/?q=rust+browser"
        );
        assert_eq!(
            normalize_address("vixen"),
            "https://duckduckgo.com/?q=vixen"
        );
    }
}
