//! Bounded renderer mutation, commit, and query protocol DTOs.
//!
//! These types cross a content-controlled native/Dart boundary. Limits are
//! declared before payload details, validation happens before state changes,
//! and every top-level exchange names protocol version and exact generations.

mod broker;
mod commit;
mod source;

pub use broker::*;
pub use commit::*;
pub use source::*;

use std::fmt;

use crate::RenderHandleId;

/// Initial version of the renderer mutation/commit protocol.
pub const RENDER_PROTOCOL_VERSION: u16 = 1;

// Keep protocol limits centralized. A bridge may impose a smaller transport
// limit, but it must never accept more than these model limits.
pub const RENDER_MAX_NODES: usize = 16_384;
pub const RENDER_MAX_TREE_DEPTH: u16 = 256;
pub const RENDER_MAX_MUTATIONS: usize = 4_096;
pub const RENDER_MAX_RESOURCES: usize = 512;
pub const RENDER_MAX_RESOURCE_BYTES: usize = 16 * 1024 * 1024;
pub const RENDER_MAX_TOTAL_RESOURCE_BYTES: usize = 64 * 1024 * 1024;
pub const RENDER_MAX_CAPTURE_BYTES: usize = 65 * 1024 * 1024;
pub const RENDER_MAX_STRING_BYTES: usize = 64 * 1024;
pub const RENDER_MAX_TOTAL_STRING_BYTES: usize = 4 * 1024 * 1024;
pub const RENDER_MAX_STYLES_PER_NODE: usize = 512;
pub const RENDER_MAX_RESOURCES_PER_NODE: usize = 32;
pub const RENDER_MAX_SEMANTIC_ACTIONS_PER_NODE: usize = 32;
pub const RENDER_MAX_SEMANTIC_VALUE_BYTES: usize = 16 * 1024;
pub const RENDER_MAX_GEOMETRY_ENTRIES: usize = 65_536;
pub const RENDER_MAX_SCROLL_ENTRIES: usize = 4_096;
pub const RENDER_MAX_SEMANTIC_BOUNDS: usize = 16_384;
pub const RENDER_MAX_TEXT_QUERIES: usize = 256;
pub const RENDER_MAX_TEXT_BOXES: usize = 4_096;
pub const RENDER_MAX_TRUNCATION_DIAGNOSTICS: usize = 32;
pub const RENDER_MAX_SEEN_SCROLL_COMMANDS: usize = 1_024;
pub const RENDER_MAX_SEEN_SEMANTIC_ACTIONS: usize = 1_024;
pub const RENDER_MAX_VIEWPORT_DIMENSION: u32 = 16_384;
pub const RENDER_MAX_COORDINATE: f64 = 16_777_216.0;
pub const RENDER_MAX_SCALE: f64 = 16.0;

macro_rules! define_opaque_handle {
    ($name:ident) => {
        #[repr(transparent)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(RenderHandleId);

        impl $name {
            pub const fn new(raw: u64) -> Option<Self> {
                match RenderHandleId::new(raw) {
                    Some(id) => Some(Self(id)),
                    None => None,
                }
            }

            pub const fn get(self) -> u64 {
                self.0.get()
            }
        }
    };
}

define_opaque_handle!(RenderHitTestHandle);
define_opaque_handle!(RenderTextQueryHandle);

/// Stable renderer protocol error codes consumed by native and Dart adapters.
pub mod render_error_codes {
    pub const VERSION: &str = "render.version";
    pub const REVISION: &str = "render.revision";
    pub const STALE: &str = "render.stale";
    pub const LIMIT: &str = "render.limit";
    pub const DUPLICATE_ID: &str = "render.duplicate-id";
    pub const UNKNOWN_ID: &str = "render.unknown-id";
    pub const INVALID_GRAPH: &str = "render.invalid-graph";
    pub const NON_FINITE: &str = "render.non-finite";
    pub const INVALID_GEOMETRY: &str = "render.invalid-geometry";
    pub const TRUNCATED_REQUIRED: &str = "render.truncated-required";
    pub const UNADVERTISED_ACTION: &str = "render.unadvertised-action";
    pub const REPLAYED_COMMAND: &str = "render.replayed-command";
    pub const REPLAYED_ACTION: &str = "render.replayed-action";
}

/// Validation failure at the renderer trust boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderProtocolError {
    pub code: &'static str,
    pub message: String,
}

impl RenderProtocolError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for RenderProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for RenderProtocolError {}

pub(crate) fn validate_version(version: u16) -> Result<(), RenderProtocolError> {
    if version == RENDER_PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(RenderProtocolError::new(
            render_error_codes::VERSION,
            format!(
                "renderer protocol version {version} is unsupported; expected {RENDER_PROTOCOL_VERSION}"
            ),
        ))
    }
}

pub(crate) fn validate_finite(value: f64, field: &str) -> Result<(), RenderProtocolError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(RenderProtocolError::new(
            render_error_codes::NON_FINITE,
            format!("{field} must be finite"),
        ))
    }
}

/// Protocol limit domains used by truncation diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderLimitDomain {
    Nodes,
    TreeDepth,
    Mutations,
    Resources,
    ResourceBytes,
    StringBytes,
    Geometry,
    ScrollEntries,
    SemanticBounds,
    TextQueries,
    TextBoxes,
}

/// A bounded renderer result that omitted optional output.
///
/// Required output may be described for diagnostics, but a commit containing a
/// `required` truncation is rejected rather than accepted approximately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderTruncationDiagnostic {
    pub domain: RenderLimitDomain,
    pub limit: u64,
    pub omitted: u64,
    pub required: bool,
}

/// Finite point in physical viewport coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderPoint {
    pub x: f64,
    pub y: f64,
}

impl RenderPoint {
    pub fn validate(self, field: &str) -> Result<(), RenderProtocolError> {
        validate_finite(self.x, &format!("{field}.x"))?;
        validate_finite(self.y, &format!("{field}.y"))?;
        if self.x.abs() > RENDER_MAX_COORDINATE || self.y.abs() > RENDER_MAX_COORDINATE {
            return Err(RenderProtocolError::new(
                render_error_codes::INVALID_GEOMETRY,
                format!("{field} exceeds the coordinate limit"),
            ));
        }
        Ok(())
    }
}

/// Finite non-negative size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderSize {
    pub width: f64,
    pub height: f64,
}

impl RenderSize {
    pub fn validate(self, field: &str) -> Result<(), RenderProtocolError> {
        validate_finite(self.width, &format!("{field}.width"))?;
        validate_finite(self.height, &format!("{field}.height"))?;
        if self.width < 0.0
            || self.height < 0.0
            || self.width > RENDER_MAX_COORDINATE
            || self.height > RENDER_MAX_COORDINATE
        {
            return Err(RenderProtocolError::new(
                render_error_codes::INVALID_GEOMETRY,
                format!("{field} must be non-negative and within the coordinate limit"),
            ));
        }
        Ok(())
    }
}

/// Finite axis-aligned rectangle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl RenderRect {
    pub fn validate(self, field: &str) -> Result<(), RenderProtocolError> {
        RenderPoint {
            x: self.x,
            y: self.y,
        }
        .validate(field)?;
        RenderSize {
            width: self.width,
            height: self.height,
        }
        .validate(field)
    }
}

/// Physical viewport and scale authored by BrowserCore and echoed by a commit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderViewport {
    pub width: u32,
    pub height: u32,
    pub device_scale: f64,
    pub page_zoom: f64,
}

impl RenderViewport {
    pub fn validate(self) -> Result<(), RenderProtocolError> {
        if self.width == 0
            || self.height == 0
            || self.width > RENDER_MAX_VIEWPORT_DIMENSION
            || self.height > RENDER_MAX_VIEWPORT_DIMENSION
        {
            return Err(RenderProtocolError::new(
                render_error_codes::INVALID_GEOMETRY,
                "render viewport dimensions must be non-zero and bounded",
            ));
        }
        validate_finite(self.device_scale, "render viewport device scale")?;
        validate_finite(self.page_zoom, "render viewport page zoom")?;
        if !(0.0..=RENDER_MAX_SCALE).contains(&self.device_scale)
            || self.device_scale == 0.0
            || !(0.0..=RENDER_MAX_SCALE).contains(&self.page_zoom)
            || self.page_zoom == 0.0
        {
            return Err(RenderProtocolError::new(
                render_error_codes::INVALID_GEOMETRY,
                "render viewport scales must be positive and bounded",
            ));
        }
        Ok(())
    }
}
