//! vixen-wpt — WPT harness.
//!
//! Asserts document state against fixture manifests (docs/SPEC.md "WPT
//! harness — check types"). The harness consumes only [`HarnessEngine`] — a
//! vixen-wpt-local trait composing `vixen_api` DTOs — never engine internals
//! (docs/ARCHITECTURE.md "Boundary rules": `vixen-wpt → vixen-api only`).
//!
//! The 15 check types (14 inherited from upstream WPT + `ref-equivalent`,
//! Vixen's addition) are all defined. `ref-equivalent` compares the stable
//! display-list render projection through [`HarnessEngine`]; `visual-hash`
//! remains skipped until the offscreen pixel path lands.

#![forbid(unsafe_code)]

pub mod check;
pub mod harness;
pub mod manifest;
pub mod visual_hash;

pub use check::{Check, Outcome};
pub use harness::{HarnessEngine, Report, run_fixture, run_manifest};
pub use manifest::{Fixture, Manifest, ManifestError};
