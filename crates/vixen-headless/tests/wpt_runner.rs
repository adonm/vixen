//! Text/runtime WPT runner — drives the non-rendered checks in
//! `fixtures/manifest.json` through the production BrowserCore command/query
//! seam. R5 routes layout, pixels, and references through the release Flutter
//! host in `just flutter-fixture-manifest`.
//!
//! Lives in `vixen-headless/tests/` (not the lib) because `vixen-wpt` is a
//! dev-dependency: the architecture rule "vixen-wpt → vixen-api only"
//! (docs/ARCHITECTURE.md) keeps the harness crate out of the runtime dep
//! graph, so the engine-side adapter lives here at the integration seam.

mod support;

use support::{HarnessBrowser, assert_clean_report, workspace_root};
use vixen_wpt::check::Check;
use vixen_wpt::manifest::Manifest;

#[test]
fn fixtures_manifest_text_checks_pass_end_to_end() {
    let root = workspace_root();
    let mut manifest = Manifest::from_path(&root.join("fixtures/manifest.json"))
        .unwrap_or_else(|e| panic!("load manifest: {e}"));
    for fixture in &mut manifest.fixtures {
        fixture.checks.retain(|check| {
            !matches!(
                check,
                Check::LayoutBox { .. }
                    | Check::VisualHash { .. }
                    | Check::RefEquivalent { .. }
                    | Check::FlutterJsEval { .. }
            )
        });
    }

    let browser = HarnessBrowser::new(&root);
    let report = vixen_wpt::run_manifest(&manifest, |url| Box::new(browser.engine_for(url)));
    assert_eq!(report.total, 1_868);
    assert_clean_report(&report);
}
