//! GUI GL surface seam (docs/PLAN.md Phase 5 step 2).
//!
//! The shell owns the GTK widget and exposes only `vixen_api::GlContext` to the
//! engine. GL work is expected to run inside `gtk4::GLArea::render`, where GTK
//! has already made the context current; `make_current` is still provided for
//! setup paths that need an explicit current context.

#![allow(unsafe_code)]

use std::ffi::{CString, c_void};
use std::rc::Rc;

use gtk4::prelude::*;
use vixen_api::GlContext;
use vixen_engine::display_list::PaintCommand;
use vixen_engine::paint::{PaintError, Renderer};

const EPOXY_LIBRARIES: &[&str] = &["libepoxy.so.0", "libepoxy.so"];

/// `gtk4::GLArea` wrapper used by the GUI paint path.
#[derive(Clone)]
pub struct GlAreaSurface {
    area: gtk4::GLArea,
    proc_loader: Option<Rc<EpoxyProcLoader>>,
}

impl GlAreaSurface {
    pub fn new(area: &gtk4::GLArea) -> Self {
        Self {
            area: area.clone(),
            proc_loader: EpoxyProcLoader::load().map(Rc::new),
        }
    }

    pub fn widget(&self) -> &gtk4::GLArea {
        &self.area
    }

    pub fn has_proc_loader(&self) -> bool {
        self.proc_loader.is_some()
    }
}

/// Reusable WebRender state for a `gtk4::GLArea` render callback.
pub struct GlAreaRenderer {
    surface: GlAreaSurface,
    renderer: Option<Renderer>,
}

impl GlAreaRenderer {
    pub fn new(area: &gtk4::GLArea) -> Self {
        Self {
            surface: GlAreaSurface::new(area),
            renderer: None,
        }
    }

    pub fn surface(&self) -> &GlAreaSurface {
        &self.surface
    }

    /// Render an immutable BrowserCore paint snapshot into the GLArea's current
    /// framebuffer. Snapshot capture happens outside the GL render callback.
    pub fn render_commands(
        &mut self,
        commands: &[PaintCommand],
        viewport: (u32, u32),
    ) -> Result<(), PaintError> {
        if self.renderer.is_none() {
            self.renderer = Some(Renderer::new(&self.surface)?);
        }
        self.renderer
            .as_mut()
            .expect("renderer was just initialised")
            .render(&self.surface, commands, viewport)
    }

    /// Drop WebRender state after GTK recreates the GL context.
    pub fn reset_renderer(&mut self) {
        self.renderer = None;
    }
}

struct EpoxyProcLoader {
    library: libloading::Library,
}

impl EpoxyProcLoader {
    fn load() -> Option<Self> {
        for library_name in EPOXY_LIBRARIES {
            // SAFETY: this is the GTK GL symbol-loader trust boundary. We load
            // libepoxy by its stable SONAME while GTK owns the active GDK GL
            // context, and fail closed by returning no proc loader if unavailable.
            let library = match unsafe { libloading::Library::new(library_name) } {
                Ok(library) => library,
                Err(_) => continue,
            };
            return Some(Self { library });
        }
        None
    }

    fn proc_address(&self, name: &str) -> *const c_void {
        let Some(symbol_name) = epoxy_symbol_name(name) else {
            return std::ptr::null();
        };
        // SAFETY: libepoxy exports stable `epoxy_gl*` wrapper functions. Each
        // wrapper resolves/dispatches to the current GDK GL context when called,
        // and `library` is retained for at least as long as returned function
        // pointers can be used by WebRender/gleam.
        unsafe {
            self.library
                .get::<*const c_void>(symbol_name.as_bytes_with_nul())
                .map_or(std::ptr::null(), |symbol| *symbol)
        }
    }
}

fn epoxy_symbol_name(name: &str) -> Option<CString> {
    CString::new(format!("epoxy_{name}")).ok()
}

impl GlContext for GlAreaSurface {
    fn make_current(&self) {
        self.area.make_current();
    }

    fn proc_address(&self, name: &str) -> *const c_void {
        self.proc_loader
            .as_ref()
            .map_or(std::ptr::null(), |loader| loader.proc_address(name))
    }

    fn drawable_size(&self) -> (u32, u32) {
        let scale = u32::try_from(self.area.scale_factor()).unwrap_or(1).max(1);
        let width = u32::try_from(self.area.width()).unwrap_or(0);
        let height = u32::try_from(self.area.height()).unwrap_or(0);
        (width.saturating_mul(scale), height.saturating_mul(scale))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoxy_symbol_names_match_flatpak_runtime_exports() {
        assert_eq!(
            epoxy_symbol_name("glGetString")
                .expect("symbol")
                .as_bytes_with_nul(),
            b"epoxy_glGetString\0"
        );
        assert!(epoxy_symbol_name("gl\0GetString").is_none());
    }
}
