//! Factory tab component for GTK presentation state and paint snapshots.

#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::glib;
use gtk4::prelude::*;
use relm4::factory::{DynamicIndex, FactoryComponent, FactorySender};
use vixen_api::{BrowsingContextState, DocumentId};
use vixen_engine::browser::PaintSnapshot;

use crate::address::START_URI;
use crate::engine_worker::ShellTabId;
use crate::surface::GlAreaRenderer;

type SharedPaintSnapshot = Rc<RefCell<Option<Rc<PaintSnapshot>>>>;

#[derive(Debug, Clone)]
pub struct TabInit {
    pub id: ShellTabId,
    pub start_uri: String,
}

#[derive(Debug, Clone)]
pub struct TabChromeState {
    pub uri: String,
    pub title: String,
    pub status: String,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub is_loading: bool,
    pub progress: f64,
}

impl Default for TabChromeState {
    fn default() -> Self {
        Self {
            uri: START_URI.to_owned(),
            title: "Vixen".to_owned(),
            status: "Vixen - starting".to_owned(),
            can_go_back: false,
            can_go_forward: false,
            is_loading: false,
            progress: 0.0,
        }
    }
}

pub struct TabModel {
    id: ShellTabId,
    paint_snapshot: SharedPaintSnapshot,
    current_uri: String,
    pending_uri: Option<String>,
    current_title: String,
    status: String,
    can_go_back: bool,
    can_go_forward: bool,
    is_loading: bool,
    progress: f64,
    document_id: Option<DocumentId>,
    viewport: Option<(u32, u32)>,
    requested_paint: Option<(DocumentId, (u32, u32))>,
    last_error: Option<String>,
    render_generation: u64,
}

#[derive(Debug)]
pub enum TabMsg {
    Loading(Option<String>),
    State(BrowsingContextState),
    Paint(PaintSnapshot),
    ViewportChanged((u32, u32)),
    Failed(String),
    RenderFailed(String),
}

#[derive(Debug)]
pub enum TabOutput {
    StateChanged(ShellTabId, TabChromeState),
    PaintRequested(ShellTabId, DocumentId, (u32, u32)),
}

pub struct TabWidgets {
    tab_page: libadwaita::TabPage,
    status_label: gtk4::Label,
    progress_bar: gtk4::ProgressBar,
    gl_area: gtk4::GLArea,
    rendered_generation: u64,
}

impl FactoryComponent for TabModel {
    type Init = TabInit;
    type Input = TabMsg;
    type Output = TabOutput;
    type CommandOutput = ();
    type ParentWidget = libadwaita::TabView;
    type Root = gtk4::Box;
    type Widgets = TabWidgets;
    type Index = DynamicIndex;

    fn init_model(init: Self::Init, _index: &DynamicIndex, _sender: FactorySender<Self>) -> Self {
        let mut model = Self {
            id: init.id,
            paint_snapshot: Rc::new(RefCell::new(None)),
            current_uri: init.start_uri,
            pending_uri: None,
            current_title: "Loading...".to_owned(),
            status: String::new(),
            can_go_back: false,
            can_go_forward: false,
            is_loading: true,
            progress: 0.1,
            document_id: None,
            viewport: None,
            requested_paint: None,
            last_error: None,
            render_generation: 0,
        };
        model.refresh_status();
        model
    }

    fn init_root(&self) -> Self::Root {
        gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(0)
            .hexpand(true)
            .vexpand(true)
            .build()
    }

    fn init_widgets(
        &mut self,
        _index: &DynamicIndex,
        root: Self::Root,
        returned_widget: &libadwaita::TabPage,
        sender: FactorySender<Self>,
    ) -> Self::Widgets {
        let progress_bar = gtk4::ProgressBar::builder().show_text(false).build();
        progress_bar.add_css_class("osd");
        let gl_area = gtk4::GLArea::builder()
            .hexpand(true)
            .vexpand(true)
            .auto_render(true)
            .has_depth_buffer(false)
            .has_stencil_buffer(false)
            .build();
        let status_label = gtk4::Label::builder()
            .xalign(0.0)
            .single_line_mode(true)
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .margin_start(12)
            .margin_end(12)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        status_label.add_css_class("dim-label");

        root.append(&progress_bar);
        root.append(&gl_area);
        root.append(&status_label);
        connect_renderer(&sender, &gl_area, self.paint_snapshot.clone());

        returned_widget.set_title(&self.tab_title());
        returned_widget.set_tooltip(&self.status);
        returned_widget.set_loading(self.is_loading);

        Self::Widgets {
            tab_page: returned_widget.clone(),
            status_label,
            progress_bar,
            gl_area,
            rendered_generation: 0,
        }
    }

    fn update(&mut self, message: Self::Input, sender: FactorySender<Self>) {
        let chrome_changed = match message {
            TabMsg::Loading(uri) => {
                self.pending_uri = uri;
                self.current_title = "Loading...".to_owned();
                self.is_loading = true;
                self.progress = 0.1;
                self.last_error = None;
                true
            }
            TabMsg::State(state) => {
                self.apply_state(state);
                self.request_paint(&sender);
                true
            }
            TabMsg::Paint(snapshot) => {
                self.apply_paint_snapshot(snapshot);
                false
            }
            TabMsg::ViewportChanged(viewport) => {
                self.apply_viewport(viewport);
                self.request_paint(&sender);
                false
            }
            TabMsg::Failed(message) | TabMsg::RenderFailed(message) => {
                self.pending_uri = None;
                self.current_title = "Load failed".to_owned();
                self.is_loading = false;
                self.progress = 0.0;
                self.last_error = Some(message);
                true
            }
        };
        self.refresh_status();
        if chrome_changed {
            let _ = sender.output(TabOutput::StateChanged(self.id, self.chrome_state()));
        }
    }

    fn update_view(&self, widgets: &mut Self::Widgets, _sender: FactorySender<Self>) {
        widgets
            .progress_bar
            .set_fraction(self.progress.clamp(0.0, 1.0));
        widgets.status_label.set_label(&self.status);
        widgets.tab_page.set_title(&self.tab_title());
        widgets.tab_page.set_tooltip(&self.status);
        widgets.tab_page.set_loading(self.is_loading);
        if widgets.rendered_generation != self.render_generation {
            widgets.rendered_generation = self.render_generation;
            widgets.gl_area.queue_render();
        }
    }
}

impl TabModel {
    pub fn id(&self) -> ShellTabId {
        self.id
    }

    pub fn chrome_state(&self) -> TabChromeState {
        TabChromeState {
            uri: self
                .pending_uri
                .clone()
                .unwrap_or_else(|| self.current_uri.clone()),
            title: self.current_title.clone(),
            status: self.status.clone(),
            can_go_back: self.can_go_back,
            can_go_forward: self.can_go_forward,
            is_loading: self.is_loading,
            progress: self.progress,
        }
    }

    pub fn session_uri(&self) -> String {
        self.current_uri.clone()
    }

    fn apply_state(&mut self, state: BrowsingContextState) {
        if self.document_id != Some(state.document_id) {
            self.document_id = Some(state.document_id);
            self.requested_paint = None;
            *self.paint_snapshot.borrow_mut() = None;
            self.render_generation = self.render_generation.saturating_add(1);
        }
        self.current_uri = state.url;
        self.pending_uri = None;
        self.current_title = state
            .title
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| self.current_uri.clone());
        self.can_go_back = state.can_go_back;
        self.can_go_forward = state.can_go_forward;
        self.is_loading = state.is_loading;
        self.progress = state.load_progress;
        self.last_error = None;
    }

    fn apply_viewport(&mut self, viewport: (u32, u32)) {
        let viewport = (viewport.0 > 0 && viewport.1 > 0).then_some(viewport);
        if self.viewport != viewport {
            self.viewport = viewport;
            self.requested_paint = None;
        }
    }

    fn request_paint(&mut self, sender: &FactorySender<Self>) {
        let (Some(document_id), Some(viewport)) = (self.document_id, self.viewport) else {
            return;
        };
        let request = (document_id, viewport);
        if self.requested_paint == Some(request) {
            return;
        }
        self.requested_paint = Some(request);
        let _ = sender.output(TabOutput::PaintRequested(self.id, document_id, viewport));
    }

    fn apply_paint_snapshot(&mut self, snapshot: PaintSnapshot) {
        if self.document_id != Some(snapshot.document_id)
            || self.viewport != Some(snapshot.viewport)
        {
            return;
        }
        *self.paint_snapshot.borrow_mut() = Some(Rc::new(snapshot));
        self.render_generation = self.render_generation.saturating_add(1);
    }

    fn refresh_status(&mut self) {
        self.status = if let Some(error) = self.last_error.as_deref() {
            format!("{} - {} - {error}", self.current_title, self.current_uri)
        } else if self.is_loading {
            format!("{} - loading {}", self.current_title, self.current_uri)
        } else {
            format!("{} - {}", self.current_title, self.current_uri)
        };
    }

    fn tab_title(&self) -> String {
        if self.current_title.trim().is_empty() {
            format!("Tab {}", self.id.get().saturating_add(1))
        } else {
            self.current_title.clone()
        }
    }
}

fn connect_renderer(
    sender: &FactorySender<TabModel>,
    gl_area: &gtk4::GLArea,
    snapshot: SharedPaintSnapshot,
) {
    let renderer = Rc::new(RefCell::new(GlAreaRenderer::new(gl_area)));
    let render_sender = sender.clone();
    let render_snapshot = snapshot.clone();
    let render_state = renderer.clone();
    gl_area.connect_render(move |area, _| {
        let Some(snapshot) = render_snapshot.borrow().clone() else {
            return glib::Propagation::Proceed;
        };
        if snapshot.viewport != drawable_size(area) {
            return glib::Propagation::Proceed;
        }
        match render_state
            .borrow_mut()
            .render_commands(&snapshot.commands, snapshot.viewport)
        {
            Ok(()) => glib::Propagation::Stop,
            Err(error) => {
                render_sender.input(TabMsg::RenderFailed(error.to_string()));
                glib::Propagation::Stop
            }
        }
    });

    let resize_sender = sender.clone();
    let resize_renderer = renderer.clone();
    gl_area.connect_resize(move |area, _, _| {
        resize_renderer.borrow_mut().reset_renderer();
        resize_sender.input(TabMsg::ViewportChanged(drawable_size(area)));
        area.queue_render();
    });

    gl_area.connect_unrealize(move |_| {
        renderer.borrow_mut().reset_renderer();
    });
}

fn drawable_size(area: &gtk4::GLArea) -> (u32, u32) {
    let scale = u32::try_from(area.scale_factor()).unwrap_or(1).max(1);
    let width = u32::try_from(area.width()).unwrap_or(0);
    let height = u32::try_from(area.height()).unwrap_or(0);
    (width.saturating_mul(scale), height.saturating_mul(scale))
}
