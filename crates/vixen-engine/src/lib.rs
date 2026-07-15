//! vixen-engine — engine integration glue.
//!
//! Phase 2 stands up the `deno_core` runtime here (`script.rs`). Phase 3
//! adds HTML parse + Stylo cascade. Flutter owns formatting and paint through
//! the renderer source/commit protocol.

#![deny(unsafe_code)]

pub mod abort;
pub mod browser;
pub mod class_list;
pub mod data_url;
pub mod dataset;
pub mod date_units;
pub mod doc;
pub mod engine_error;
pub mod event_path;
pub mod form_submission;
pub mod forms;
pub mod headers;
pub mod high_res_time;
pub mod history;
pub mod html_serialize;
pub mod length;
pub mod media_query;
pub mod message_port;
pub mod microsyntax;
pub mod mime;
pub mod mutation_observer;
pub mod page;
pub mod range;
mod raster_image;
pub mod ratio;
pub mod resolution;
pub mod responsive_select;
pub mod script;
pub mod source_size;
pub mod srcset;
pub mod storage_key;
pub mod structured_clone;
pub mod style_cascade;
pub mod style_dom;
mod stylesheet;
pub mod text_codec;
pub mod time;
pub mod traversal;
pub mod url_pattern;
pub mod url_search_params;
pub mod viewport_meta;
pub mod whatwg_url;

// Removed once the first post-Phase-0 module landed; kept out of the build.
