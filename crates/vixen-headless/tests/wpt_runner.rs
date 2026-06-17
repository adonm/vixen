//! End-to-end WPT runner — drives `fixtures/manifest.json` through
//! [`vixen_wpt`] against a parsed [`vixen_core::doc::Document`] wrapped as a
//! [`HarnessEngine`]. This is the Phase 3 gate (docs/PLAN.md "vixen-wpt runs
//! the CSS fixtures; pass rate recorded as baseline").
//!
//! Lives in `vixen-headless/tests/` (not the lib) because `vixen-wpt` is a
//! dev-dependency: the architecture rule "vixen-wpt → vixen-api only"
//! (docs/ARCHITECTURE.md) keeps the harness crate out of the runtime dep
//! graph, so the engine-side adapter lives here at the integration seam.

use vixen_api::{ElementInfo, EngineDiagnostic, PageSnapshot};
use vixen_core::doc::Document;
use vixen_core::style_dom::Selector;
use vixen_wpt::harness::HarnessEngine;
use vixen_wpt::manifest::Manifest;

/// Wrap a parsed [`Document`] as a [`HarnessEngine`] the WPT harness can drive.
struct DocumentHarnessEngine<'a> {
    doc: &'a Document,
}

impl<'a> DocumentHarnessEngine<'a> {
    fn new(doc: &'a Document) -> Self {
        Self { doc }
    }
}

impl<'a> HarnessEngine for DocumentHarnessEngine<'a> {
    fn snapshot(&self, _vw: u32, _vh: u32) -> PageSnapshot {
        PageSnapshot {
            url: String::new(),
            title: self.doc.title(),
            viewport: (800, 600),
            text_content: self.doc.text_content(),
            element_count: self.doc.element_count(),
        }
    }

    fn query_selector_all(&self, selector: &str) -> Result<Vec<ElementInfo>, String> {
        let parsed = Selector::parse(selector).map_err(|e| e.to_string())?;
        Ok(self
            .doc
            .query_all(&parsed)
            .into_iter()
            .map(|m| vixen_api::ElementInfo {
                node_id: m.node_id,
                tag: m.tag,
                id: m.id,
                classes: m.classes,
                attributes: m.attributes,
                text: m.text,
                bbox: None,
            })
            .collect())
    }

    fn computed_style(&self, _node_id: usize) -> Vec<(String, String)> {
        // Phase 3 step 3 (cascade) lands next; until then the WPT
        // `computed-style` check fails closed against this adapter.
        Vec::new()
    }

    fn diagnostics(&self) -> Vec<EngineDiagnostic> {
        Vec::new()
    }

    fn eval(&self, _expr: &str) -> Result<String, String> {
        Err("eval not available on the read-only Document adapter".into())
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
        let doc = Document::parse(&html).expect("parse fixture");
        let engine = DocumentHarnessEngine::new(&doc);
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
