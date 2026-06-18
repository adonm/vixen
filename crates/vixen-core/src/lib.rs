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

pub mod angle;
pub mod background_position;
pub mod border_radius;
pub mod box_model;
pub mod box_shadow;
pub mod calc;
pub mod class_list;
pub mod color;
pub mod data_url;
pub mod dataset;
pub mod date_units;
pub mod display_list;
pub mod doc;
pub mod easing;
pub mod engine_error;
pub mod event_path;
pub mod flex_resolve;
pub mod form_submission;
pub mod forms;
pub mod gradient;
pub mod length;
pub mod media_query;
pub mod microsyntax;
pub mod mime;
pub mod ratio;
pub mod resolution;
pub mod responsive_select;
pub mod script;
pub mod source_size;
pub mod srcset;
pub mod stacking_context;
pub mod storage_key;
pub mod style_dom;
pub mod text_codec;
pub mod time;
pub mod transform;
pub mod url_search_params;

// Removed once the first post-Phase-0 module landed; kept out of the build.
