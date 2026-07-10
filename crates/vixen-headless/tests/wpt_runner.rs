//! End-to-end WPT runner — drives `fixtures/manifest.json` through
//! [`vixen_wpt`] against the production BrowserCore command/query seam.
//!
//! Lives in `vixen-headless/tests/` (not the lib) because `vixen-wpt` is a
//! dev-dependency: the architecture rule "vixen-wpt → vixen-api only"
//! (docs/ARCHITECTURE.md) keeps the harness crate out of the runtime dep
//! graph, so the engine-side adapter lives here at the integration seam.

mod support;

use support::{HarnessBrowser, assert_clean_report, workspace_root};
use vixen_wpt::manifest::Manifest;

#[test]
fn fixtures_manifest_passes_end_to_end() {
    let root = workspace_root();
    let manifest = Manifest::from_path(&root.join("fixtures/manifest.json"))
        .unwrap_or_else(|e| panic!("load manifest: {e}"));

    let browser = HarnessBrowser::new(&root);
    let report = vixen_wpt::run_manifest(&manifest, |url| Box::new(browser.engine_for(url)));
    assert_clean_report(&report);
}
