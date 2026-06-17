//! vixen-wpt — WPT harness.
//!
//! Asserts document state against fixture manifests (docs/SPEC.md "WPT
//! harness — check types"). The harness consumes only [`HarnessEngine`] — a
//! vixen-wpt-local trait composing `vixen_api` DTOs — never engine internals
//! (docs/ARCHITECTURE.md "Boundary rules": `vixen-wpt → vixen-api only`).
//!
//! The 13 check types (12 inherited from upstream WPT + `ref-equivalent`,
//! Vixen's addition) are all defined; the ones that map to the current
//! [`HarnessEngine`] surface are implemented, and the two that need an
//! offscreen renderer (`visual-hash`, `ref-equivalent`) return
//! [`Outcome::Skipped`] until the paint path lands (Phase 5).

#![forbid(unsafe_code)]

pub mod check;
pub mod harness;
pub mod manifest;
pub mod visual_hash;

pub use check::{Check, Outcome};
pub use harness::{HarnessEngine, Report, run_fixture, run_manifest};
pub use manifest::{Fixture, Manifest, ManifestError};
