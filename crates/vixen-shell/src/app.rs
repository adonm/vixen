//! Relm4/libadwaita browser window.
//!
//! The shell is intentionally small but now follows the planned shape: the
//! window owns a `FactoryVecDeque<TabModel>`, each tab owns a background
//! `EngineWorker` for navigation/fetch work, and GTK keeps the non-`Send` page
//! plus `GLArea` paint state on the main thread.

#![forbid(unsafe_code)]

use std::cell::Cell;
use std::rc::Rc;

use gtk4::glib;
use gtk4::prelude::*;
use libadwaita::prelude::*;
use relm4::factory::{DynamicIndex, FactoryVecDeque};
use relm4::{ComponentParts, ComponentSender, RelmApp, SimpleComponent};

use crate::config;
use crate::engine_worker::START_URI;
use crate::tab::{TabChromeState, TabInit, TabModel, TabMsg, TabOutput};

pub fn run() {
    RelmApp::new(config::APP_ID).run::<BrowserApp>(START_URI.to_owned());
}

struct BrowserApp {
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
    TabStateChanged(DynamicIndex, TabChromeState),
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
    type Init = String;
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
        start_uri: Self::Init,
        window: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let tabs = FactoryVecDeque::builder()
            .launch(libadwaita::TabView::default())
            .forward(sender.input_sender(), |output| match output {
                TabOutput::StateChanged(index, state) => AppMsg::TabStateChanged(index, state),
            });

        let closing_from_model = Rc::new(Cell::new(false));
        let mut model = BrowserApp {
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

        model.add_tab(start_uri);
        model.refresh_selected_state();

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
            }
            AppMsg::NavigateSelected(input) => {
                self.send_to_selected(TabMsg::Navigate(input));
            }
            AppMsg::ReloadSelected => self.send_to_selected(TabMsg::Reload),
            AppMsg::StopSelected => self.send_to_selected(TabMsg::Stop),
            AppMsg::BackSelected => self.send_to_selected(TabMsg::Back),
            AppMsg::ForwardSelected => self.send_to_selected(TabMsg::Forward),
            AppMsg::TabStateChanged(index, state) => {
                if index.current_index() == self.selected_index {
                    self.selected_state = state;
                }
            }
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
    fn add_tab(&mut self, uri: String) {
        let index = self.tabs.len();
        let tab_id = self.next_tab_id;
        self.next_tab_id = self.next_tab_id.saturating_add(1);
        {
            let mut tabs = self.tabs.guard();
            tabs.push_back(TabInit {
                id: tab_id,
                start_uri: uri,
            });
        }
        self.selected_index = index;
        self.select_index(index);
        self.refresh_selected_state();
    }

    fn close_tab(&mut self, index: usize) {
        if self.tabs.len() <= 1 {
            self.send_to_selected(TabMsg::Navigate(START_URI.to_owned()));
            return;
        }
        if index >= self.tabs.len() {
            return;
        }
        self.closing_from_model.set(true);
        {
            let mut tabs = self.tabs.guard();
            tabs.remove(index);
        }
        self.closing_from_model.set(false);
        self.selected_index = self.selected_index.min(self.tabs.len().saturating_sub(1));
        self.select_index(self.selected_index);
        self.refresh_selected_state();
    }

    fn send_to_selected(&self, message: TabMsg) {
        if self.selected_index < self.tabs.len() {
            self.tabs.send(self.selected_index, message);
        }
    }

    fn refresh_selected_state(&mut self) {
        self.selected_state = self
            .tabs
            .get(self.selected_index)
            .map(TabModel::chrome_state)
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
