//! The engine seam + the fixture/manifest runners.

use vixen_api::{ElementInfo, EngineDiagnostic, PageSnapshot};

use crate::check::{Check, Outcome};
use crate::manifest::Fixture;

/// The engine surface the harness needs. This is a vixen-wpt-local trait over
/// `vixen_api` DTOs (the real engine implements it; tests use a mock). Per
/// docs/ARCHITECTURE.md the harness never touches engine internals.
pub trait HarnessEngine {
    /// Coarse document snapshot at a viewport.
    fn snapshot(&self, vw: u32, vh: u32) -> PageSnapshot;
    /// All elements matching a CSS selector (engine-side matching; never
    /// parsed here). `Err` ⇒ malformed selector or engine failure.
    fn query_selector_all(&self, selector: &str) -> Result<Vec<ElementInfo>, String>;
    /// Computed (resolved) style for a node id.
    fn computed_style(&self, node_id: usize) -> Vec<(String, String)>;
    /// Recorded diagnostics (docs/SPEC.md "Diagnostics shape").
    fn diagnostics(&self) -> Vec<EngineDiagnostic>;
    /// Evaluate a JS expression, returning its stringified result. `Err` ⇒
    /// JS unavailable or evaluation threw.
    fn eval(&self, expr: &str) -> Result<String, String>;
}

/// Per-fixture results.
#[derive(Debug, Clone)]
pub struct FixtureReport {
    pub url: String,
    pub results: Vec<(Check, Outcome)>,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
}

/// Aggregate report across a whole manifest.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub fixtures: Vec<FixtureReport>,
}

impl Report {
    pub fn is_clean(&self) -> bool {
        self.failed == 0
    }
}

/// Run every check in `fixture` against `engine`.
pub fn run_fixture(fixture: &Fixture, engine: &dyn HarnessEngine) -> FixtureReport {
    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;
    let mut results = Vec::with_capacity(fixture.checks.len());
    for check in &fixture.checks {
        let outcome = check.run(engine);
        match &outcome {
            Outcome::Pass => passed += 1,
            Outcome::Fail(_) => failed += 1,
            Outcome::Skipped(_) => skipped += 1,
        }
        results.push((check.clone(), outcome));
    }
    FixtureReport {
        url: fixture.url.clone(),
        results,
        passed,
        failed,
        skipped,
    }
}

/// Run a whole manifest. `engine_for(url)` produces the engine for each
/// fixture (the caller owns engine construction — e.g. navigation).
pub fn run_manifest<F>(manifest: &crate::manifest::Manifest, mut engine_for: F) -> Report
where
    F: FnMut(&str) -> Box<dyn HarnessEngine>,
{
    let mut report = Report::default();
    for fixture in &manifest.fixtures {
        let engine = engine_for(&fixture.url);
        let fr = run_fixture(fixture, &*engine);
        report.total += fr.results.len();
        report.passed += fr.passed;
        report.failed += fr.failed;
        report.skipped += fr.skipped;
        report.fixtures.push(fr);
    }
    report
}

// ---------------------------------------------------------------------------
// Test support — a mock engine shared by this crate's tests.
// ---------------------------------------------------------------------------
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    pub struct MockEngine {
        pub snapshot: PageSnapshot,
        pub matches: HashMap<String, Vec<ElementInfo>>,
        pub styles: HashMap<usize, Vec<(String, String)>>,
        pub diagnostics: Vec<EngineDiagnostic>,
        pub eval_result: Option<Result<String, String>>,
    }

    impl HarnessEngine for MockEngine {
        fn snapshot(&self, _vw: u32, _vh: u32) -> PageSnapshot {
            self.snapshot.clone()
        }
        fn query_selector_all(&self, selector: &str) -> Result<Vec<ElementInfo>, String> {
            Ok(self.matches.get(selector).cloned().unwrap_or_default())
        }
        fn computed_style(&self, node_id: usize) -> Vec<(String, String)> {
            self.styles.get(&node_id).cloned().unwrap_or_default()
        }
        fn diagnostics(&self) -> Vec<EngineDiagnostic> {
            self.diagnostics.clone()
        }
        fn eval(&self, _expr: &str) -> Result<String, String> {
            self.eval_result
                .clone()
                .unwrap_or(Err("JS not available".into()))
        }
    }
}

#[cfg(test)]
pub(crate) use test_support::MockEngine;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;
    use vixen_api::PageSnapshot;

    fn snap() -> PageSnapshot {
        PageSnapshot {
            url: "https://example.test/".into(),
            title: Some("T".into()),
            viewport: (800, 600),
            text_content: "body text".into(),
            element_count: 4,
        }
    }

    #[test]
    fn run_fixture_classifies_outcomes() {
        let e = MockEngine {
            snapshot: snap(),
            ..Default::default()
        };
        let fixture = Fixture {
            url: "a.html".into(),
            checks: vec![
                Check::Title {
                    expected: "T".into(),
                },
                Check::Title {
                    expected: "X".into(),
                },
                Check::VisualHash {
                    expected: "h".into(),
                },
            ],
        };
        let fr = run_fixture(&fixture, &e);
        assert_eq!(fr.passed, 1);
        assert_eq!(fr.failed, 1);
        assert_eq!(fr.skipped, 1);
    }

    #[test]
    fn run_manifest_aggregates() {
        let e = MockEngine {
            snapshot: snap(),
            ..Default::default()
        };
        let manifest: Manifest = serde_json::from_str(
            r#"{"fixtures":[
                {"url":"a.html","checks":[{"type":"title","expected":"T"},{"type":"min-nodes","min":2}]},
                {"url":"b.html","checks":[{"type":"no-critical-diagnostics"}]}
            ]}"#,
        )
        .unwrap();
        let report = run_manifest(&manifest, |_| {
            Box::new(MockEngine {
                snapshot: snap(),
                ..Default::default()
            })
        });
        let _ = &e; // keep the typed mock example live
        assert_eq!(report.total, 3);
        assert_eq!(report.passed, 3);
        assert_eq!(report.failed, 0);
        assert!(report.is_clean());
    }
}
