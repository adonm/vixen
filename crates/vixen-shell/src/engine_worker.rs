//! App-level BrowserCore worker for the GTK shell.
//!
//! One worker owns one [`ShellBrowser`] and routes every tab operation through
//! stable shell tab IDs. Tabs receive only presentation state and immutable
//! paint snapshots.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use relm4::{ComponentSender, Worker};
use vixen_api::{
    BrowserError, BrowsingContextId, BrowsingContextState, DocumentId, ProfileSessionState,
};
use vixen_engine::browser::PaintSnapshot;

use crate::browser_adapter::ShellBrowser;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ShellTabId(u64);

impl ShellTabId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug)]
pub struct EngineInit {
    pub profile_database: Result<PathBuf, String>,
}

pub struct EngineWorker {
    browser: Option<ShellBrowser>,
    contexts: BTreeMap<ShellTabId, BrowsingContextId>,
}

#[derive(Debug)]
pub enum EngineCommand {
    Create {
        tab_id: ShellTabId,
        start_uri: String,
    },
    Close(ShellTabId),
    Activate(ShellTabId),
    Navigate {
        tab_id: ShellTabId,
        uri: String,
    },
    Reload(ShellTabId),
    Stop(ShellTabId),
    Back(ShellTabId),
    Forward(ShellTabId),
    Paint {
        tab_id: ShellTabId,
        document_id: DocumentId,
        viewport: (u32, u32),
    },
    SaveSession(ProfileSessionState),
}

#[derive(Debug)]
pub enum EngineEvent {
    SessionLoaded(ProfileSessionState),
    StateChanged {
        tab_id: ShellTabId,
        state: BrowsingContextState,
    },
    PaintReady {
        tab_id: ShellTabId,
        snapshot: PaintSnapshot,
    },
    Failed {
        tab_id: Option<ShellTabId>,
        action: &'static str,
        message: String,
    },
}

impl Worker for EngineWorker {
    type Init = EngineInit;
    type Input = EngineCommand;
    type Output = EngineEvent;

    fn init(init: Self::Init, sender: ComponentSender<Self>) -> Self {
        let mut worker = Self {
            browser: None,
            contexts: BTreeMap::new(),
        };

        match init.profile_database {
            Ok(database) => match ShellBrowser::open(database) {
                Ok(mut browser) => {
                    let session = match browser.load_profile_session() {
                        Ok(session) => session,
                        Err(error) => {
                            report_error(&sender, None, "load profile session", error);
                            ProfileSessionState::default()
                        }
                    };
                    worker.browser = Some(browser);
                    let _ = sender.output(EngineEvent::SessionLoaded(session));
                }
                Err(error) => {
                    report_error(&sender, None, "open browser profile", error);
                    let _ =
                        sender.output(EngineEvent::SessionLoaded(ProfileSessionState::default()));
                }
            },
            Err(error) => {
                report_error(&sender, None, "resolve browser profile", error);
                let _ = sender.output(EngineEvent::SessionLoaded(ProfileSessionState::default()));
            }
        }

        worker
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>) {
        match message {
            EngineCommand::Create { tab_id, start_uri } => self.create(tab_id, start_uri, &sender),
            EngineCommand::Close(tab_id) => self.close(tab_id, &sender),
            EngineCommand::Activate(tab_id) => self.activate(tab_id, &sender),
            EngineCommand::Navigate { tab_id, uri } => {
                self.update_context(tab_id, "navigate", &sender, |browser, context_id| {
                    browser.navigate(context_id, uri)
                });
            }
            EngineCommand::Reload(tab_id) => {
                self.update_context(tab_id, "reload", &sender, ShellBrowser::reload)
            }
            EngineCommand::Stop(tab_id) => {
                self.update_context(tab_id, "stop", &sender, ShellBrowser::stop)
            }
            EngineCommand::Back(tab_id) => {
                self.update_context(tab_id, "go back", &sender, ShellBrowser::go_back)
            }
            EngineCommand::Forward(tab_id) => {
                self.update_context(tab_id, "go forward", &sender, ShellBrowser::go_forward)
            }
            EngineCommand::Paint {
                tab_id,
                document_id,
                viewport,
            } => self.paint(tab_id, document_id, viewport, &sender),
            EngineCommand::SaveSession(session) => self.save_session(session, &sender),
        }
    }
}

impl EngineWorker {
    fn create(&mut self, tab_id: ShellTabId, start_uri: String, sender: &ComponentSender<Self>) {
        if self.contexts.contains_key(&tab_id) {
            report_error(sender, Some(tab_id), "create tab", "duplicate shell tab ID");
            return;
        }
        let result = self
            .browser
            .as_mut()
            .ok_or_else(browser_unavailable)
            .and_then(|browser| browser.create_context().map_err(|error| error.to_string()));
        match result {
            Ok(context_id) => {
                self.contexts.insert(tab_id, context_id);
                self.update_context(
                    tab_id,
                    "initial navigation",
                    sender,
                    |browser, context_id| browser.navigate(context_id, start_uri),
                );
            }
            Err(error) => report_error(sender, Some(tab_id), "create tab", error),
        }
    }

    fn close(&mut self, tab_id: ShellTabId, sender: &ComponentSender<Self>) {
        let result = self.context_id(tab_id).and_then(|context_id| {
            self.browser
                .as_mut()
                .ok_or_else(browser_unavailable)?
                .close_context(context_id)
                .map_err(|error| error.to_string())
        });
        match result {
            Ok(()) => {
                self.contexts.remove(&tab_id);
            }
            Err(error) => report_error(sender, Some(tab_id), "close tab", error),
        }
    }

    fn activate(&mut self, tab_id: ShellTabId, sender: &ComponentSender<Self>) {
        let result = self.context_id(tab_id).and_then(|context_id| {
            self.browser
                .as_mut()
                .ok_or_else(browser_unavailable)?
                .activate_context(context_id)
                .map_err(|error| error.to_string())
        });
        if let Err(error) = result {
            report_error(sender, Some(tab_id), "activate tab", error);
        }
    }

    fn update_context(
        &mut self,
        tab_id: ShellTabId,
        action: &'static str,
        sender: &ComponentSender<Self>,
        operation: impl FnOnce(
            &mut ShellBrowser,
            BrowsingContextId,
        ) -> Result<BrowsingContextState, BrowserError>,
    ) {
        let result = self.context_id(tab_id).and_then(|context_id| {
            operation(
                self.browser.as_mut().ok_or_else(browser_unavailable)?,
                context_id,
            )
            .map_err(|error| error.to_string())
        });
        match result {
            Ok(state) => {
                let _ = sender.output(EngineEvent::StateChanged { tab_id, state });
            }
            Err(error) => report_error(sender, Some(tab_id), action, error),
        }
    }

    fn paint(
        &mut self,
        tab_id: ShellTabId,
        document_id: DocumentId,
        viewport: (u32, u32),
        sender: &ComponentSender<Self>,
    ) {
        let result = self.context_id(tab_id).and_then(|context_id| {
            self.browser
                .as_mut()
                .ok_or_else(browser_unavailable)?
                .paint_snapshot_for_document(context_id, document_id, viewport)
                .map_err(|error| error.to_string())
        });
        match result {
            Ok(snapshot) => {
                let _ = sender.output(EngineEvent::PaintReady { tab_id, snapshot });
            }
            Err(error) => report_error(sender, Some(tab_id), "capture paint", error),
        }
    }

    fn save_session(&mut self, session: ProfileSessionState, sender: &ComponentSender<Self>) {
        let result = self
            .browser
            .as_mut()
            .ok_or_else(browser_unavailable)
            .and_then(|browser| {
                browser
                    .save_profile_session(session)
                    .map_err(|error| error.to_string())
            });
        if let Err(error) = result {
            report_error(sender, None, "save profile session", error);
        }
    }

    fn context_id(&self, tab_id: ShellTabId) -> Result<BrowsingContextId, String> {
        self.contexts
            .get(&tab_id)
            .copied()
            .ok_or_else(|| format!("unknown shell tab {}", tab_id.get()))
    }
}

fn browser_unavailable() -> String {
    "browser core is unavailable".to_owned()
}

fn report_error(
    sender: &ComponentSender<EngineWorker>,
    tab_id: Option<ShellTabId>,
    action: &'static str,
    error: impl ToString,
) {
    let _ = sender.output(EngineEvent::Failed {
        tab_id,
        action,
        message: error.to_string(),
    });
}
