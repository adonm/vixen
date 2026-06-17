//! Typed engine errors with stable codes (docs/SPEC.md "Diagnostics shape",
//! docs/ARCHITECTURE.md). Codes are a stable contract: automation may match
//! on them, so they must not be silently renamed.

use thiserror::Error;

/// Stable, dotted error codes. Add new codes only at the end.
pub mod codes {
    pub const SCRIPT_EVAL: &str = "script.eval";
    pub const SCRIPT_COMPILE: &str = "script.compile";
    pub const SCRIPT_OOM: &str = "script.oom";
    pub const SCRIPT_TIMEOUT: &str = "script.timeout";
    pub const UNSUPPORTED_SCREENSHOT: &str = "unsupported.screenshot";
    pub const INVALID_SELECTOR: &str = "invalid-selector";
}

/// Engine-level error carrying a stable code.
#[derive(Debug, Error)]
pub enum EngineError {
    #[error("script error: {message}")]
    Script { code: &'static str, message: String },

    #[error("engine: {message}")]
    Other { code: &'static str, message: String },
}

impl EngineError {
    pub fn code(&self) -> &'static str {
        match self {
            EngineError::Script { code, .. } | EngineError::Other { code, .. } => code,
        }
    }

    pub fn script(code: &'static str, message: impl Into<String>) -> Self {
        EngineError::Script {
            code,
            message: message.into(),
        }
    }
}
