//! Relm4/libadwaita browser window.
//!
//! The app owns one BrowserCore worker shared by every tab. Factory tabs own
//! only GTK presentation state and immutable paint snapshots.

#![forbid(unsafe_code)]

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk4::glib;
use gtk4::prelude::*;
use libadwaita::prelude::*;
use relm4::factory::FactoryVecDeque;
use relm4::{
    Component, ComponentParts, ComponentSender, RelmApp, SimpleComponent, WorkerController,
};
use vixen_api::ProfileSessionState;

use crate::address::{START_URI, normalize_address};
use crate::config;
use crate::engine_worker::{EngineCommand, EngineEvent, EngineInit, EngineWorker, ShellTabId};
use crate::profile;
use crate::tab::{TabChromeState, TabInit, TabModel, TabMsg, TabOutput};

pub fn run() {
    RelmApp::new(config::APP_ID).run::<BrowserApp>(BrowserInit::load());
}

#[derive(Debug, Clone)]
struct BrowserInit {
    profile_database: Result<PathBuf, String>,
}

impl BrowserInit {
    fn load() -> Self {
        let profile_database = profile::production_paths()
            .map_err(|error| error.to_string())
            .and_then(|paths| {
                profile::prepare_directories(&paths)
                    .map_err(|error| error.to_string())
                    .map(|()| paths.database)
            });
        Self { profile_database }
    }
}

struct BrowserApp {
    engine: WorkerController<EngineWorker>,
    tabs: FactoryVecDeque<TabModel>,
    selected_index: usize,
    selected_state: TabChromeState,
    next_tab_id: u64,
    closing_from_model: Rc<Cell<bool>>,
}

#[derive(Debug)]
enum AppMsg {
    NewTab,
    CloseCurrentTab,
    CloseIndex(usize),
    SelectionChanged(usize),
    NavigateSelected(String),
    ReloadSelected,
    StopSelected,
    BackSelected,
    ForwardSelected,
    TabStateChanged(ShellTabId, TabChromeState),
    PaintRequested(ShellTabId, vixen_api::DocumentId, (u32, u32)),
    Engine(EngineEvent),
}

struct BrowserWidgets {
    back_button: gtk4::Button,
    forward_button: gtk4::Button,
    reload_button: gtk4::Button,
    stop_button: gtk4::Button,
    close_tab_button: gtk4::Button,
    location_entry: gtk4::Entry,
    status_label: gtk4::Label,
    progress_bar: gtk4::ProgressBar,
}

impl SimpleComponent for BrowserApp {
    type Init = BrowserInit;
    type Input = AppMsg;
    type Output = ();
    type Root = libadwaita::ApplicationWindow;
    type Widgets = BrowserWidgets;

    fn init_root() -> Self::Root {
        libadwaita::ApplicationWindow::builder()
            .title("Vixen")
            .default_width(1100)
            .default_height(820)
            .build()
    }

    fn init(
        init: Self::Init,
        window: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let engine = EngineWorker::builder()
            .detach_worker(EngineInit {
                profile_database: init.profile_database,
            })
            .forward(sender.input_sender(), AppMsg::Engine);
        let tabs = FactoryVecDeque::builder()
            .launch(libadwaita::TabView::default())
            .forward(sender.input_sender(), |output| match output {
                TabOutput::StateChanged(tab_id, state) => AppMsg::TabStateChanged(tab_id, state),
                TabOutput::PaintRequested(tab_id, document_id, viewport) => {
                    AppMsg::PaintRequested(tab_id, document_id, viewport)
                }
            });

        let closing_from_model = Rc::new(Cell::new(false));
        let model = BrowserApp {
            engine,
            tabs,
            selected_index: 0,
            selected_state: TabChromeState::default(),
            next_tab_id: 0,
            closing_from_model: closing_from_model.clone(),
        };

        let tab_view = model.tabs.widget().clone();
        let header = libadwaita::HeaderBar::new();
        let back_button = toolbar_button("go-previous-symbolic", "Back");
        let forward_button = toolbar_button("go-next-symbolic", "Forward");
        let reload_button = toolbar_button("view-refresh-symbolic", "Reload");
        let stop_button = toolbar_button("process-stop-symbolic", "Stop");
        let new_tab_button = toolbar_button("list-add-symbolic", "New tab");
        let close_tab_button = toolbar_button("window-close-symbolic", "Close tab");
        let location_entry = gtk4::Entry::builder()
            .hexpand(true)
            .placeholder_text("Enter URL")
            .build();
        header.pack_start(&back_button);
        header.pack_start(&forward_button);
        header.pack_start(&reload_button);
        header.pack_start(&stop_button);
        header.set_title_widget(Some(&location_entry));
        header.pack_end(&close_tab_button);
        header.pack_end(&new_tab_button);

        let tab_bar = libadwaita::TabBar::new();
        tab_bar.set_view(Some(&tab_view));
        tab_bar.set_autohide(false);
        tab_bar.set_expand_tabs(true);

        let progress_bar = gtk4::ProgressBar::builder().show_text(false).build();
        progress_bar.add_css_class("osd");
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

        let content = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(0)
            .build();
        content.append(&header);
        content.append(&tab_bar);
        content.append(&progress_bar);
        content.append(&tab_view);
        content.append(&status_label);
        window.set_content(Some(&content));

        connect_controls(
            &sender,
            &back_button,
            &forward_button,
            &reload_button,
            &stop_button,
            &new_tab_button,
            &close_tab_button,
            &location_entry,
        );
        connect_tab_view(&sender, &tab_view, closing_from_model);

        let widgets = BrowserWidgets {
            back_button,
            forward_button,
            reload_button,
            stop_button,
            close_tab_button,
            location_entry,
            status_label,
            progress_bar,
        };

        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, _sender: ComponentSender<Self>) {
        match message {
            AppMsg::NewTab => self.add_tab(START_URI.to_owned()),
            AppMsg::CloseCurrentTab => self.close_tab(self.selected_index),
            AppMsg::CloseIndex(index) => self.close_tab(index),
            AppMsg::SelectionChanged(index) => {
                self.selected_index = index.min(self.tabs.len().saturating_sub(1));
                self.refresh_selected_state();
                if let Some(tab_id) = self.selected_tab_id() {
                    self.engine.emit(EngineCommand::Activate(tab_id));
                }
                self.persist_session();
            }
            AppMsg::NavigateSelected(input) => self.navigate_selected(input),
            AppMsg::ReloadSelected => self.route_selected_navigation(EngineCommand::Reload),
            AppMsg::StopSelected => {
                if let Some(tab_id) = self.selected_tab_id() {
                    self.engine.emit(EngineCommand::Stop(tab_id));
                }
            }
            AppMsg::BackSelected => self.route_selected_navigation(EngineCommand::Back),
            AppMsg::ForwardSelected => self.route_selected_navigation(EngineCommand::Forward),
            AppMsg::TabStateChanged(tab_id, state) => {
                let loaded = !state.is_loading;
                if self.selected_tab_id() == Some(tab_id) {
                    self.selected_state = state;
                }
                if loaded {
                    self.persist_session();
                }
            }
            AppMsg::PaintRequested(tab_id, document_id, viewport) => {
                self.engine.emit(EngineCommand::Paint {
                    tab_id,
                    document_id,
                    viewport,
                });
            }
            AppMsg::Engine(event) => self.apply_engine_event(event),
        }
    }

    fn update_view(&self, widgets: &mut Self::Widgets, _sender: ComponentSender<Self>) {
        widgets
            .back_button
            .set_sensitive(self.selected_state.can_go_back);
        widgets
            .forward_button
            .set_sensitive(self.selected_state.can_go_forward);
        widgets
            .reload_button
            .set_sensitive(!self.selected_state.uri.is_empty());
        widgets
            .stop_button
            .set_sensitive(self.selected_state.is_loading);
        widgets.close_tab_button.set_sensitive(self.tabs.len() > 1);
        widgets
            .progress_bar
            .set_fraction(self.selected_state.progress.clamp(0.0, 1.0));
        widgets.status_label.set_label(&self.selected_state.status);
        widgets
            .location_entry
            .set_tooltip_text(Some(&self.selected_state.title));
        if widgets.location_entry.text().as_str() != self.selected_state.uri {
            widgets.location_entry.set_text(&self.selected_state.uri);
        }
    }
}

impl BrowserApp {
    fn restore_session(&mut self, session: ProfileSessionState) {
        if self.tabs.len() != 0 {
            return;
        }
        let tabs = if session.tabs.is_empty() {
            vec![START_URI.to_owned()]
        } else {
            session.tabs
        };
        for uri in tabs {
            self.push_tab(uri);
        }
        self.selected_index = session.active_index.min(self.tabs.len().saturating_sub(1));
        self.select_index(self.selected_index);
        self.refresh_selected_state();
        if let Some(tab_id) = self.selected_tab_id() {
            self.engine.emit(EngineCommand::Activate(tab_id));
        }
        self.persist_session();
    }

    fn apply_engine_event(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::SessionLoaded(session) => self.restore_session(session),
            EngineEvent::StateChanged { tab_id, state } => {
                self.send_to_tab(tab_id, TabMsg::State(state));
            }
            EngineEvent::PaintReady { tab_id, snapshot } => {
                self.send_to_tab(tab_id, TabMsg::Paint(snapshot));
            }
            EngineEvent::Failed {
                tab_id,
                action,
                message,
            } => {
                let detail = format!("{action}: {message}");
                if let Some(tab_id) = tab_id
                    && self.tab_index(tab_id).is_some()
                {
                    self.send_to_tab(tab_id, TabMsg::Failed(detail));
                } else {
                    eprintln!("vixen: {detail}");
                }
            }
        }
    }

    fn add_tab(&mut self, uri: String) {
        let index = self.push_tab(uri);
        self.selected_index = index;
        self.select_index(index);
        self.refresh_selected_state();
        self.persist_session();
    }

    fn push_tab(&mut self, uri: String) -> usize {
        let uri = normalize_address(&uri);
        let index = self.tabs.len();
        let tab_id = ShellTabId::new(self.next_tab_id);
        self.next_tab_id = self.next_tab_id.saturating_add(1);
        {
            let mut tabs = self.tabs.guard();
            tabs.push_back(TabInit {
                id: tab_id,
                start_uri: uri.clone(),
            });
        }
        self.engine.emit(EngineCommand::Create {
            tab_id,
            start_uri: uri,
        });
        index
    }

    fn close_tab(&mut self, index: usize) {
        if self.tabs.len() <= 1 {
            self.navigate_selected(START_URI.to_owned());
            return;
        }
        let Some(tab_id) = self.tabs.get(index).map(|tab| tab.id()) else {
            return;
        };
        self.closing_from_model.set(true);
        {
            let mut tabs = self.tabs.guard();
            tabs.remove(index);
        }
        self.closing_from_model.set(false);
        self.engine.emit(EngineCommand::Close(tab_id));
        self.selected_index = self.selected_index.min(self.tabs.len().saturating_sub(1));
        self.select_index(self.selected_index);
        self.refresh_selected_state();
        if let Some(tab_id) = self.selected_tab_id() {
            self.engine.emit(EngineCommand::Activate(tab_id));
        }
        self.persist_session();
    }

    fn navigate_selected(&self, input: String) {
        let Some(tab_id) = self.selected_tab_id() else {
            return;
        };
        let uri = normalize_address(&input);
        self.send_to_tab(tab_id, TabMsg::Loading(Some(uri.clone())));
        self.engine.emit(EngineCommand::Navigate { tab_id, uri });
    }

    fn route_selected_navigation(&self, command: fn(ShellTabId) -> EngineCommand) {
        let Some(tab_id) = self.selected_tab_id() else {
            return;
        };
        self.send_to_tab(tab_id, TabMsg::Loading(None));
        self.engine.emit(command(tab_id));
    }

    fn send_to_tab(&self, tab_id: ShellTabId, message: TabMsg) {
        if let Some(index) = self.tab_index(tab_id) {
            self.tabs.send(index, message);
        }
    }

    fn tab_index(&self, tab_id: ShellTabId) -> Option<usize> {
        (0..self.tabs.len())
            .find(|&index| self.tabs.get(index).is_some_and(|tab| tab.id() == tab_id))
    }

    fn selected_tab_id(&self) -> Option<ShellTabId> {
        self.tabs.get(self.selected_index).map(|tab| tab.id())
    }

    fn refresh_selected_state(&mut self) {
        self.selected_state = self
            .tabs
            .get(self.selected_index)
            .map(|tab| tab.chrome_state())
            .unwrap_or_default();
    }

    fn select_index(&self, target: usize) {
        let tab_view = self.tabs.widget();
        let Some(mut current) = selected_position(tab_view) else {
            return;
        };
        while current < target {
            if !tab_view.select_next_page() {
                break;
            }
            current += 1;
        }
        while current > target {
            if !tab_view.select_previous_page() {
                break;
            }
            current -= 1;
        }
    }

    fn persist_session(&self) {
        let tabs = (0..self.tabs.len())
            .filter_map(|index| self.tabs.get(index))
            .map(|tab| tab.session_uri())
            .filter(|uri| !uri.trim().is_empty())
            .collect::<Vec<_>>();
        if tabs.is_empty() {
            return;
        }
        self.engine
            .emit(EngineCommand::SaveSession(ProfileSessionState {
                tabs,
                active_index: self.selected_index,
            }));
    }
}

fn selected_position(tab_view: &libadwaita::TabView) -> Option<usize> {
    let page = tab_view.selected_page()?;
    usize::try_from(tab_view.page_position(&page)).ok()
}

fn toolbar_button(icon_name: &str, tooltip: &str) -> gtk4::Button {
    gtk4::Button::builder()
        .icon_name(icon_name)
        .tooltip_text(tooltip)
        .build()
}

fn connect_controls(
    sender: &ComponentSender<BrowserApp>,
    back_button: &gtk4::Button,
    forward_button: &gtk4::Button,
    reload_button: &gtk4::Button,
    stop_button: &gtk4::Button,
    new_tab_button: &gtk4::Button,
    close_tab_button: &gtk4::Button,
    location_entry: &gtk4::Entry,
) {
    let input = sender.clone();
    back_button.connect_clicked(move |_| input.input(AppMsg::BackSelected));
    let input = sender.clone();
    forward_button.connect_clicked(move |_| input.input(AppMsg::ForwardSelected));
    let input = sender.clone();
    reload_button.connect_clicked(move |_| input.input(AppMsg::ReloadSelected));
    let input = sender.clone();
    stop_button.connect_clicked(move |_| input.input(AppMsg::StopSelected));
    let input = sender.clone();
    new_tab_button.connect_clicked(move |_| input.input(AppMsg::NewTab));
    let input = sender.clone();
    close_tab_button.connect_clicked(move |_| input.input(AppMsg::CloseCurrentTab));
    let input = sender.clone();
    location_entry.connect_activate(move |entry| {
        input.input(AppMsg::NavigateSelected(entry.text().to_string()));
    });
}

fn connect_tab_view(
    sender: &ComponentSender<BrowserApp>,
    tab_view: &libadwaita::TabView,
    closing_from_model: Rc<Cell<bool>>,
) {
    let input = sender.clone();
    tab_view.connect_selected_page_notify(move |view| {
        if let Some(index) = selected_position(view) {
            input.input(AppMsg::SelectionChanged(index));
        }
    });

    let input = sender.clone();
    tab_view.connect_close_page(move |view, page| {
        if closing_from_model.get() {
            return glib::Propagation::Proceed;
        }
        if let Ok(index) = usize::try_from(view.page_position(page)) {
            input.input(AppMsg::CloseIndex(index));
        }
        glib::Propagation::Stop
    });
}
