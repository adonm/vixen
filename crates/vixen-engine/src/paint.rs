//! WebRender paint path for Vixen display-list commands.
//!
//! This module owns the single renderer dispatch from Vixen's invariant-enforced
//! [`PaintCommand`](crate::display_list::PaintCommand) stream to WebRender. The
//! only platform-specific input is [`vixen_api::GlContext`]; callers provide GTK
//! or EGL context currentness and symbol lookup at that seam.

#![allow(unsafe_code)]

use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use gleam::gl;
use thiserror::Error;
use vixen_api::GlContext;
use webrender::api::units::{
    DeviceIntRect, DeviceIntSize, FramebufferIntRect, FramebufferIntSize, LayoutPoint, LayoutRect,
    LayoutSize,
};
use webrender::api::{
    BuiltDisplayList, ColorF, CommonItemProperties, DocumentId, Epoch, ExternalEvent,
    FramePublishId, FrameReadyParams, PipelineId, PrimitiveFlags, RenderNotifier, RenderReasons,
    SpaceAndClipInfo,
};
use webrender::{
    Renderer as WebRenderRenderer, Transaction, WebRenderOptions, create_webrender_instance,
};

use crate::display_list::{Color, PaintCommand, Rect};

const PIPELINE_ID: PipelineId = PipelineId(0, 0);
const FRAME_READY_TIMEOUT: Duration = Duration::from_secs(2);

/// Packed RGBA8 framebuffer read back from the WebRender path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    /// Row-major top-to-bottom RGBA8 pixels, exactly `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// Fail-closed paint path errors.
#[derive(Debug, Error)]
pub enum PaintError {
    #[error("invalid viewport {0}x{1}")]
    InvalidViewport(u32, u32),
    #[error("GL symbol {0} is unavailable")]
    MissingGlSymbol(&'static str),
    #[error("failed to create WebRender renderer: {0:?}")]
    RendererCreate(webrender::RendererError),
    #[error("WebRender frame was not ready within {0:?}")]
    FrameTimeout(Duration),
    #[error("WebRender render failed: {0:?}")]
    Render(Vec<webrender::RendererError>),
}

/// A WebRender instance bound to one GL context/document.
pub struct Renderer {
    renderer: Option<WebRenderRenderer>,
    api: webrender::RenderApi,
    document_id: DocumentId,
    epoch: u32,
    notifier: PaintNotifier,
}

impl Renderer {
    /// Create a WebRender renderer for the current `GlContext`.
    pub fn new(context: &dyn GlContext) -> Result<Self, PaintError> {
        context.make_current();
        require_gl_symbol(context, "glGetString")?;

        let gl = load_gl(context);
        let notifier = PaintNotifier::default();
        let options = WebRenderOptions {
            clear_color: ColorF::new(1.0, 1.0, 1.0, 1.0),
            enable_debugger: false,
            testing: true,
            ..WebRenderOptions::default()
        };

        let (renderer, sender) =
            create_webrender_instance(gl, Box::new(Clone::clone(&notifier)), options, None)
                .map_err(PaintError::RendererCreate)?;
        let api = sender.create_api();
        let size = device_size(context.drawable_size())?;
        let document_id = api.add_document(size);

        Ok(Self {
            renderer: Some(renderer),
            api,
            document_id,
            epoch: 0,
            notifier,
        })
    }

    /// Submit the command stream and render a frame to the current GL draw target.
    pub fn render(
        &mut self,
        context: &dyn GlContext,
        commands: &[PaintCommand],
        viewport: (u32, u32),
    ) -> Result<(), PaintError> {
        context.make_current();
        let size = device_size(viewport)?;
        self.epoch = self.epoch.saturating_add(1).max(1);
        let epoch = Epoch(self.epoch);
        let display_list = build_webrender_display_list(commands, viewport)?;

        let mut txn = Transaction::new();
        txn.set_document_view(DeviceIntRect::from_size(size));
        txn.set_root_pipeline(PIPELINE_ID);
        txn.set_display_list(epoch, display_list);
        txn.generate_frame(self.epoch as u64, true, false, RenderReasons::TESTING);
        self.notifier.reset();
        self.api.send_transaction(self.document_id, txn);

        let renderer = self.renderer.as_mut().expect("renderer live until drop");
        wait_for_frame(renderer, self.document_id, PIPELINE_ID, epoch)?;

        renderer
            .render(size, 0)
            .map(|_| ())
            .map_err(PaintError::Render)
    }

    /// Submit the command stream, render a frame, and read back RGBA pixels.
    pub fn render_to_rgba(
        &mut self,
        context: &dyn GlContext,
        commands: &[PaintCommand],
        viewport: (u32, u32),
    ) -> Result<RgbaFrame, PaintError> {
        self.render(context, commands, viewport)?;
        let size = device_size(viewport)?;
        let renderer = self.renderer.as_mut().expect("renderer live until drop");
        let rect = FramebufferIntRect::from_size(FramebufferIntSize::new(size.width, size.height));
        let mut rgba = renderer.read_pixels_rgba8(rect);
        flip_rgba_rows(&mut rgba, viewport.0, viewport.1);
        Ok(RgbaFrame {
            width: viewport.0,
            height: viewport.1,
            rgba,
        })
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        self.api.shut_down(true);
        if let Some(renderer) = self.renderer.take() {
            renderer.deinit();
        }
    }
}

/// Convenience entry point for one-shot headless screenshots.
pub fn render_commands_to_rgba(
    context: &dyn GlContext,
    commands: &[PaintCommand],
    viewport: (u32, u32),
) -> Result<RgbaFrame, PaintError> {
    Renderer::new(context)?.render_to_rgba(context, commands, viewport)
}

/// Build the WebRender display list for tests and renderer submission.
pub fn build_webrender_display_list(
    commands: &[PaintCommand],
    viewport: (u32, u32),
) -> Result<(PipelineId, BuiltDisplayList), PaintError> {
    let _ = device_size(viewport)?;
    let mut builder = webrender::api::DisplayListBuilder::new(PIPELINE_ID);
    let viewport_rect = layout_rect(Rect::new(0.0, 0.0, viewport.0 as f32, viewport.1 as f32));
    let space_and_clip = SpaceAndClipInfo::root_scroll(PIPELINE_ID);
    let common = CommonItemProperties::new(viewport_rect, space_and_clip);

    builder.begin();
    builder.push_simple_stacking_context(space_and_clip.spatial_id, PrimitiveFlags::default());
    for command in commands {
        match command {
            PaintCommand::Background { fill, color, .. } => {
                push_rect(&mut builder, &common, *fill, *color);
            }
            // Text shaping/glyph upload is the next renderer slice. Until then,
            // paint the invariant-enforced text run rect through WebRender so the
            // screenshot path exercises the same GPU command stream without a CPU
            // fallback painter.
            PaintCommand::Text { rect, color, .. } => {
                push_rect(&mut builder, &common, *rect, *color);
            }
        }
    }
    builder.pop_stacking_context();
    Ok(builder.end())
}

fn push_rect(
    builder: &mut webrender::api::DisplayListBuilder,
    common: &CommonItemProperties,
    rect: Rect,
    color: Color,
) {
    if rect.w <= 0.0 || rect.h <= 0.0 || color.a == 0 {
        return;
    }
    builder.push_rect(common, layout_rect(rect), color_f(color));
}

fn load_gl(context: &dyn GlContext) -> Rc<dyn gl::Gl> {
    // SAFETY: `GlContext` is Vixen's GL trust boundary. Implementations must
    // return function pointers for the current context; WebRender/gleam only
    // call through those pointers after the caller has made the context current.
    unsafe { gl::GlFns::load_with(|name| context.proc_address(name)) }
}

fn require_gl_symbol(context: &dyn GlContext, name: &'static str) -> Result<(), PaintError> {
    if context.proc_address(name).is_null() {
        Err(PaintError::MissingGlSymbol(name))
    } else {
        Ok(())
    }
}

fn wait_for_frame(
    renderer: &mut WebRenderRenderer,
    document_id: DocumentId,
    pipeline_id: PipelineId,
    epoch: Epoch,
) -> Result<(), PaintError> {
    let deadline = Instant::now() + FRAME_READY_TIMEOUT;
    loop {
        renderer.update();
        if renderer.current_epoch(document_id, pipeline_id) == Some(epoch) {
            renderer.update();
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(PaintError::FrameTimeout(FRAME_READY_TIMEOUT));
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn device_size(viewport: (u32, u32)) -> Result<DeviceIntSize, PaintError> {
    let (width, height) = viewport;
    if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
        return Err(PaintError::InvalidViewport(width, height));
    }
    Ok(DeviceIntSize::new(width as i32, height as i32))
}

fn layout_rect(rect: Rect) -> LayoutRect {
    LayoutRect::from_origin_and_size(
        LayoutPoint::new(rect.x, rect.y),
        LayoutSize::new(rect.w.max(0.0), rect.h.max(0.0)),
    )
}

fn color_f(color: Color) -> ColorF {
    ColorF::new(
        f32::from(color.r) / 255.0,
        f32::from(color.g) / 255.0,
        f32::from(color.b) / 255.0,
        f32::from(color.a) / 255.0,
    )
}

fn flip_rgba_rows(rgba: &mut [u8], width: u32, height: u32) {
    let stride = width as usize * 4;
    for row in 0..(height as usize / 2) {
        let opposite = height as usize - row - 1;
        let top = row * stride;
        let bottom = opposite * stride;
        for col in 0..stride {
            rgba.swap(top + col, bottom + col);
        }
    }
}

#[derive(Clone, Default)]
struct PaintNotifier {
    ready: Arc<AtomicBool>,
}

impl PaintNotifier {
    fn reset(&self) {
        self.ready.store(false, Ordering::SeqCst);
    }
}

impl RenderNotifier for PaintNotifier {
    fn clone(&self) -> Box<dyn RenderNotifier> {
        Box::new(Clone::clone(self))
    }

    fn wake_up(&self, _composite_needed: bool) {
        self.ready.store(true, Ordering::SeqCst);
    }

    fn new_frame_ready(
        &self,
        _document_id: DocumentId,
        _publish_id: FramePublishId,
        _params: &FrameReadyParams,
    ) {
        self.ready.store(true, Ordering::SeqCst);
    }

    fn external_event(&self, _evt: ExternalEvent) {}

    fn shut_down(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use webrender::api::DisplayItem;

    #[test]
    fn converts_background_and_text_rects_to_webrender_rectangles() {
        let commands = vec![
            PaintCommand::Background {
                fill: Rect::new(1.0, 2.0, 30.0, 40.0),
                color: Color::rgba(10, 20, 30, 255),
                attachment: crate::display_list::BackgroundAttachment::Scroll,
                origin: crate::display_list::BackgroundBox::BorderBox,
            },
            PaintCommand::Text {
                rect: Rect::new(4.0, 5.0, 6.0, 7.0),
                color: Color::rgba(200, 100, 50, 255),
                text: "hello".into(),
            },
        ];

        let (_, list) = build_webrender_display_list(&commands, (100, 80)).unwrap();
        let mut iter = list.iter();
        let mut rectangles = 0;
        while let Some(item) = iter.next() {
            if matches!(item.item(), DisplayItem::Rectangle(_)) {
                rectangles += 1;
            }
        }

        assert_eq!(rectangles, 2);
    }

    #[test]
    fn rejects_zero_viewport_before_touching_gl() {
        let err = match build_webrender_display_list(&[], (0, 10)) {
            Ok(_) => panic!("zero viewport should fail"),
            Err(err) => err,
        };
        assert!(matches!(err, PaintError::InvalidViewport(0, 10)));
    }

    #[test]
    fn flips_rgba_rows_for_png_order() {
        let mut rgba = vec![1, 0, 0, 255, 2, 0, 0, 255, 3, 0, 0, 255, 4, 0, 0, 255];
        flip_rgba_rows(&mut rgba, 2, 2);
        assert_eq!(
            rgba,
            vec![3, 0, 0, 255, 4, 0, 0, 255, 1, 0, 0, 255, 2, 0, 0, 255,]
        );
    }

    #[test]
    fn color_channels_are_normalized() {
        let color = color_f(Color::rgba(128, 64, 32, 255));
        assert!((color.r - 128.0 / 255.0).abs() < f32::EPSILON);
        assert!((color.g - 64.0 / 255.0).abs() < f32::EPSILON);
        assert!((color.b - 32.0 / 255.0).abs() < f32::EPSILON);
        assert_eq!(color.a, 1.0);
    }
}
