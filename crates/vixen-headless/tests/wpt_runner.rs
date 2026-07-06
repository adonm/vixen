//! End-to-end WPT runner — drives `fixtures/manifest.json` through
//! [`vixen_wpt`] against the shared [`vixen_engine::page::Page`] facade wrapped
//! as a [`HarnessEngine`]. This is the Phase 3 gate (docs/PLAN.md "vixen-wpt
//! runs the CSS fixtures; pass rate recorded as baseline").
//!
//! Lives in `vixen-headless/tests/` (not the lib) because `vixen-wpt` is a
//! dev-dependency: the architecture rule "vixen-wpt → vixen-api only"
//! (docs/ARCHITECTURE.md) keeps the harness crate out of the runtime dep
//! graph, so the engine-side adapter lives here at the integration seam.

use vixen_api::{ElementInfo, EngineDiagnostic, PageSnapshot};
use vixen_engine::page::Page;
use vixen_wpt::harness::HarnessEngine;
use vixen_wpt::manifest::Manifest;

/// Wrap the shared engine [`Page`] facade as a [`HarnessEngine`] the WPT
/// harness can drive. This keeps the fixture runner on the same vertical seam
/// as `vixen-headless` instead of growing a parallel document adapter.
struct PageHarnessEngine<'a> {
    page: &'a Page,
}

impl<'a> PageHarnessEngine<'a> {
    fn new(page: &'a Page) -> Self {
        Self { page }
    }
}

impl<'a> HarnessEngine for PageHarnessEngine<'a> {
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

    fn eval(&self, _expr: &str) -> Result<String, String> {
        Err("eval not available on the read-only Page harness adapter".into())
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

    let mut failures = Vec::new();
    let mut total = 0usize;
    let mut passed = 0usize;
    for fixture in &manifest.fixtures {
        let path = root.join(&fixture.url);
        let html = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let page = Page::from_html(fixture.url.clone(), &html).expect("parse fixture");
        let engine = PageHarnessEngine::new(&page);
        for check in &fixture.checks {
            total += 1;
            let outcome = vixen_wpt::check::Check::run(check, &engine);
            match outcome {
                vixen_wpt::check::Outcome::Pass => passed += 1,
                vixen_wpt::check::Outcome::Fail(msg) => {
                    failures.push(format!("{} :: {:?} → FAIL: {}", fixture.url, check, msg))
                }
                vixen_wpt::check::Outcome::Skipped(msg) => {
                    failures.push(format!("{} :: {:?} → SKIP: {}", fixture.url, check, msg))
                }
            }
        }
    }

    assert!(
        failures.is_empty(),
        "WPT fixtures: {passed}/{total} passed.\nFailures:\n{}",
        failures.join("\n")
    );
    eprintln!("WPT fixtures: {passed}/{total} passed");
}
