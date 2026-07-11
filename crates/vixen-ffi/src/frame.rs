//! Bounded offscreen frame rendering for foreign-language frontends.

#![allow(unsafe_code)]

use std::ffi::c_void;

use khronos_egl as egl;
use vixen_api::{BrowserError, BrowsingContextId, DocumentId, GlContext, browser_error_codes};
use vixen_engine::browser::EngineBrowserHandle;
use vixen_engine::engine_error::codes;
use vixen_engine::paint::{RgbaFrame, render_commands_to_rgba};

pub const MAX_FRAME_DIMENSION: u32 = 4096;
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

type Egl = egl::DynamicInstance<egl::EGL1_5>;

const EGL_PLATFORM_SURFACELESS_MESA: egl::Enum = 0x31DD;

pub(crate) fn capture_rgba_frame(
    handle: &mut EngineBrowserHandle,
    context_id: BrowsingContextId,
    document_id: DocumentId,
    viewport: (u32, u32),
) -> Result<RgbaFrame, BrowserError> {
    let expected_len = expected_rgba_len(viewport)?;
    let snapshot = handle.capture_paint_snapshot(context_id, document_id, viewport)?;
    let surface = FrameGlContext::new(viewport).map_err(render_error)?;
    let rendered = render_commands_to_rgba(&surface, &snapshot.commands, viewport)
        .map_err(|error| render_error(format!("render frame: {error}")))?;
    if (rendered.width, rendered.height) != viewport || rendered.rgba.len() != expected_len {
        return Err(render_error(format!(
            "renderer returned {}x{} and {} bytes for requested {}x{} RGBA frame",
            rendered.width,
            rendered.height,
            rendered.rgba.len(),
            viewport.0,
            viewport.1
        )));
    }
    Ok(rendered)
}

pub(crate) fn expected_rgba_len(viewport: (u32, u32)) -> Result<usize, BrowserError> {
    let (width, height) = viewport;
    if width == 0 || height == 0 || width > MAX_FRAME_DIMENSION || height > MAX_FRAME_DIMENSION {
        return Err(BrowserError::new(
            browser_error_codes::INVALID_ARGUMENT,
            format!("frame dimensions must each be within 1 and {MAX_FRAME_DIMENSION} pixels"),
        ));
    }
    let len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| {
            BrowserError::new(
                browser_error_codes::INVALID_ARGUMENT,
                "frame RGBA byte length overflows size_t",
            )
        })?;
    if len > MAX_FRAME_BYTES {
        return Err(BrowserError::new(
            browser_error_codes::INVALID_ARGUMENT,
            format!("frame exceeds {MAX_FRAME_BYTES} RGBA bytes"),
        ));
    }
    Ok(len)
}

fn render_error(message: impl Into<String>) -> BrowserError {
    BrowserError::new(codes::UNSUPPORTED_SCREENSHOT, message)
}

struct FrameGlContext {
    viewport: (u32, u32),
    state: EglState,
}

impl FrameGlContext {
    fn new(viewport: (u32, u32)) -> Result<Self, String> {
        Ok(Self {
            viewport,
            state: EglState::new(viewport)?,
        })
    }
}

struct EglState {
    egl: Egl,
    display: egl::Display,
    surface: egl::Surface,
    context: egl::Context,
}

impl EglState {
    fn new(viewport: (u32, u32)) -> Result<Self, String> {
        let egl = load_egl()?;
        let display = create_display(&egl)?;
        egl.initialize(display)
            .map_err(|error| format!("initialize EGL: {error}"))?;
        egl.bind_api(egl::OPENGL_API)
            .map_err(|error| format!("bind OpenGL API: {error}"))?;
        let config = choose_config(&egl, display)?;
        let surface_attrs = [
            egl::WIDTH,
            viewport.0 as i32,
            egl::HEIGHT,
            viewport.1 as i32,
            egl::NONE,
        ];
        let surface = egl
            .create_pbuffer_surface(display, config, &surface_attrs)
            .map_err(|error| format!("create EGL pbuffer surface: {error}"))?;
        let context = create_context(&egl, display, config)?;
        egl.make_current(display, Some(surface), Some(surface), Some(context))
            .map_err(|error| format!("make EGL context current: {error}"))?;
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
            .map_or(std::ptr::null(), |address| address as *const c_void)
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

impl GlContext for FrameGlContext {
    fn make_current(&self) {
        self.state.make_current();
    }

    fn proc_address(&self, name: &str) -> *const c_void {
        self.state.proc_address(name)
    }

    fn drawable_size(&self) -> (u32, u32) {
        self.viewport
    }
}

fn load_egl() -> Result<Egl, String> {
    // SAFETY: this is the local EGL trust boundary. Only required EGL 1.5
    // symbols are dynamically loaded, and construction fails if unavailable.
    unsafe { egl::DynamicInstance::<egl::EGL1_5>::load_required() }
        .map_err(|error| format!("load EGL: {error}"))
}

fn create_display(egl: &Egl) -> Result<egl::Display, String> {
    // SAFETY: EGL_MESA_platform_surfaceless requires a null native display;
    // the EGL implementation validates extension support before returning it.
    let surfaceless = unsafe {
        egl.get_platform_display(
            EGL_PLATFORM_SURFACELESS_MESA,
            std::ptr::null_mut(),
            &[egl::ATTRIB_NONE],
        )
    };
    match surfaceless {
        Ok(display) => Ok(display),
        Err(platform_error) => {
            // SAFETY: EGL_DEFAULT_DISPLAY is EGL's standard opaque display token.
            unsafe { egl.get_display(egl::DEFAULT_DISPLAY) }.ok_or_else(|| {
                format!(
                    "EGL surfaceless display unavailable ({platform_error}); default display unavailable"
                )
            })
        }
    }
}

fn choose_config(egl: &Egl, display: egl::Display) -> Result<egl::Config, String> {
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
        .map_err(|error| format!("choose EGL config: {error}"))?
        .ok_or_else(|| "choose EGL config: no matching pbuffer OpenGL config".to_owned())
}

fn create_context(
    egl: &Egl,
    display: egl::Display,
    config: egl::Config,
) -> Result<egl::Context, String> {
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
        Err(core_error) => egl
            .create_context(display, config, None, &[egl::NONE])
            .map_err(|compatibility_error| {
                format!(
                    "create EGL OpenGL context: core profile failed ({core_error}); compatibility failed ({compatibility_error})"
                )
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_bounds_are_exact() {
        assert_eq!(expected_rgba_len((1, 1)).unwrap(), 4);
        assert_eq!(
            expected_rgba_len((MAX_FRAME_DIMENSION, MAX_FRAME_DIMENSION)).unwrap(),
            MAX_FRAME_BYTES
        );
        for viewport in [(0, 1), (1, 0), (MAX_FRAME_DIMENSION + 1, 1)] {
            let error = expected_rgba_len(viewport).unwrap_err();
            assert_eq!(error.code, browser_error_codes::INVALID_ARGUMENT);
        }
    }
}
