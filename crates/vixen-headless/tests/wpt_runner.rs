//! End-to-end WPT runner — drives `fixtures/manifest.json` through
//! [`vixen_wpt`] against the shared [`vixen_engine::page::Page`] facade wrapped
//! as a [`HarnessEngine`]. This is the Phase 3 gate (docs/PLAN.md "vixen-wpt
//! runs the CSS fixtures; pass rate recorded as baseline").
//!
//! Lives in `vixen-headless/tests/` (not the lib) because `vixen-wpt` is a
//! dev-dependency: the architecture rule "vixen-wpt → vixen-api only"
//! (docs/ARCHITECTURE.md) keeps the harness crate out of the runtime dep
//! graph, so the engine-side adapter lives here at the integration seam.

use std::path::{Path, PathBuf};
use vixen_api::{ElementInfo, EngineDiagnostic, PageSnapshot};
use vixen_engine::page::Page;
use vixen_wpt::harness::HarnessEngine;
use vixen_wpt::manifest::Manifest;

/// Wrap the shared engine [`Page`] facade as a [`HarnessEngine`] the WPT
/// harness can drive. This keeps the fixture runner on the same vertical seam
/// as `vixen-headless` instead of growing a parallel document adapter.
struct PageHarnessEngine {
    page: Page,
    root: PathBuf,
    fixture_url: String,
}

impl PageHarnessEngine {
    fn from_fixture(root: &Path, fixture_url: &str) -> Self {
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
        self.page.evaluate_dom_expression(expr).unwrap_or_else(|| {
            Err("eval not available on the read-only Page harness adapter".into())
        })
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
}

fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn fixtures_manifest_passes_end_to_end() {
    let root = workspace_root();
    let manifest = Manifest::from_path(&root.join("fixtures/manifest.json"))
        .unwrap_or_else(|e| panic!("load manifest: {e}"));

    let report = vixen_wpt::run_manifest(&manifest, |url| {
        Box::new(PageHarnessEngine::from_fixture(&root, url))
    });

    let mut failures = Vec::new();
    for fixture in &report.fixtures {
        for (check, outcome) in &fixture.results {
            match outcome {
                vixen_wpt::check::Outcome::Pass => {}
                vixen_wpt::check::Outcome::Fail(msg) => failures.push(format!(
                    "{} [{}:{}] :: {:?} → FAIL: {}",
                    fixture.url,
                    fixture.source.as_str(),
                    fixture.category,
                    check,
                    msg
                )),
                vixen_wpt::check::Outcome::Skipped(msg) => failures.push(format!(
                    "{} [{}:{}] :: {:?} → SKIP: {}",
                    fixture.url,
                    fixture.source.as_str(),
                    fixture.category,
                    check,
                    msg
                )),
            }
        }
    }

    assert!(
        report.is_clean(),
        "{}\nFailures:\n{}",
        report.summary_text(),
        failures.join("\n")
    );
    eprintln!("{}", report.summary_text());
}
