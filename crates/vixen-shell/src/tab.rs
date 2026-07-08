//! Factory tab component for the GTK shell.

#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::glib;
use gtk4::prelude::*;
use relm4::factory::{DynamicIndex, FactoryComponent, FactorySender};
use relm4::{Component, WorkerController};
use vixen_api::{EngineDiagnostic, EngineDiagnosticCategory};
use vixen_engine::page::Page;

use crate::engine_worker::{EngineCommand, EngineEvent, EngineWorker, START_URI, WorkerState};
use crate::surface::GlAreaRenderer;

type SharedPage = Rc<RefCell<Option<Page>>>;

#[derive(Debug, Clone)]
pub struct TabInit {
    pub id: u64,
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
            status: "Vixen — starting".to_owned(),
            can_go_back: false,
            can_go_forward: false,
            is_loading: false,
            progress: 0.0,
        }
    }
}

pub struct TabModel {
    index: DynamicIndex,
    id: u64,
    worker: WorkerController<EngineWorker>,
    shared_page: SharedPage,
    current_uri: String,
    current_title: String,
    status: String,
    can_go_back: bool,
    can_go_forward: bool,
    is_loading: bool,
    progress: f64,
    diagnostics: Vec<EngineDiagnostic>,
    render_generation: u64,
}

#[derive(Debug)]
pub enum TabMsg {
    Navigate(String),
    Reload,
    Stop,
    Back,
    Forward,
    Worker(EngineEvent),
    RenderFailed(String),
}

#[derive(Debug)]
pub enum TabOutput {
    StateChanged(DynamicIndex, TabChromeState),
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

    fn init_model(init: Self::Init, index: &DynamicIndex, sender: FactorySender<Self>) -> Self {
        let worker = EngineWorker::builder()
            .detach_worker(())
            .forward(sender.input_sender(), TabMsg::Worker);
        let mut model = Self {
            index: index.clone(),
            id: init.id,
            worker,
            shared_page: Rc::new(RefCell::new(None)),
            current_uri: init.start_uri.clone(),
            current_title: "Vixen".to_owned(),
            status: String::new(),
            can_go_back: false,
            can_go_forward: false,
            is_loading: true,
            progress: 0.0,
            diagnostics: Vec::new(),
            render_generation: 0,
        };
        model.refresh_status();
        model.worker.emit(EngineCommand::Navigate(init.start_uri));
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
        connect_renderer(&sender, &gl_area, self.shared_page.clone());

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
        match message {
            TabMsg::Navigate(input) => {
                self.start_load(input.clone());
                self.worker.emit(EngineCommand::Navigate(input));
            }
            TabMsg::Reload => {
                self.start_load(self.current_uri.clone());
                self.worker.emit(EngineCommand::Reload);
            }
            TabMsg::Stop => {
                self.is_loading = false;
                self.worker.emit(EngineCommand::Stop);
            }
            TabMsg::Back => {
                self.is_loading = true;
                self.progress = 0.1;
                self.worker.emit(EngineCommand::Back);
            }
            TabMsg::Forward => {
                self.is_loading = true;
                self.progress = 0.1;
                self.worker.emit(EngineCommand::Forward);
            }
            TabMsg::Worker(event) => self.apply_worker_event(event),
            TabMsg::RenderFailed(message) => {
                self.diagnostics = vec![EngineDiagnostic::new(
                    EngineDiagnosticCategory::LayoutRender,
                    "shell.render",
                    message,
                )];
            }
        }
        self.refresh_status();
        let _ = sender.output(TabOutput::StateChanged(
            self.index.clone(),
            self.chrome_state(),
        ));
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
    pub fn chrome_state(&self) -> TabChromeState {
        TabChromeState {
            uri: self.current_uri.clone(),
            title: self.current_title.clone(),
            status: self.status.clone(),
            can_go_back: self.can_go_back,
            can_go_forward: self.can_go_forward,
            is_loading: self.is_loading,
            progress: self.progress,
        }
    }

    fn start_load(&mut self, input: String) {
        self.current_uri = input;
        self.current_title = "Loading…".to_owned();
        self.is_loading = true;
        self.progress = 0.1;
        self.diagnostics.clear();
    }

    fn apply_worker_event(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::Progress(state) => self.apply_worker_state(state),
            EngineEvent::Loaded {
                state,
                final_uri,
                html,
            } => {
                self.apply_worker_state(state);
                self.load_html(final_uri, html);
            }
            EngineEvent::Failed {
                state,
                attempted_uri,
                message,
                diagnostics,
            } => {
                self.apply_worker_state(state);
                self.current_uri = attempted_uri;
                self.current_title = "Load failed".to_owned();
                self.diagnostics = diagnostics;
                self.load_error_page(&message);
            }
            EngineEvent::Stopped(state) => self.apply_worker_state(state),
        }
    }

    fn apply_worker_state(&mut self, state: WorkerState) {
        if let Some(uri) = state.current_uri {
            self.current_uri = uri;
        }
        self.can_go_back = state.can_go_back;
        self.can_go_forward = state.can_go_forward;
        self.is_loading = state.is_loading;
        self.progress = state.progress;
    }

    fn load_html(&mut self, uri: String, html: String) {
        match Page::from_html(uri.clone(), &html) {
            Ok(page) => {
                self.current_uri = uri;
                self.current_title = page_title(&page).unwrap_or_else(|| self.current_uri.clone());
                self.diagnostics = page.diagnostics();
                *self.shared_page.borrow_mut() = Some(page);
                self.is_loading = false;
                self.progress = 1.0;
                self.render_generation = self.render_generation.saturating_add(1);
            }
            Err(error) => {
                self.current_title = "Parse failed".to_owned();
                self.diagnostics = vec![EngineDiagnostic::new(
                    EngineDiagnosticCategory::ParseDom,
                    "shell.parse",
                    format!("parse failed: {error}"),
                )];
                self.is_loading = false;
                self.progress = 0.0;
            }
        }
    }

    fn load_error_page(&mut self, message: &str) {
        let html = format!(
            "<!doctype html><html><head><title>Load failed</title><style>body{{font:18px sans-serif;margin:48px;color:#fee2e2;background:#450a0a}}pre{{white-space:pre-wrap}}</style></head><body><h1>Load failed</h1><pre>{}</pre></body></html>",
            escape_html(message)
        );
        if let Ok(page) = Page::from_html("about:vixen-error", &html) {
            *self.shared_page.borrow_mut() = Some(page);
            self.render_generation = self.render_generation.saturating_add(1);
        }
    }

    fn refresh_status(&mut self) {
        let diagnostic = self.diagnostics.first();
        self.status = if let Some(diag) = diagnostic {
            format!(
                "{} — {} — {}: {}",
                self.current_title, self.current_uri, diag.code, diag.message
            )
        } else if self.is_loading {
            format!("{} — loading {}", self.current_title, self.current_uri)
        } else {
            format!("{} — {}", self.current_title, self.current_uri)
        };
    }

    fn tab_title(&self) -> String {
        if self.current_title.trim().is_empty() {
            format!("Tab {}", self.id + 1)
        } else {
            self.current_title.clone()
        }
    }
}

fn page_title(page: &Page) -> Option<String> {
    let title = page.document().title().unwrap_or_default();
    (!title.trim().is_empty()).then_some(title)
}

fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn connect_renderer(sender: &FactorySender<TabModel>, gl_area: &gtk4::GLArea, page: SharedPage) {
    let renderer = Rc::new(RefCell::new(GlAreaRenderer::new(gl_area)));
    let render_sender = sender.clone();
    let render_page = page.clone();
    let render_state = renderer.clone();
    gl_area.connect_render(move |_, _| {
        let page = render_page.borrow();
        let Some(page) = page.as_ref() else {
            return glib::Propagation::Proceed;
        };
        match render_state.borrow_mut().render_page(page) {
            Ok(()) => glib::Propagation::Stop,
            Err(err) => {
                render_sender.input(TabMsg::RenderFailed(err.to_string()));
                glib::Propagation::Stop
            }
        }
    });

    let resize_renderer = renderer.clone();
    gl_area.connect_resize(move |area, _, _| {
        resize_renderer.borrow_mut().reset_renderer();
        area.queue_render();
    });

    gl_area.connect_unrealize(move |_| {
        renderer.borrow_mut().reset_renderer();
    });
}
