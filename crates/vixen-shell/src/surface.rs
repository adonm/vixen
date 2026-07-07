//! GUI GL surface seam (docs/PLAN.md Phase 5 step 2).
//!
//! The shell owns the GTK widget and exposes only `vixen_api::GlContext` to the
//! engine. GL work is expected to run inside `gtk4::GLArea::render`, where GTK
//! has already made the context current; `make_current` is still provided for
//! setup paths that need an explicit current context.

#![forbid(unsafe_code)]

use std::ffi::c_void;

use gtk4::prelude::*;
use vixen_api::GlContext;

/// `gtk4::GLArea` wrapper used by the GUI paint path.
#[derive(Clone)]
pub struct GlAreaSurface {
    area: gtk4::GLArea,
}

impl GlAreaSurface {
    pub fn new(area: &gtk4::GLArea) -> Self {
        Self { area: area.clone() }
    }

    pub fn widget(&self) -> &gtk4::GLArea {
        &self.area
    }
}

impl GlContext for GlAreaSurface {
    fn make_current(&self) {
        self.area.make_current();
    }

    fn proc_address(&self, _name: &str) -> *const c_void {
        // GTK/GDK currentness and sizing are in place; the concrete GL symbol
        // loader is wired with the WebRender renderer so there is still exactly
        // one paint path and no shell-local fallback renderer.
        std::ptr::null()
    }

    fn drawable_size(&self) -> (u32, u32) {
        let scale = u32::try_from(self.area.scale_factor()).unwrap_or(1).max(1);
        let width = u32::try_from(self.area.width()).unwrap_or(0);
        let height = u32::try_from(self.area.height()).unwrap_or(0);
        (width.saturating_mul(scale), height.saturating_mul(scale))
    }
}
