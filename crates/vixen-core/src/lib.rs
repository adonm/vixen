//! vixen-core — engine integration glue.
//!
//! Phase 2 stands up the SpiderMonkey runtime here (`script.rs`). Phase 3
//! adds HTML parse + Stylo cascade, Phase 4 layout, Phase 5 the single
//! WebRender paint path consuming `vixen_api::GlContext`.
//!
//! `unsafe` is confined to `script` — the SpiderMonkey FFI (mozjs) is C, and
//! GC rooting is enforced via mozjs's `rooted!` macro. Every other module
//! stays `forbid(unsafe_code)`. The crate uses `deny(unsafe_code)` so the
//! boundary is explicit and locally allowed.

#![deny(unsafe_code)]

pub mod doc;
pub mod engine_error;
pub mod forms;
pub mod script;
pub mod style_dom;

// Removed once the first post-Phase-0 module landed; kept out of the build.
