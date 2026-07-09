use std::path::{Path, PathBuf};

use vixen_api::{ElementInfo, EngineDiagnostic, PageSnapshot};
use vixen_engine::page::Page;
use vixen_engine::script::JsRuntime;
use vixen_wpt::harness::{HarnessEngine, RgbaScreenshot};

/// Shared Page-backed WPT harness adapter for committed manifests and optional
/// external WPT profiles.
pub struct PageHarnessEngine {
    page: Page,
    root: PathBuf,
    fixture_url: String,
}

impl PageHarnessEngine {
    pub fn from_fixture(root: &Path, fixture_url: &str) -> Self {
        let path = root.join(fixture_url);
        let html = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let page = Page::from_html(fixture_url.to_owned(), &html).expect("parse fixture");
        Self {
            page,
            root: root.to_path_buf(),
            fixture_url: fixture_url.to_owned(),
        }
    }

    fn resolve_reference(&self, reference: &str) -> PathBuf {
        let reference_path = Path::new(reference);
        if reference_path.is_absolute() {
            return reference_path.to_path_buf();
        }
        if reference.starts_with("fixtures/") {
            return self.root.join(reference);
        }
        let fixture_path = self.root.join(&self.fixture_url);
        fixture_path
            .parent()
            .unwrap_or(&self.root)
            .join(reference_path)
    }
}

impl HarnessEngine for PageHarnessEngine {
    fn snapshot(&self, vw: u32, vh: u32) -> PageSnapshot {
        self.page.snapshot((vw, vh))
    }

    fn query_selector_all(&self, selector: &str) -> Result<Vec<ElementInfo>, String> {
        self.page.query_selector_all(selector)
    }

    fn computed_style(&self, node_id: usize) -> Vec<(String, String)> {
        self.page.computed_style(node_id)
    }

    fn diagnostics(&self) -> Vec<EngineDiagnostic> {
        self.page.diagnostics()
    }

    fn eval(&self, expr: &str) -> Result<String, String> {
        match JsRuntime::new().and_then(|mut runtime| runtime.evaluate_with_page(expr, &self.page))
        {
            Ok(value) => Ok(value.to_display()),
            Err(runtime_error) => self
                .page
                .evaluate_dom_expression(expr)
                .unwrap_or_else(|| Err(runtime_error.to_string())),
        }
    }

    fn display_list(&self, vw: u32, vh: u32) -> Result<String, String> {
        Ok(self.page.dump_display_list((vw, vh)))
    }

    fn reference_display_list(&self, reference: &str, vw: u32, vh: u32) -> Result<String, String> {
        let path = self.resolve_reference(reference);
        let html = std::fs::read_to_string(&path)
            .map_err(|e| format!("read reference {}: {e}", path.display()))?;
        let page = Page::from_html(path.display().to_string(), &html)
            .map_err(|e| format!("parse reference {}: {e}", path.display()))?;
        Ok(page.dump_display_list((vw, vh)))
    }

    fn screenshot_rgba(&self, vw: u32, vh: u32) -> Result<RgbaScreenshot, String> {
        let viewport = (vw, vh);
        let surface = vixen_headless::surface::SurfacelessSurface::new(viewport)
            .map_err(|e| e.to_string())?;
        let frame = vixen_engine::paint::render_commands_to_rgba(
            &surface,
            &self.page.display_list(viewport),
            viewport,
        )
        .map_err(|e| e.to_string())?;
        Ok(RgbaScreenshot {
            width: frame.width,
            height: frame.height,
            rgba: frame.rgba,
        })
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
