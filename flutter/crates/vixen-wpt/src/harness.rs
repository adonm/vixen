//! The engine seam + the fixture/manifest runners.

use std::collections::BTreeMap;

use vixen_api::{ElementInfo, EngineDiagnostic, PageSnapshot};

use crate::check::{Check, Outcome};
use crate::manifest::{Fixture, FixtureSource};

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
    pub category: String,
    pub source: FixtureSource,
    pub results: Vec<(Check, Outcome)>,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
}

/// Aggregate report across a whole manifest.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub fixtures_run: usize,
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub by_category: BTreeMap<String, ReportSummary>,
    pub by_source: BTreeMap<String, ReportSummary>,
    pub by_source_category: BTreeMap<String, BTreeMap<String, ReportSummary>>,
    pub fixtures: Vec<FixtureReport>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReportSummary {
    pub fixtures: usize,
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
}

impl ReportSummary {
    pub fn pass_rate(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.passed as f64 / self.total as f64
        }
    }

    fn add_fixture(&mut self, fixture: &FixtureReport) {
        self.fixtures += 1;
        self.total += fixture.results.len();
        self.passed += fixture.passed;
        self.failed += fixture.failed;
        self.skipped += fixture.skipped;
    }
}

impl Report {
    pub fn is_clean(&self) -> bool {
        self.failed == 0 && self.skipped == 0
    }

    pub fn pass_rate(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.passed as f64 / self.total as f64
        }
    }

    pub fn summary_text(&self) -> String {
        let mut out = String::new();
        out.push_str("# wpt-report\n");
        out.push_str(&format!(
            "overall fixtures={} checks={} passed={} failed={} skipped={} pass-rate={:.1}%\n",
            self.fixtures_run,
            self.total,
            self.passed,
            self.failed,
            self.skipped,
            self.pass_rate() * 100.0
        ));
        for (source, summary) in &self.by_source {
            out.push_str(&format!(
                "source {source}: fixtures={} checks={} passed={} failed={} skipped={} pass-rate={:.1}%\n",
                summary.fixtures,
                summary.total,
                summary.passed,
                summary.failed,
                summary.skipped,
                summary.pass_rate() * 100.0
            ));
        }
        for (category, summary) in &self.by_category {
            out.push_str(&format!(
                "category {category}: fixtures={} checks={} passed={} failed={} skipped={} pass-rate={:.1}%\n",
                summary.fixtures,
                summary.total,
                summary.passed,
                summary.failed,
                summary.skipped,
                summary.pass_rate() * 100.0
            ));
        }
        for (source, categories) in &self.by_source_category {
            for (category, summary) in categories {
                out.push_str(&format!(
                    "source-category {source} {category}: fixtures={} checks={} passed={} failed={} skipped={} pass-rate={:.1}%\n",
                    summary.fixtures,
                    summary.total,
                    summary.passed,
                    summary.failed,
                    summary.skipped,
                    summary.pass_rate() * 100.0
                ));
            }
        }
        out
    }

    /// Text-first report with stable aggregate lines followed by actionable
    /// failing/skipped check details. Intended for humans and automation logs;
    /// clean reports are identical to [`Self::summary_text`].
    pub fn detailed_text(&self) -> String {
        let mut out = self.summary_text();
        let mut wrote_header = false;
        for fixture in &self.fixtures {
            if fixture.failed == 0 && fixture.skipped == 0 {
                continue;
            }
            if !wrote_header {
                out.push_str("failures:\n");
                wrote_header = true;
            }
            out.push_str(&format!(
                "fixture {} [{}:{}] checks={} passed={} failed={} skipped={}\n",
                fixture.url,
                fixture.source.as_str(),
                fixture.category,
                fixture.results.len(),
                fixture.passed,
                fixture.failed,
                fixture.skipped,
            ));
            for (check, outcome) in &fixture.results {
                match outcome {
                    Outcome::Pass => {}
                    Outcome::Fail(message) => {
                        out.push_str(&format!("  FAIL {:?}: {}\n", check, message));
                    }
                    Outcome::Skipped(message) => {
                        out.push_str(&format!("  SKIP {:?}: {}\n", check, message));
                    }
                }
            }
        }
        out
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
        category: fixture.category_name(),
        source: fixture.source,
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
        report.fixtures_run += 1;
        report.total += fr.results.len();
        report.passed += fr.passed;
        report.failed += fr.failed;
        report.skipped += fr.skipped;
        report
            .by_category
            .entry(fr.category.clone())
            .or_default()
            .add_fixture(&fr);
        report
            .by_source
            .entry(fr.source.as_str().to_owned())
            .or_default()
            .add_fixture(&fr);
        report
            .by_source_category
            .entry(fr.source.as_str().to_owned())
            .or_default()
            .entry(fr.category.clone())
            .or_default()
            .add_fixture(&fr);
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
            root_scroll: (0.0, 0.0),
            root_scroll_max: (0.0, 0.0),
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
            category: None,
            source: FixtureSource::Local,
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
        assert_eq!(fr.category, "uncategorized");
        assert_eq!(fr.source, FixtureSource::Local);
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
        assert_eq!(report.fixtures_run, 2);
        assert_eq!(report.total, 3);
        assert_eq!(report.passed, 3);
        assert_eq!(report.failed, 0);
        assert_eq!(report.skipped, 0);
        assert!(report.is_clean());
        assert_eq!(report.by_source["local"].fixtures, 2);
        assert_eq!(report.by_category["uncategorized"].fixtures, 2);
        assert_eq!(
            report.by_source_category["local"]["uncategorized"].fixtures,
            2
        );
        assert!(
            report
                .summary_text()
                .contains("overall fixtures=2 checks=3")
        );
        assert!(
            report
                .summary_text()
                .contains("source-category local uncategorized: fixtures=2 checks=3")
        );
    }

    #[test]
    fn skipped_checks_make_report_unclean() {
        let manifest: Manifest = serde_json::from_str(
            r#"{"fixtures":[{"url":"fixtures/dom/a.html","checks":[{"type":"visual-hash","expected":"h"}]}]}"#,
        )
        .unwrap();
        let report = run_manifest(&manifest, |_| {
            Box::new(MockEngine {
                snapshot: snap(),
                ..Default::default()
            })
        });
        assert_eq!(report.failed, 0);
        assert_eq!(report.skipped, 1);
        assert!(!report.is_clean());
    }

    #[test]
    fn detailed_text_includes_actionable_unclean_checks() {
        let manifest: Manifest = serde_json::from_str(
            r#"{"fixtures":[{"url":"fixtures/dom/a.html","category":"dom","checks":[{"type":"title","expected":"X"},{"type":"visual-hash","expected":"h"}]}]}"#,
        )
        .unwrap();
        let report = run_manifest(&manifest, |_| {
            Box::new(MockEngine {
                snapshot: snap(),
                ..Default::default()
            })
        });
        let text = report.detailed_text();

        assert!(text.contains("# wpt-report"));
        assert!(text.contains("failures:"));
        assert!(text.contains("fixture fixtures/dom/a.html [local:dom]"));
        assert!(text.contains("FAIL Title"));
        assert!(text.contains("SKIP VisualHash"));
    }
}
