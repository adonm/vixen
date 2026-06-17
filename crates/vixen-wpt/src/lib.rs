//! vixen-wpt — WPT harness.
//!
//! Stub at Phase 0 (docs/PLAN.md). The harness consumes
//! `vixen_api::EngineInspector` only; it never depends on engine internals
//! (docs/ARCHITECTURE.md "Boundary rules"). The 13 check types (12 inherited
//! from upstream WPT plus `ref-equivalent`) are defined in docs/SPEC.md.

#![forbid(unsafe_code)]

pub mod placeholder;
