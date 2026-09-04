//! External WPT profile support.
//!
//! Profiles are the small, committed contract for large upstream WPT slices:
//! they pin provenance plus the expected Vixen check list while the upstream
//! HTML files live in an ignored checkout such as `.tmp/wpt/`. This keeps broad
//! coverage reproducible without vendoring hundreds of WPT source files into the
//! repository.

use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

use serde::{Deserialize, Serialize};

use crate::check::Check;
use crate::manifest::{Fixture, FixtureSource, Manifest};

/// Canonical upstream repository accepted by external WPT profiles.
pub const WPT_REPOSITORY_URL: &str = "https://github.com/web-platform-tests/wpt.git";

/// A curated set of upstream WPT files plus Vixen harness checks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WptProfile {
    /// Stable profile name used in docs/reports.
    pub name: String,
    /// Optional human-readable scope note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Upstream repository provenance for fetching/checking out the fixture set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<WptUpstream>,
    /// Upstream fixture paths, relative to the WPT checkout root.
    #[serde(default)]
    pub fixtures: Vec<WptProfileFixture>,
}

/// Pinned upstream WPT checkout information.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WptUpstream {
    /// Canonical WPT repository URL.
    pub repo: String,
    /// Full, lowercase 40-hex commit expected in the external checkout.
    pub revision: String,
    /// Relative sparse-checkout paths covering every fixture in this profile.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sparse_paths: Vec<String>,
}

/// One upstream WPT test mapped to ordinary Vixen manifest checks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WptProfileFixture {
    /// Path relative to the external WPT checkout root. Absolute paths and `..`
    /// are rejected when materializing a manifest.
    pub path: String,
    /// Report category. Profiles usually set this explicitly because upstream
    /// WPT paths do not live below `fixtures/<category>/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Defaults to `imported`; profile fixtures are upstream WPT by design.
    #[serde(default = "default_profile_source")]
    pub source: FixtureSource,
    #[serde(default)]
    pub checks: Vec<Check>,
}

fn default_profile_source() -> FixtureSource {
    FixtureSource::Imported
}

#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("WPT profile is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("WPT profile IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("WPT profile path must be relative and stay under WPT root: {path}")]
    UnsafePath { path: String },
    #[error("WPT profile with fixtures must declare upstream provenance")]
    MissingUpstream,
    #[error(
        "WPT upstream repository must be exactly https://github.com/web-platform-tests/wpt.git; found: {repo}"
    )]
    InvalidRepository { repo: String },
    #[error(
        "WPT upstream revision must be exactly 40 lowercase hexadecimal characters: {revision}"
    )]
    InvalidRevision { revision: String },
    #[error("WPT sparse path must be relative and stay under WPT root: {path}")]
    UnsafeSparsePath { path: String },
    #[error("WPT fixture path is not covered by upstream.sparse_paths: {path}")]
    FixtureOutsideSparsePaths { path: String },
    #[error("WPT checkout is not a Git worktree rooted at {}", root.display())]
    NotGitWorktree { root: PathBuf },
    #[error("Git command failed while checking WPT checkout ({operation}): {detail}")]
    GitCommand {
        operation: &'static str,
        detail: String,
    },
    #[error("WPT checkout revision mismatch: expected {expected}, found {actual}")]
    RevisionMismatch { expected: String, actual: String },
    #[error("WPT checkout is dirty at {}: {status}", root.display())]
    DirtyCheckout { root: PathBuf, status: String },
}

impl WptProfile {
    pub fn from_json(s: &str) -> Result<Self, ProfileError> {
        let profile: Self = serde_json::from_str(s)?;
        profile.validate()?;
        Ok(profile)
    }

    pub fn from_path(path: &Path) -> Result<Self, ProfileError> {
        Self::from_json(&std::fs::read_to_string(path)?)
    }

    pub fn to_json(&self) -> Result<String, ProfileError> {
        self.validate()?;
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Validate pinned provenance and lexical fixture/sparse-checkout paths.
    pub fn validate(&self) -> Result<(), ProfileError> {
        let upstream = match &self.upstream {
            Some(upstream) => upstream,
            None if self.fixtures.is_empty() => return Ok(()),
            None => return Err(ProfileError::MissingUpstream),
        };

        if upstream.repo != WPT_REPOSITORY_URL {
            return Err(ProfileError::InvalidRepository {
                repo: upstream.repo.clone(),
            });
        }
        if upstream.revision.len() != 40
            || !upstream
                .revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ProfileError::InvalidRevision {
                revision: upstream.revision.clone(),
            });
        }

        let sparse_paths = upstream
            .sparse_paths
            .iter()
            .map(|path| {
                safe_relative_path(path)
                    .map_err(|_| ProfileError::UnsafeSparsePath { path: path.clone() })
            })
            .collect::<Result<Vec<_>, _>>()?;

        for fixture in &self.fixtures {
            let fixture_path = safe_relative_path(&fixture.path)?;
            if !sparse_paths
                .iter()
                .any(|sparse_path| fixture_path.starts_with(sparse_path))
            {
                return Err(ProfileError::FixtureOutsideSparsePaths {
                    path: fixture.path.clone(),
                });
            }
        }

        Ok(())
    }

    /// Verify that `wpt_root` is the exact clean Git checkout pinned by this
    /// profile. Both ordinary repositories (`.git/`) and linked worktrees
    /// (`.git` file) are accepted, but a path nested inside another worktree is
    /// not.
    pub fn verify_checkout(&self, wpt_root: &Path) -> Result<(), ProfileError> {
        self.validate()?;
        let upstream = self
            .upstream
            .as_ref()
            .ok_or(ProfileError::MissingUpstream)?;

        if !wpt_root.join(".git").try_exists()? {
            return Err(ProfileError::NotGitWorktree {
                root: wpt_root.to_path_buf(),
            });
        }

        let inside = git_output(
            wpt_root,
            "detect worktree",
            &["rev-parse", "--is-inside-work-tree"],
        )?;
        if output_text(&inside) != "true" {
            return Err(ProfileError::NotGitWorktree {
                root: wpt_root.to_path_buf(),
            });
        }

        let top_level = git_output(
            wpt_root,
            "resolve worktree root",
            &["rev-parse", "--show-toplevel"],
        )?;
        let actual_root = PathBuf::from(output_text(&top_level));
        if wpt_root.canonicalize()? != actual_root.canonicalize()? {
            return Err(ProfileError::NotGitWorktree {
                root: wpt_root.to_path_buf(),
            });
        }

        let head = git_output(
            wpt_root,
            "resolve HEAD",
            &["rev-parse", "--verify", "HEAD^{commit}"],
        )?;
        let actual_revision = output_text(&head);
        if actual_revision != upstream.revision {
            return Err(ProfileError::RevisionMismatch {
                expected: upstream.revision.clone(),
                actual: actual_revision,
            });
        }

        let status = git_output(
            wpt_root,
            "check worktree status",
            &["status", "--porcelain=v1", "--untracked-files=all"],
        )?;
        let status = output_text(&status);
        if !status.is_empty() {
            return Err(ProfileError::DirtyCheckout {
                root: wpt_root.to_path_buf(),
                status,
            });
        }

        Ok(())
    }

    /// Materialize this profile into the existing manifest runner format using
    /// `wpt_root` as the external checkout root. Paths are validated before
    /// joining so a checked-in profile cannot escape the intended root.
    pub fn to_manifest(&self, wpt_root: &Path) -> Result<Manifest, ProfileError> {
        self.validate()?;
        let fixtures = self
            .fixtures
            .iter()
            .map(|fixture| fixture.to_manifest_fixture(wpt_root))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Manifest { fixtures })
    }
}

fn clear_git_repository_environment(command: &mut Command) {
    for name in [
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
        "GIT_OBJECT_DIRECTORY",
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_IMPLICIT_WORK_TREE",
        "GIT_GRAFT_FILE",
        "GIT_INDEX_FILE",
        "GIT_NO_REPLACE_OBJECTS",
        "GIT_REPLACE_REF_BASE",
        "GIT_PREFIX",
        "GIT_SHALLOW_FILE",
        "GIT_COMMON_DIR",
    ] {
        command.env_remove(name);
    }
}

fn git_output(root: &Path, operation: &'static str, args: &[&str]) -> Result<Output, ProfileError> {
    let mut command = Command::new("git");
    clear_git_repository_environment(&mut command);
    let output = command
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|error| ProfileError::GitCommand {
            operation,
            detail: error.to_string(),
        })?;
    if !output.status.success() {
        let stderr = output_text_bytes(&output.stderr);
        return Err(ProfileError::GitCommand {
            operation,
            detail: if stderr.is_empty() {
                output.status.to_string()
            } else {
                stderr
            },
        });
    }
    Ok(output)
}

fn output_text(output: &Output) -> String {
    output_text_bytes(&output.stdout)
}

fn output_text_bytes(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_owned()
}

impl WptProfileFixture {
    fn to_manifest_fixture(&self, wpt_root: &Path) -> Result<Fixture, ProfileError> {
        let url = resolve_profile_path(wpt_root, &self.path)?;
        Ok(Fixture {
            url: url.to_string_lossy().into_owned(),
            category: self.category.clone(),
            source: self.source,
            checks: self.checks.clone(),
        })
    }
}

fn resolve_profile_path(wpt_root: &Path, relative: &str) -> Result<PathBuf, ProfileError> {
    let relative_path = safe_relative_path(relative)?;
    Ok(wpt_root.join(relative_path))
}

fn safe_relative_path(path: &str) -> Result<PathBuf, ProfileError> {
    let input = Path::new(path);
    if path.trim().is_empty()
        || input.is_absolute()
        || path.contains('\\')
        || path
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(ProfileError::UnsafePath {
            path: path.to_owned(),
        });
    }

    let mut out = PathBuf::new();
    for component in input.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(ProfileError::UnsafePath {
                    path: path.to_owned(),
                });
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Err(ProfileError::UnsafePath {
            path: path.to_owned(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_materializes_imported_manifest() {
        let profile = WptProfile::from_json(
            r#"{
              "name": "layout-smoke",
              "upstream": {
                "repo": "https://github.com/web-platform-tests/wpt.git",
                "revision": "0123456789abcdef0123456789abcdef01234567",
                "sparse_paths": ["css/css-display"]
              },
              "fixtures": [
                {
                  "path": "css/css-display/display-flow-root-001.html",
                  "category": "layout block/inline/position",
                  "checks": [
                    {"type":"title","expected":"display: flow-root"},
                    {"type":"no-critical-diagnostics"}
                  ]
                }
              ]
            }"#,
        )
        .unwrap();

        let manifest = profile
            .to_manifest(Path::new("/workspace/.tmp/wpt"))
            .unwrap();
        assert_eq!(manifest.fixtures.len(), 1);
        let fixture = &manifest.fixtures[0];
        assert_eq!(
            fixture.url,
            "/workspace/.tmp/wpt/css/css-display/display-flow-root-001.html"
        );
        assert_eq!(fixture.category_name(), "layout block/inline/position");
        assert_eq!(fixture.source, FixtureSource::Imported);
        assert_eq!(fixture.checks.len(), 2);
    }

    #[test]
    fn profile_path_rejects_root_escape() {
        for path in ["/abs/test.html", "../test.html", "css/../test.html", ""] {
            let err = WptProfile {
                name: "bad".into(),
                description: None,
                upstream: Some(valid_upstream(vec!["css".into()])),
                fixtures: vec![WptProfileFixture {
                    path: path.into(),
                    category: None,
                    source: FixtureSource::Imported,
                    checks: Vec::new(),
                }],
            }
            .to_manifest(Path::new("/workspace/.tmp/wpt"))
            .unwrap_err();
            assert!(matches!(err, ProfileError::UnsafePath { .. }));
        }
    }

    #[test]
    fn profile_rejects_unpinned_revisions() {
        for revision in [
            "main",
            "0123456789abcdef",
            "0123456789abcdef0123456789abcdef0123456g",
            "0123456789ABCDEF0123456789ABCDEF01234567",
        ] {
            let json = format!(
                r#"{{
                  "name": "bad-revision",
                  "upstream": {{
                    "repo": "{WPT_REPOSITORY_URL}",
                    "revision": "{revision}",
                    "sparse_paths": []
                  }},
                  "fixtures": []
                }}"#
            );
            let error = WptProfile::from_json(&json).unwrap_err();
            assert!(matches!(error, ProfileError::InvalidRevision { .. }));
        }
    }

    #[test]
    fn profile_rejects_unexpected_repository() {
        let mut profile = WptProfile {
            name: "wrong-repository".into(),
            description: None,
            upstream: Some(valid_upstream(Vec::new())),
            fixtures: Vec::new(),
        };
        profile.upstream.as_mut().unwrap().repo = "http://example.test/wpt.git".into();

        let error = profile.validate().unwrap_err();
        assert!(matches!(error, ProfileError::InvalidRepository { .. }));
    }

    #[test]
    fn profile_rejects_fixture_outside_sparse_paths() {
        let profile = WptProfile {
            name: "sparse-mismatch".into(),
            description: None,
            upstream: Some(valid_upstream(vec!["css/css-grid".into()])),
            fixtures: vec![WptProfileFixture {
                path: "css/css-display/display-flow-root-001.html".into(),
                category: None,
                source: FixtureSource::Imported,
                checks: Vec::new(),
            }],
        };

        let error = profile.to_manifest(Path::new("/wpt")).unwrap_err();
        assert!(matches!(
            error,
            ProfileError::FixtureOutsideSparsePaths { .. }
        ));
    }

    #[test]
    fn sparse_paths_use_component_boundaries() {
        let profile = WptProfile {
            name: "sparse-prefix".into(),
            description: None,
            upstream: Some(valid_upstream(vec!["css/css-display".into()])),
            fixtures: vec![WptProfileFixture {
                path: "css/css-display-other/test.html".into(),
                category: None,
                source: FixtureSource::Imported,
                checks: Vec::new(),
            }],
        };

        assert!(matches!(
            profile.validate().unwrap_err(),
            ProfileError::FixtureOutsideSparsePaths { .. }
        ));
    }

    #[test]
    fn profile_round_trips() {
        let profile = WptProfile {
            name: "empty".into(),
            description: Some("no fixtures yet".into()),
            upstream: None,
            fixtures: Vec::new(),
        };
        let json = profile.to_json().unwrap();
        assert_eq!(WptProfile::from_json(&json).unwrap(), profile);
    }

    #[test]
    fn committed_layout_profile_parses() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let profile = WptProfile::from_path(
            &workspace.join("fixtures/wpt-profiles/layout-block-inline-position.json"),
        )
        .unwrap();
        assert_eq!(profile.name, "layout-block-inline-position");
        let manifest = profile.to_manifest(Path::new("/wpt")).unwrap();
        assert_eq!(manifest.fixtures.len(), 1);
        assert!(
            manifest.fixtures[0]
                .url
                .ends_with("display-flow-root-001.html")
        );
    }

    fn valid_upstream(sparse_paths: Vec<String>) -> WptUpstream {
        WptUpstream {
            repo: WPT_REPOSITORY_URL.into(),
            revision: "0123456789abcdef0123456789abcdef01234567".into(),
            sparse_paths,
        }
    }
}
