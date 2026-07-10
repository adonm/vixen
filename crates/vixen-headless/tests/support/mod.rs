use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vixen_api::{
    BrowserCommand, BrowserCommandResult, BrowserEvent, BrowsingContextId, DocumentId, ElementInfo,
    EngineDiagnostic, NavigationId, NavigationPhase, PageSnapshot,
};
use vixen_engine::browser::{BrowserConfig, EngineBrowserHandle, spawn_browser};
use vixen_wpt::harness::{HarnessEngine, RgbaScreenshot};

/// One production BrowserCore shared by all fixture contexts in a report.
pub struct HarnessBrowser {
    shared: Arc<HarnessBrowserShared>,
}

struct HarnessBrowserShared {
    handle: Mutex<EngineBrowserHandle>,
    root: PathBuf,
    _profile: tempfile::TempDir,
}

impl HarnessBrowser {
    pub fn new(root: &Path) -> Self {
        let profile = tempfile::tempdir().expect("create WPT profile directory");
        let handle = spawn_browser(BrowserConfig::new(profile.path().join("profile.redb")))
            .expect("start WPT BrowserCore");
        Self {
            shared: Arc::new(HarnessBrowserShared {
                handle: Mutex::new(handle),
                root: root.to_path_buf(),
                _profile: profile,
            }),
        }
    }

    pub fn engine_for(&self, fixture_url: &str) -> BrowserHarnessEngine {
        BrowserHarnessEngine::from_fixture(Arc::clone(&self.shared), fixture_url)
    }
}

/// BrowserCore-backed WPT harness adapter for committed manifests and optional
/// external WPT profiles. It never owns a Page or creates an alternate runtime.
pub struct PageHarnessEngine {
    shared: Arc<HarnessBrowserShared>,
    context_id: BrowsingContextId,
    document_id: DocumentId,
    fixture_url: String,
}

pub type BrowserHarnessEngine = PageHarnessEngine;

impl PageHarnessEngine {
    fn from_fixture(shared: Arc<HarnessBrowserShared>, fixture_url: &str) -> Self {
        let path = shared.root.join(fixture_url);
        let html = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let mut handle = shared.handle.lock().expect("WPT browser lock");
        let context_id = match handle
            .dispatch(BrowserCommand::CreateBrowsingContext)
            .expect("create WPT context")
        {
            BrowserCommandResult::BrowsingContextCreated { context_id } => context_id,
            result => panic!("unexpected create-context result: {result:?}"),
        };
        navigate_html_and_wait(&mut handle, context_id, fixture_url.to_owned(), html)
            .expect("navigate WPT context");
        let state = context_state(&mut handle, context_id).expect("read WPT context state");
        drop(handle);
        Self {
            shared,
            context_id,
            document_id: state.document_id,
            fixture_url: fixture_url.to_owned(),
        }
    }

    fn resolve_reference(&self, reference: &str) -> PathBuf {
        let reference_path = Path::new(reference);
        if reference_path.is_absolute() {
            return reference_path.to_path_buf();
        }
        if reference.starts_with("fixtures/") {
            return self.shared.root.join(reference);
        }
        let fixture_path = self.shared.root.join(&self.fixture_url);
        fixture_path
            .parent()
            .unwrap_or(&self.shared.root)
            .join(reference_path)
    }

    fn dispatch(&self, command: BrowserCommand) -> Result<BrowserCommandResult, String> {
        self.shared
            .handle
            .lock()
            .map_err(|_| "WPT browser lock poisoned".to_owned())?
            .dispatch(command)
            .map_err(|error| error.to_string())
    }
}

impl HarnessEngine for PageHarnessEngine {
    fn snapshot(&self, vw: u32, vh: u32) -> PageSnapshot {
        match self
            .dispatch(BrowserCommand::Snapshot {
                context_id: self.context_id,
                document_id: self.document_id,
                viewport: (vw, vh),
            })
            .expect("capture WPT snapshot")
        {
            BrowserCommandResult::Snapshot(snapshot) => snapshot,
            result => panic!("unexpected snapshot result: {result:?}"),
        }
    }

    fn query_selector_all(&self, selector: &str) -> Result<Vec<ElementInfo>, String> {
        match self.dispatch(BrowserCommand::QuerySelectorAll {
            context_id: self.context_id,
            document_id: self.document_id,
            selector: selector.to_owned(),
            viewport: (800, 600),
        })? {
            BrowserCommandResult::SelectorMatches(matches) => Ok(matches),
            result => Err(format!("unexpected selector result: {result:?}")),
        }
    }

    fn computed_style(&self, node_id: usize) -> Vec<(String, String)> {
        match self
            .dispatch(BrowserCommand::ComputedStyle {
                context_id: self.context_id,
                document_id: self.document_id,
                node_id,
                viewport: (800, 600),
            })
            .expect("query WPT computed style")
        {
            BrowserCommandResult::ComputedStyle(style) => style,
            result => panic!("unexpected computed-style result: {result:?}"),
        }
    }

    fn diagnostics(&self) -> Vec<EngineDiagnostic> {
        match self
            .dispatch(BrowserCommand::Diagnostics {
                context_id: self.context_id,
                document_id: self.document_id,
            })
            .expect("query WPT diagnostics")
        {
            BrowserCommandResult::Diagnostics(diagnostics) => diagnostics,
            result => panic!("unexpected diagnostics result: {result:?}"),
        }
    }

    fn eval(&self, expr: &str) -> Result<String, String> {
        let state = {
            let mut handle = self
                .shared
                .handle
                .lock()
                .map_err(|_| "WPT browser lock poisoned".to_owned())?;
            context_state(&mut handle, self.context_id)?
        };
        match self.dispatch(BrowserCommand::Evaluate {
            context_id: self.context_id,
            document_id: state.document_id,
            runtime_context_id: state
                .runtime_context_id
                .ok_or_else(|| "WPT context has no runtime".to_owned())?,
            source: expr.to_owned(),
        })? {
            BrowserCommandResult::Evaluation(value) => Ok(value.to_display()),
            result => Err(format!("unexpected evaluation result: {result:?}")),
        }
    }

    fn display_list(&self, vw: u32, vh: u32) -> Result<String, String> {
        match self.dispatch(BrowserCommand::DisplayListText {
            context_id: self.context_id,
            document_id: self.document_id,
            viewport: (vw, vh),
        })? {
            BrowserCommandResult::DisplayListText(text) => Ok(text),
            result => Err(format!("unexpected display-list result: {result:?}")),
        }
    }

    fn reference_display_list(&self, reference: &str, vw: u32, vh: u32) -> Result<String, String> {
        let path = self.resolve_reference(reference);
        let html = std::fs::read_to_string(&path)
            .map_err(|error| format!("read reference {}: {error}", path.display()))?;
        let url = path.display().to_string();
        let mut handle = self
            .shared
            .handle
            .lock()
            .map_err(|_| "WPT browser lock poisoned".to_owned())?;
        let context_id = create_context(&mut handle)?;
        let result = (|| {
            navigate_html_and_wait(&mut handle, context_id, url, html)?;
            let state = context_state(&mut handle, context_id)?;
            match handle
                .dispatch(BrowserCommand::DisplayListText {
                    context_id,
                    document_id: state.document_id,
                    viewport: (vw, vh),
                })
                .map_err(|error| error.to_string())?
            {
                BrowserCommandResult::DisplayListText(text) => Ok(text),
                result => Err(format!(
                    "unexpected reference display-list result: {result:?}"
                )),
            }
        })();
        let _ = handle.dispatch(BrowserCommand::CloseBrowsingContext { context_id });
        result
    }

    fn screenshot_rgba(&self, vw: u32, vh: u32) -> Result<RgbaScreenshot, String> {
        let viewport = (vw, vh);
        let snapshot = self
            .shared
            .handle
            .lock()
            .map_err(|_| "WPT browser lock poisoned".to_owned())?
            .capture_paint_snapshot(self.context_id, self.document_id, viewport)
            .map_err(|error| error.to_string())?;
        let surface = vixen_headless::surface::SurfacelessSurface::new(viewport)
            .map_err(|e| e.to_string())?;
        let frame =
            vixen_engine::paint::render_commands_to_rgba(&surface, &snapshot.commands, viewport)
                .map_err(|e| e.to_string())?;
        Ok(RgbaScreenshot {
            width: frame.width,
            height: frame.height,
            rgba: frame.rgba,
        })
    }
}

impl Drop for PageHarnessEngine {
    fn drop(&mut self) {
        if let Ok(mut handle) = self.shared.handle.lock() {
            let _ = handle.dispatch(BrowserCommand::CloseBrowsingContext {
                context_id: self.context_id,
            });
        }
    }
}

fn create_context(handle: &mut EngineBrowserHandle) -> Result<BrowsingContextId, String> {
    match handle
        .dispatch(BrowserCommand::CreateBrowsingContext)
        .map_err(|error| error.to_string())?
    {
        BrowserCommandResult::BrowsingContextCreated { context_id } => Ok(context_id),
        result => Err(format!("unexpected create-context result: {result:?}")),
    }
}

fn context_state(
    handle: &mut EngineBrowserHandle,
    context_id: BrowsingContextId,
) -> Result<vixen_api::BrowsingContextState, String> {
    match handle
        .dispatch(BrowserCommand::GetBrowsingContextState { context_id })
        .map_err(|error| error.to_string())?
    {
        BrowserCommandResult::BrowsingContextState(state) => Ok(state),
        result => Err(format!("unexpected context-state result: {result:?}")),
    }
}

fn navigate_html_and_wait(
    handle: &mut EngineBrowserHandle,
    context_id: BrowsingContextId,
    url: String,
    html: String,
) -> Result<(), String> {
    let navigation_id = match handle
        .navigate_html(context_id, url, html)
        .map_err(|error| error.to_string())?
    {
        BrowserCommandResult::NavigationAccepted { navigation_id } => navigation_id,
        result => return Err(format!("unexpected injected-navigation result: {result:?}")),
    };
    wait_for_navigation(handle, context_id, navigation_id)
}

fn wait_for_navigation(
    handle: &mut EngineBrowserHandle,
    context_id: BrowsingContextId,
    navigation_id: NavigationId,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(35);
    let mut committed = false;
    loop {
        let Some(event) = handle
            .wait_next_event(deadline.saturating_duration_since(Instant::now()))
            .map_err(|error| error.to_string())?
        else {
            return Err(format!("timed out waiting for navigation {navigation_id}"));
        };
        match event {
            BrowserEvent::NavigationCommitted {
                context_id: event_context_id,
                navigation_id: event_navigation_id,
                ..
            } if event_context_id == context_id && event_navigation_id == navigation_id => {
                committed = true;
            }
            BrowserEvent::NavigationPhaseChanged {
                context_id: event_context_id,
                navigation_id: event_navigation_id,
                phase: NavigationPhase::Settled,
                ..
            } if event_context_id == context_id && event_navigation_id == navigation_id => {
                return Ok(());
            }
            BrowserEvent::NavigationFailed {
                context_id: event_context_id,
                navigation_id: event_navigation_id,
                error,
                ..
            } if event_context_id == context_id && event_navigation_id == navigation_id => {
                if committed {
                    return Ok(());
                }
                return Err(error.to_string());
            }
            BrowserEvent::NavigationCancelled {
                context_id: event_context_id,
                navigation_id: event_navigation_id,
                reason,
                ..
            } if event_context_id == context_id && event_navigation_id == navigation_id => {
                return Err(format!(
                    "navigation {navigation_id} was cancelled: {reason:?}"
                ));
            }
            _ => {}
        }
    }
}

pub fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

pub fn assert_clean_report(report: &vixen_wpt::harness::Report) {
    assert!(report.is_clean(), "{}", report.detailed_text());
    eprintln!("{}", report.detailed_text());
}

#[allow(dead_code)]
pub fn resolve_workspace_path(path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root().join(path)
    }
}
