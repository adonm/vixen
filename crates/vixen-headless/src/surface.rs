//! Headless GL surface seam (docs/PLAN.md Phase 5 step 3).
//!
//! This module owns the headless-side implementation of
//! [`vixen_api::GlContext`].  The type is intentionally small: construction is
//! the fallible trust boundary where EGL availability is checked; once a
//! `SurfacelessSurface` exists, the renderer can treat it as a plain
//! `GlContext`.
//!
//! The public constructor creates an EGL context using Mesa's surfaceless
//! platform when available and a pbuffer default framebuffer for WebRender's
//! draw target. Tests build a surface with an injected loader to keep the
//! `GlContext` contract executable without adding a second paint path or a CPU
//! fallback.

#![allow(unsafe_code)]

use std::ffi::c_void;

use khronos_egl as egl;
use vixen_api::GlContext;
use vixen_engine::engine_error::codes;

#[cfg(test)]
type ProcAddressLoader = fn(&str) -> *const c_void;
type Egl = egl::DynamicInstance<egl::EGL1_5>;

const EGL_PLATFORM_SURFACELESS_MESA: egl::Enum = 0x31DD;

/// Physical drawable size for an offscreen headless surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceSize {
    pub width: u32,
    pub height: u32,
}

impl SurfaceSize {
    pub fn new(width: u32, height: u32) -> Result<Self, SurfaceError> {
        if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
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
    Unavailable { reason: String },
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
pub struct SurfacelessSurface {
    size: SurfaceSize,
    proc_loader: ProcLoader,
}

enum ProcLoader {
    Egl(Box<EglState>),
    #[cfg(test)]
    Test(ProcAddressLoader),
}

struct EglState {
    egl: Egl,
    display: egl::Display,
    surface: egl::Surface,
    context: egl::Context,
}

impl SurfacelessSurface {
    /// Create the headless GL surface for `viewport`.
    pub fn new(viewport: (u32, u32)) -> Result<Self, SurfaceError> {
        let size = SurfaceSize::from_viewport(viewport)?;
        Ok(Self {
            size,
            proc_loader: ProcLoader::Egl(Box::new(EglState::new(size)?)),
        })
    }

    #[cfg(test)]
    fn with_loader_for_tests(
        viewport: (u32, u32),
        proc_loader: ProcAddressLoader,
    ) -> Result<Self, SurfaceError> {
        Ok(Self {
            size: SurfaceSize::from_viewport(viewport)?,
            proc_loader: ProcLoader::Test(proc_loader),
        })
    }
}

impl EglState {
    fn new(size: SurfaceSize) -> Result<Self, SurfaceError> {
        let egl = load_egl()?;
        let display = create_display(&egl)?;
        egl.initialize(display)
            .map_err(egl_error("initialize EGL"))?;
        egl.bind_api(egl::OPENGL_API)
            .map_err(egl_error("bind OpenGL API"))?;

        let config = choose_config(&egl, display)?;
        let surface_attrs = [
            egl::WIDTH,
            size.width as i32,
            egl::HEIGHT,
            size.height as i32,
            egl::NONE,
        ];
        let surface = egl
            .create_pbuffer_surface(display, config, &surface_attrs)
            .map_err(egl_error("create EGL pbuffer surface"))?;
        let context = create_context(&egl, display, config)?;
        egl.make_current(display, Some(surface), Some(surface), Some(context))
            .map_err(egl_error("make EGL context current"))?;

        Ok(Self {
            egl,
            display,
            surface,
            context,
        })
    }

    fn make_current(&self) {
        let _ = self.egl.make_current(
            self.display,
            Some(self.surface),
            Some(self.surface),
            Some(self.context),
        );
    }

    fn proc_address(&self, name: &str) -> *const c_void {
        self.egl
            .get_proc_address(name)
            .map_or(std::ptr::null(), |addr| addr as *const c_void)
    }
}

impl Drop for EglState {
    fn drop(&mut self) {
        let _ = self.egl.make_current(self.display, None, None, None);
        let _ = self.egl.destroy_context(self.display, self.context);
        let _ = self.egl.destroy_surface(self.display, self.surface);
        let _ = self.egl.terminate(self.display);
        let _ = self.egl.release_thread();
    }
}

fn load_egl() -> Result<Egl, SurfaceError> {
    // SAFETY: dynamic EGL loading is the headless GL trust boundary. We only
    // accept the Khronos EGL entry points exposed by libEGL and fail closed if
    // the library or required EGL 1.5 symbols are unavailable.
    unsafe { egl::DynamicInstance::<egl::EGL1_5>::load_required() }.map_err(|e| {
        SurfaceError::Unavailable {
            reason: format!("load EGL: {e}"),
        }
    })
}

fn create_display(egl: &Egl) -> Result<egl::Display, SurfaceError> {
    // SAFETY: EGL_MESA_platform_surfaceless requires a null native display. The
    // EGL implementation validates support and returns an error if unavailable.
    let surfaceless = unsafe {
        egl.get_platform_display(
            EGL_PLATFORM_SURFACELESS_MESA,
            std::ptr::null_mut(),
            &[egl::ATTRIB_NONE],
        )
    };
    match surfaceless {
        Ok(display) => Ok(display),
        Err(platform_err) => {
            // SAFETY: EGL_DEFAULT_DISPLAY is the standard fallback display token;
            // no native display pointer is dereferenced by Vixen.
            unsafe { egl.get_display(egl::DEFAULT_DISPLAY) }.ok_or_else(|| {
                SurfaceError::Unavailable {
                    reason: format!(
                        "EGL surfaceless display unavailable ({platform_err}); default display unavailable"
                    ),
                }
            })
        }
    }
}

fn choose_config(egl: &Egl, display: egl::Display) -> Result<egl::Config, SurfaceError> {
    let attrs = [
        egl::SURFACE_TYPE,
        egl::PBUFFER_BIT,
        egl::RENDERABLE_TYPE,
        egl::OPENGL_BIT,
        egl::RED_SIZE,
        8,
        egl::GREEN_SIZE,
        8,
        egl::BLUE_SIZE,
        8,
        egl::ALPHA_SIZE,
        8,
        egl::DEPTH_SIZE,
        0,
        egl::STENCIL_SIZE,
        0,
        egl::NONE,
    ];
    egl.choose_first_config(display, &attrs)
        .map_err(egl_error("choose EGL config"))?
        .ok_or_else(|| SurfaceError::Unavailable {
            reason: "choose EGL config: no matching pbuffer OpenGL config".to_owned(),
        })
}

fn create_context(
    egl: &Egl,
    display: egl::Display,
    config: egl::Config,
) -> Result<egl::Context, SurfaceError> {
    let core_attrs = [
        egl::CONTEXT_MAJOR_VERSION,
        3,
        egl::CONTEXT_MINOR_VERSION,
        3,
        egl::CONTEXT_OPENGL_PROFILE_MASK,
        egl::CONTEXT_OPENGL_CORE_PROFILE_BIT,
        egl::NONE,
    ];
    match egl.create_context(display, config, None, &core_attrs) {
        Ok(context) => Ok(context),
        Err(core_err) => egl
            .create_context(display, config, None, &[egl::NONE])
            .map_err(|compat_err| SurfaceError::Unavailable {
                reason: format!(
                    "create EGL OpenGL context: core profile failed ({core_err}); compatibility failed ({compat_err})"
                ),
            }),
    }
}

fn egl_error(action: &'static str) -> impl FnOnce(egl::Error) -> SurfaceError {
    move |err| SurfaceError::Unavailable {
        reason: format!("{action}: {err}"),
    }
}

impl GlContext for SurfacelessSurface {
    fn make_current(&self) {
        match &self.proc_loader {
            ProcLoader::Egl(state) => state.make_current(),
            #[cfg(test)]
            ProcLoader::Test(_) => {}
        }
    }

    fn proc_address(&self, name: &str) -> *const c_void {
        match &self.proc_loader {
            ProcLoader::Egl(state) => state.proc_address(name),
            #[cfg(test)]
            ProcLoader::Test(loader) => loader(name),
        }
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
    fn invalid_constructor_preserves_stable_code() {
        let err = match SurfacelessSurface::new((0, 600)) {
            Ok(_) => panic!("zero viewport should fail"),
            Err(err) => err,
        };
        assert_eq!(err.stable_code(), codes::UNSUPPORTED_SCREENSHOT);
        assert!(matches!(err, SurfaceError::InvalidSize { .. }));
    }

    #[test]
    fn unavailable_errors_preserve_stable_code() {
        let err = SurfaceError::Unavailable {
            reason: "no EGL".to_owned(),
        };
        assert_eq!(err.stable_code(), codes::UNSUPPORTED_SCREENSHOT);
        assert_eq!(err.to_string(), "no EGL");
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
