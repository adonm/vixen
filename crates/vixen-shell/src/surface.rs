//! GUI GL surface seam (docs/PLAN.md Phase 5 step 2).
//!
//! The shell owns the GTK widget and exposes only `vixen_api::GlContext` to the
//! engine. GL work is expected to run inside `gtk4::GLArea::render`, where GTK
//! has already made the context current; `make_current` is still provided for
//! setup paths that need an explicit current context.

#![allow(unsafe_code)]

use std::ffi::{CString, c_char, c_void};
use std::rc::Rc;

use gtk4::prelude::*;
use vixen_api::GlContext;
use vixen_engine::display_list::PaintCommand;
use vixen_engine::page::Page;
use vixen_engine::paint::{PaintError, Renderer};

type EpoxyGetProcAddress = unsafe extern "C" fn(*const c_char) -> *mut c_void;

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

    /// Render a page through the shared WebRender path into the GLArea's
    /// current framebuffer. Call this from `GLArea::connect_render`.
    pub fn render_page(&mut self, page: &Page) -> Result<(), PaintError> {
        let viewport = self.surface.drawable_size();
        let commands = page.display_list(viewport);
        self.render_commands(&commands, viewport)
    }

    /// Render prebuilt paint commands into the GLArea's current framebuffer.
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
    _library: libloading::Library,
    get_proc_address: EpoxyGetProcAddress,
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
            // SAFETY: libepoxy exposes `epoxy_get_proc_address` with this C ABI.
            // The `Library` is retained in `Self` for at least as long as the
            // copied function pointer can be called.
            let get_proc_address =
                match unsafe { library.get::<EpoxyGetProcAddress>(b"epoxy_get_proc_address\0") } {
                    Ok(symbol) => *symbol,
                    Err(_) => continue,
                };
            return Some(Self {
                _library: library,
                get_proc_address,
            });
        }
        None
    }

    fn proc_address(&self, name: &str) -> *const c_void {
        let Ok(name) = CString::new(name) else {
            return std::ptr::null();
        };
        // SAFETY: libepoxy resolves the symbol for the current GDK GL context;
        // callers invoke this only after `make_current` has made that context
        // current on this thread.
        unsafe { (self.get_proc_address)(name.as_ptr()) as *const c_void }
    }
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
