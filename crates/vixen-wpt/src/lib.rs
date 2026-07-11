//! vixen-wpt — WPT harness.
//!
//! Asserts document state against fixture manifests (docs/SPEC.md "WPT
//! harness — check types"). The harness consumes only [`HarnessEngine`] — a
//! vixen-wpt-local trait composing `vixen_api` DTOs — never engine internals
//! (docs/ARCHITECTURE.md "Dependency direction": `vixen-wpt → vixen-api` only).
//!
//! The 15 check types (14 inherited from upstream WPT + `ref-equivalent`,
//! Vixen's addition) are all defined. `ref-equivalent` compares the stable
//! display-list render projection through [`HarnessEngine`]; `visual-hash`
//! hashes RGBA screenshots through adapters with an offscreen renderer and
//! remains skipped for adapters that cannot provide pixels.

#![forbid(unsafe_code)]

pub mod check;
pub mod harness;
pub mod manifest;
pub mod profile;
pub mod visual_hash;

pub use check::{Check, Outcome};
pub use harness::{HarnessEngine, Report, RgbaScreenshot, run_fixture, run_manifest};
pub use manifest::{Fixture, Manifest, ManifestError};
pub use profile::{ProfileError, WPT_REPOSITORY_URL, WptProfile, WptProfileFixture, WptUpstream};
