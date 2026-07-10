//! Optional external WPT profile runner.
//!
//! Set `VIXEN_WPT_PROFILE=fixtures/wpt-profiles/<name>.json` and
//! `VIXEN_WPT_ROOT=.tmp/wpt` to run a committed profile against an ignored,
//! pinned upstream WPT checkout. With no env vars, this test is a no-op so the
//! default host gate remains hermetic.

mod support;

use support::{HarnessBrowser, assert_clean_report, resolve_workspace_path};
use vixen_wpt::WptProfile;

#[test]
fn external_wpt_profile_passes_when_configured() {
    let Ok(profile) = std::env::var("VIXEN_WPT_PROFILE") else {
        eprintln!("skipping external WPT profile; set VIXEN_WPT_PROFILE and VIXEN_WPT_ROOT to run");
        return;
    };
    let wpt_root = std::env::var("VIXEN_WPT_ROOT").unwrap_or_else(|_| ".tmp/wpt".into());

    let profile_path = resolve_workspace_path(&profile);
    let wpt_root = resolve_workspace_path(&wpt_root);
    let profile = WptProfile::from_path(&profile_path)
        .unwrap_or_else(|e| panic!("load WPT profile {}: {e}", profile_path.display()));
    let manifest = profile
        .to_manifest(&wpt_root)
        .unwrap_or_else(|e| panic!("materialize WPT profile manifest: {e}"));

    let browser = HarnessBrowser::new(&wpt_root);
    let report = vixen_wpt::run_manifest(&manifest, |url| Box::new(browser.engine_for(url)));
    assert_clean_report(&report);
}
