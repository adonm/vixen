//! Manifest format — the list of fixtures and their checks
//! (docs/SPEC.md "WPT harness"). JSON, designed for `fixtures/manifest.json`.

use serde::{Deserialize, Serialize};

use crate::check::Check;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    #[serde(default)]
    pub fixtures: Vec<Fixture>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Fixture {
    /// Workspace-relative path (or URL) the engine navigates to.
    pub url: String,
    #[serde(default)]
    pub checks: Vec<Check>,
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("manifest IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl Manifest {
    pub fn from_json(s: &str) -> Result<Self, ManifestError> {
        Ok(serde_json::from_str(s)?)
    }

    pub fn from_path(path: &std::path::Path) -> Result<Self, ManifestError> {
        Self::from_json(&std::fs::read_to_string(path)?)
    }

    pub fn to_json(&self) -> Result<String, ManifestError> {
        Ok(serde_json::to_string_pretty(self)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_manifest_round_trips() {
        let m = Manifest::from_json(r#"{"fixtures":[]}"#).unwrap();
        assert!(m.fixtures.is_empty());
        assert_eq!(m, Manifest::from_json(&m.to_json().unwrap()).unwrap());
    }

    #[test]
    fn parses_a_realistic_fixture() {
        let json = r#"{
          "fixtures": [
            {
              "url": "fixtures/css/at-property.html",
              "checks": [
                {"type":"title","expected":"@property"},
                {"type":"selector-count","selector":"[style]","expected":3},
                {"type":"no-critical-diagnostics"}
              ]
            }
          ]
        }"#;
        let m = Manifest::from_json(json).unwrap();
        assert_eq!(m.fixtures.len(), 1);
        assert_eq!(m.fixtures[0].url, "fixtures/css/at-property.html");
        assert_eq!(m.fixtures[0].checks.len(), 3);
    }
}
