//! External WPT profile support.
//!
//! Profiles are the small, committed contract for large upstream WPT slices:
//! they pin provenance plus the expected Vixen check list while the upstream
//! HTML files live in an ignored checkout such as `.tmp/wpt/`. This keeps broad
//! coverage reproducible without vendoring hundreds of WPT source files into the
//! repository.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::check::Check;
use crate::manifest::{Fixture, FixtureSource, Manifest};

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
    /// Repository URL, normally `https://github.com/web-platform-tests/wpt.git`.
    pub repo: String,
    /// Commit SHA or immutable ref expected in the external checkout.
    pub revision: String,
    /// Optional sparse-checkout directories needed by this profile.
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
}

impl WptProfile {
    pub fn from_json(s: &str) -> Result<Self, ProfileError> {
        Ok(serde_json::from_str(s)?)
    }

    pub fn from_path(path: &Path) -> Result<Self, ProfileError> {
        Self::from_json(&std::fs::read_to_string(path)?)
    }

    pub fn to_json(&self) -> Result<String, ProfileError> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Materialize this profile into the existing manifest runner format using
    /// `wpt_root` as the external checkout root. Paths are validated before
    /// joining so a checked-in profile cannot escape the intended root.
    pub fn to_manifest(&self, wpt_root: &Path) -> Result<Manifest, ProfileError> {
        let fixtures = self
            .fixtures
            .iter()
            .map(|fixture| fixture.to_manifest_fixture(wpt_root))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Manifest { fixtures })
    }
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
    if path.trim().is_empty() || input.is_absolute() {
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
                "revision": "0123456789abcdef",
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
                upstream: None,
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
}
