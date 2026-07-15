//! vixen-wpt — WPT harness.
//!
//! Asserts document state against fixture manifests (docs/SPEC.md "WPT
//! harness — check types"). The harness consumes only [`HarnessEngine`] — a
//! vixen-wpt-local trait composing `vixen_api` DTOs — never engine internals
//! (docs/ARCHITECTURE.md "Dependency direction": `vixen-wpt → vixen-api` only).
//!
//! All 16 manifest check types are defined. Native adapters execute source and
//! runtime checks; `flutter-js-eval`, `layout-box`, `visual-hash`, and
//! `ref-equivalent` remain in the shared schema but are skipped here and executed
//! by the chrome-less Flutter fixture host.

#![forbid(unsafe_code)]

pub mod check;
pub mod harness;
pub mod manifest;
pub mod profile;

pub use check::{Check, Outcome};
pub use harness::{HarnessEngine, Report, run_fixture, run_manifest};
pub use manifest::{Fixture, Manifest, ManifestError};
pub use profile::{ProfileError, WPT_REPOSITORY_URL, WptProfile, WptProfileFixture, WptUpstream};
