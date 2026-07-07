//! Headless GL surface seam (docs/PLAN.md Phase 5 step 3).
//!
//! This module owns the headless-side implementation of
//! [`vixen_api::GlContext`].  The type is intentionally small: construction is
//! the fallible trust boundary where EGL availability is checked; once a
//! `SurfacelessSurface` exists, the renderer can treat it as a plain
//! `GlContext`.
//!
//! The EGL/WebRender hookup has not landed yet, so the public constructor fails
//! closed with the stable screenshot error code used by the CLI. Tests build a
//! surface with an injected loader to keep the `GlContext` contract executable
//! without adding a second paint path or a CPU fallback.

#![forbid(unsafe_code)]

use std::ffi::c_void;

use vixen_api::GlContext;
use vixen_engine::engine_error::codes;

type ProcAddressLoader = fn(&str) -> *const c_void;

const EGL_NOT_WIRED: &str = "EGL surfaceless context is not wired yet";

/// Physical drawable size for an offscreen headless surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceSize {
    pub width: u32,
    pub height: u32,
}

impl SurfaceSize {
    pub fn new(width: u32, height: u32) -> Result<Self, SurfaceError> {
        if width == 0 || height == 0 {
            return Err(SurfaceError::InvalidSize { width, height });
        }
        Ok(Self { width, height })
    }

    pub fn from_viewport(viewport: (u32, u32)) -> Result<Self, SurfaceError> {
        Self::new(viewport.0, viewport.1)
    }
}

/// Fail-closed errors from creating a headless GL surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceError {
    InvalidSize { width: u32, height: u32 },
    Unavailable { reason: &'static str },
}

impl SurfaceError {
    /// Stable CLI/CDP-facing error code for screenshot-capability failures.
    pub fn stable_code(&self) -> &'static str {
        match self {
            SurfaceError::InvalidSize { .. } | SurfaceError::Unavailable { .. } => {
                codes::UNSUPPORTED_SCREENSHOT
            }
        }
    }
}

impl std::fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SurfaceError::InvalidSize { width, height } => {
                write!(f, "invalid surface size {width}x{height}")
            }
            SurfaceError::Unavailable { reason } => f.write_str(reason),
        }
    }
}

impl std::error::Error for SurfaceError {}

/// Headless surfaceless EGL-backed GL context.
#[derive(Debug)]
pub struct SurfacelessSurface {
    size: SurfaceSize,
    proc_loader: ProcAddressLoader,
}

impl SurfacelessSurface {
    /// Create the headless GL surface for `viewport`.
    ///
    /// Today this fails closed until the EGL context creation code lands. That
    /// preserves the stable `unsupported.screenshot` CLI contract while giving
    /// the future renderer a concrete `GlContext` type to target.
    pub fn new(viewport: (u32, u32)) -> Result<Self, SurfaceError> {
        let _size = SurfaceSize::from_viewport(viewport)?;
        Err(SurfaceError::Unavailable {
            reason: EGL_NOT_WIRED,
        })
    }

    #[cfg(test)]
    fn with_loader_for_tests(
        viewport: (u32, u32),
        proc_loader: ProcAddressLoader,
    ) -> Result<Self, SurfaceError> {
        Ok(Self {
            size: SurfaceSize::from_viewport(viewport)?,
            proc_loader,
        })
    }
}

impl GlContext for SurfacelessSurface {
    fn make_current(&self) {
        // Real EGL context currentness is established by `new` once the EGL
        // integration lands. The injected-test surface has no currentness work.
    }

    fn proc_address(&self, name: &str) -> *const c_void {
        (self.proc_loader)(name)
    }

    fn drawable_size(&self) -> (u32, u32) {
        (self.size.width, self.size.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static GL_CLEAR_SENTINEL: u8 = 0;

    fn fake_proc_loader(name: &str) -> *const c_void {
        match name {
            "glClear" => (&GL_CLEAR_SENTINEL as *const u8).cast::<c_void>(),
            _ => std::ptr::null(),
        }
    }

    #[test]
    fn size_rejects_zero_extent() {
        assert_eq!(
            SurfaceSize::new(0, 600),
            Err(SurfaceError::InvalidSize {
                width: 0,
                height: 600,
            })
        );
        assert_eq!(SurfaceSize::new(800, 600).unwrap().width, 800);
    }

    #[test]
    fn unavailable_constructor_preserves_stable_code() {
        let err = SurfacelessSurface::new((800, 600)).unwrap_err();
        assert_eq!(err.stable_code(), codes::UNSUPPORTED_SCREENSHOT);
        assert!(matches!(err, SurfaceError::Unavailable { .. }));
    }

    #[test]
    fn trait_object_exposes_size_and_proc_loader() {
        let surface = SurfacelessSurface::with_loader_for_tests((320, 200), fake_proc_loader)
            .expect("test surface");
        let ctx: &dyn GlContext = &surface;
        ctx.make_current();
        assert_eq!(ctx.drawable_size(), (320, 200));
        assert!(!ctx.proc_address("glClear").is_null());
        assert!(ctx.proc_address("glMissing").is_null());
    }
}
