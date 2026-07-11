//! Optional external WPT profile runner.
//!
//! Set `VIXEN_WPT_PROFILE=fixtures/wpt-profiles/<name>.json` and
//! `VIXEN_WPT_ROOT=.tmp/wpt` to run a committed profile against an ignored,
//! pinned upstream WPT checkout. With no env vars, this test is a no-op so the
//! default host gate remains hermetic.

mod support;

use std::path::Path;
use std::process::{Command, Output};

use support::{HarnessBrowser, assert_clean_report, resolve_workspace_path};
use vixen_wpt::manifest::FixtureSource;
use vixen_wpt::{ProfileError, WPT_REPOSITORY_URL, WptProfile, WptProfileFixture, WptUpstream};

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
    profile
        .verify_checkout(&wpt_root)
        .unwrap_or_else(|e| panic!("preflight WPT checkout {}: {e}", wpt_root.display()));
    let manifest = profile
        .to_manifest(&wpt_root)
        .unwrap_or_else(|e| panic!("materialize WPT profile manifest: {e}"));

    let browser = HarnessBrowser::new(&wpt_root);
    let report = vixen_wpt::run_manifest(&manifest, |url| Box::new(browser.engine_for(url)));
    assert_clean_report(&report);
}

#[test]
fn exact_clean_git_checkout_passes_preflight() {
    let repo = TestRepo::new();
    let revision = repo.commit("fixture.html", "<!doctype html>");

    profile(&revision).verify_checkout(repo.path()).unwrap();
}

#[test]
fn revision_mismatch_fails_preflight() {
    let repo = TestRepo::new();
    let expected = repo.commit("fixture.html", "first");
    repo.commit("fixture.html", "second");

    let error = profile(&expected).verify_checkout(repo.path()).unwrap_err();
    assert!(matches!(error, ProfileError::RevisionMismatch { .. }));
}

#[test]
fn dirty_checkout_fails_preflight() {
    let repo = TestRepo::new();
    let revision = repo.commit("fixture.html", "clean");
    std::fs::write(repo.path().join("fixture.html"), "dirty").unwrap();

    let error = profile(&revision).verify_checkout(repo.path()).unwrap_err();
    assert!(matches!(error, ProfileError::DirtyCheckout { .. }));
}

#[test]
fn non_git_root_fails_preflight() {
    let root = tempfile::tempdir().unwrap();
    let profile = profile("0123456789abcdef0123456789abcdef01234567");

    let error = profile.verify_checkout(root.path()).unwrap_err();
    assert!(matches!(error, ProfileError::NotGitWorktree { .. }));
}

fn profile(revision: &str) -> WptProfile {
    WptProfile {
        name: "preflight-test".into(),
        description: None,
        upstream: Some(WptUpstream {
            repo: WPT_REPOSITORY_URL.into(),
            revision: revision.into(),
            sparse_paths: vec!["fixture.html".into()],
        }),
        fixtures: vec![WptProfileFixture {
            path: "fixture.html".into(),
            category: None,
            source: FixtureSource::Imported,
            checks: Vec::new(),
        }],
    }
}

struct TestRepo {
    root: tempfile::TempDir,
}

impl TestRepo {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        assert_git_ok(&run_git(root.path(), &["init", "--quiet"]));
        Self { root }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn commit(&self, path: &str, contents: &str) -> String {
        std::fs::write(self.path().join(path), contents).unwrap();
        assert_git_ok(&run_git(self.path(), &["add", "--", path]));
        assert_git_ok(&run_git(
            self.path(),
            &[
                "-c",
                "user.name=Vixen WPT Test",
                "-c",
                "user.email=vixen-wpt@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "test fixture",
            ],
        ));
        let output = run_git(self.path(), &["rev-parse", "HEAD"]);
        assert_git_ok(&output);
        String::from_utf8(output.stdout).unwrap().trim().into()
    }
}

fn run_git(root: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("run git")
}

fn assert_git_ok(output: &Output) {
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
