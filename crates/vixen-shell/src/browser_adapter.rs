//! GTK-independent shell adapter over the engine-owned browser core.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use vixen_api::{
    BrowserCommand, BrowserCommandResult, BrowserError, BrowserEvent, BrowsingContextId,
    BrowsingContextState, NavigationId, NavigationPhase, ProfileDataSelection, ProfileSessionState,
    browser_error_codes,
};
use vixen_engine::browser::{BrowserConfig, EngineBrowserHandle, PaintSnapshot, spawn_browser};

const NAVIGATION_WAIT_TIMEOUT: Duration = Duration::from_secs(35);

/// One profile/browser owner shared by all shell tabs.
///
/// The adapter caches only typed presentation snapshots returned or emitted by
/// BrowserCore; it owns no Page, runtime, loader, cookie jar, store, or history
/// vector.
pub struct ShellBrowser {
    handle: EngineBrowserHandle,
    states: BTreeMap<BrowsingContextId, BrowsingContextState>,
    active_context: Option<BrowsingContextId>,
}

impl ShellBrowser {
    pub fn open(profile_path: impl Into<PathBuf>) -> Result<Self, BrowserError> {
        Self::from_config(BrowserConfig::new(profile_path))
    }

    pub fn from_config(config: BrowserConfig) -> Result<Self, BrowserError> {
        Ok(Self {
            handle: spawn_browser(config)?,
            states: BTreeMap::new(),
            active_context: None,
        })
    }

    pub fn create_context(&mut self) -> Result<BrowsingContextId, BrowserError> {
        let context_id = match self
            .handle
            .dispatch(BrowserCommand::CreateBrowsingContext)?
        {
            BrowserCommandResult::BrowsingContextCreated { context_id } => context_id,
            result => return Err(unexpected_result("create context", result)),
        };
        self.sync_state(context_id)?;
        if self.active_context.is_none() {
            self.active_context = Some(context_id);
        }
        self.drain_events(None)?;
        Ok(context_id)
    }

    pub fn load_profile_session(&mut self) -> Result<ProfileSessionState, BrowserError> {
        match self.handle.dispatch(BrowserCommand::LoadProfileSession)? {
            BrowserCommandResult::ProfileSession(session) => Ok(session),
            result => Err(unexpected_result("load profile session", result)),
        }
    }

    pub fn save_profile_session(
        &mut self,
        session: ProfileSessionState,
    ) -> Result<(), BrowserError> {
        match self
            .handle
            .dispatch(BrowserCommand::SaveProfileSession { session })?
        {
            BrowserCommandResult::Accepted => Ok(()),
            result => Err(unexpected_result("save profile session", result)),
        }
    }

    pub fn clear_profile_data(
        &mut self,
        selection: ProfileDataSelection,
    ) -> Result<(), BrowserError> {
        match self
            .handle
            .dispatch(BrowserCommand::ClearProfileData { selection })?
        {
            BrowserCommandResult::Accepted => Ok(()),
            result => Err(unexpected_result("clear profile data", result)),
        }
    }

    pub fn close_context(&mut self, context_id: BrowsingContextId) -> Result<(), BrowserError> {
        match self
            .handle
            .dispatch(BrowserCommand::CloseBrowsingContext { context_id })?
        {
            BrowserCommandResult::Accepted => {}
            result => return Err(unexpected_result("close context", result)),
        }
        self.states.remove(&context_id);
        if self.active_context == Some(context_id) {
            self.active_context = self.states.keys().next().copied();
        }
        self.drain_events(None)?;
        Ok(())
    }

    pub fn activate_context(&mut self, context_id: BrowsingContextId) -> Result<(), BrowserError> {
        match self
            .handle
            .dispatch(BrowserCommand::ActivateBrowsingContext { context_id })?
        {
            BrowserCommandResult::Accepted => {}
            result => return Err(unexpected_result("activate context", result)),
        }
        self.active_context = Some(context_id);
        self.drain_events(None)?;
        Ok(())
    }

    pub fn navigate(
        &mut self,
        context_id: BrowsingContextId,
        url: impl Into<String>,
    ) -> Result<BrowsingContextState, BrowserError> {
        self.navigation_command(
            context_id,
            BrowserCommand::Navigate {
                context_id,
                url: url.into(),
            },
        )
    }

    pub fn reload(
        &mut self,
        context_id: BrowsingContextId,
    ) -> Result<BrowsingContextState, BrowserError> {
        self.navigation_command(context_id, BrowserCommand::Reload { context_id })
    }

    pub fn go_back(
        &mut self,
        context_id: BrowsingContextId,
    ) -> Result<BrowsingContextState, BrowserError> {
        self.traverse_history(context_id, -1)
    }

    pub fn go_forward(
        &mut self,
        context_id: BrowsingContextId,
    ) -> Result<BrowsingContextState, BrowserError> {
        self.traverse_history(context_id, 1)
    }

    pub fn traverse_history(
        &mut self,
        context_id: BrowsingContextId,
        delta: i32,
    ) -> Result<BrowsingContextState, BrowserError> {
        self.navigation_command(
            context_id,
            BrowserCommand::TraverseHistory { context_id, delta },
        )
    }

    pub fn stop(
        &mut self,
        context_id: BrowsingContextId,
    ) -> Result<BrowsingContextState, BrowserError> {
        match self.handle.dispatch(BrowserCommand::Stop { context_id })? {
            BrowserCommandResult::Accepted => {}
            result => return Err(unexpected_result("stop navigation", result)),
        }
        self.drain_events(None)?;
        self.sync_state(context_id)
    }

    pub fn paint_snapshot(
        &mut self,
        context_id: BrowsingContextId,
        viewport: (u32, u32),
    ) -> Result<PaintSnapshot, BrowserError> {
        let state = self.sync_state(context_id)?;
        self.paint_snapshot_for_document(context_id, state.document_id, viewport)
    }

    pub fn paint_snapshot_for_document(
        &mut self,
        context_id: BrowsingContextId,
        document_id: vixen_api::DocumentId,
        viewport: (u32, u32),
    ) -> Result<PaintSnapshot, BrowserError> {
        self.handle
            .capture_paint_snapshot(context_id, document_id, viewport)
    }

    pub fn state(&self, context_id: BrowsingContextId) -> Option<&BrowsingContextState> {
        self.states.get(&context_id)
    }

    pub fn active_context(&self) -> Option<BrowsingContextId> {
        self.active_context
    }

    pub fn context_ids(&self) -> impl Iterator<Item = BrowsingContextId> + '_ {
        self.states.keys().copied()
    }

    fn navigation_command(
        &mut self,
        context_id: BrowsingContextId,
        command: BrowserCommand,
    ) -> Result<BrowsingContextState, BrowserError> {
        let terminal_navigation = match self.handle.dispatch(command)? {
            BrowserCommandResult::NavigationAccepted { navigation_id } => Some(navigation_id),
            BrowserCommandResult::Accepted => None,
            result => return Err(unexpected_result("navigation", result)),
        };
        self.drain_events(terminal_navigation)?;
        self.sync_state(context_id)
    }

    fn drain_events(
        &mut self,
        terminal_navigation: Option<NavigationId>,
    ) -> Result<(), BrowserError> {
        let mut terminal_result = terminal_navigation.map(|_| None);
        let deadline = terminal_navigation.map(|_| Instant::now() + NAVIGATION_WAIT_TIMEOUT);
        loop {
            let next = if let Some(deadline) = deadline {
                self.handle
                    .wait_next_event(deadline.saturating_duration_since(Instant::now()))
            } else {
                self.handle.try_next_event()
            };
            let event = match next {
                Ok(Some(event)) => event,
                Ok(None) if terminal_navigation.is_some() => {
                    return Err(BrowserError::new(
                        browser_error_codes::CLOSED,
                        format!(
                            "timed out waiting for navigation {}",
                            terminal_navigation.expect("terminal navigation exists")
                        ),
                    ));
                }
                Ok(None) => return Ok(()),
                Err(error) if error.code == browser_error_codes::EVENT_LAGGED => {
                    self.discard_queued_events()?;
                    self.resync_all()?;
                    return Err(error);
                }
                Err(error) => return Err(error),
            };
            if let Some(navigation_id) = terminal_navigation {
                match &event {
                    BrowserEvent::NavigationPhaseChanged {
                        navigation_id: event_navigation_id,
                        phase: NavigationPhase::Settled,
                        ..
                    } if *event_navigation_id == navigation_id => {
                        terminal_result = Some(Some(Ok(())));
                    }
                    BrowserEvent::NavigationFailed {
                        navigation_id: event_navigation_id,
                        error,
                        ..
                    } if *event_navigation_id == navigation_id => {
                        terminal_result = Some(Some(Err(error.clone())));
                    }
                    BrowserEvent::NavigationCancelled {
                        navigation_id: event_navigation_id,
                        reason,
                        ..
                    } if *event_navigation_id == navigation_id => {
                        terminal_result = Some(Some(Err(BrowserError::new(
                            browser_error_codes::NAVIGATION_LOAD,
                            format!("navigation {navigation_id} was cancelled: {reason:?}"),
                        ))));
                    }
                    _ => {}
                }
            }
            self.apply_event(event);
            if let Some(Some(result)) = &terminal_result {
                return result.clone();
            }
        }
    }

    fn apply_event(&mut self, event: BrowserEvent) {
        match event {
            BrowserEvent::BrowsingContextCreated { state }
            | BrowserEvent::BrowsingContextStateChanged { state } => {
                self.states.insert(state.context_id, state);
            }
            BrowserEvent::BrowsingContextClosed { context_id } => {
                self.states.remove(&context_id);
            }
            BrowserEvent::ActiveBrowsingContextChanged { context_id } => {
                self.active_context = context_id;
            }
            _ => {}
        }
    }

    fn sync_state(
        &mut self,
        context_id: BrowsingContextId,
    ) -> Result<BrowsingContextState, BrowserError> {
        let state = match self
            .handle
            .dispatch(BrowserCommand::GetBrowsingContextState { context_id })?
        {
            BrowserCommandResult::BrowsingContextState(state) => state,
            result => return Err(unexpected_result("context state", result)),
        };
        self.states.insert(context_id, state.clone());
        Ok(state)
    }

    fn resync_all(&mut self) -> Result<(), BrowserError> {
        let context_ids: Vec<_> = self.states.keys().copied().collect();
        for context_id in context_ids {
            self.sync_state(context_id)?;
        }
        Ok(())
    }

    fn discard_queued_events(&mut self) -> Result<(), BrowserError> {
        loop {
            match self.handle.try_next_event() {
                Ok(Some(_)) => {}
                Ok(None) => return Ok(()),
                Err(error) if error.code == browser_error_codes::EVENT_LAGGED => {}
                Err(error) => return Err(error),
            }
        }
    }
}

fn unexpected_result(action: &str, result: BrowserCommandResult) -> BrowserError {
    BrowserError::new(
        browser_error_codes::CLOSED,
        format!("unexpected {action} result: {result:?}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contexts_route_navigation_history_activation_and_paint() {
        let profile = tempfile::tempdir().unwrap();
        let profile_path = profile.path().join("profile.redb");
        let mut config = BrowserConfig::new(&profile_path);
        for (url, title) in [
            ("https://shell.test/a", "A"),
            ("https://shell.test/b", "B"),
            ("https://shell.test/next", "Next"),
        ] {
            config.document_overrides.insert(
                url.to_owned(),
                format!("<!doctype html><title>{title}</title><main>{title}</main>"),
            );
        }
        let mut browser = ShellBrowser::from_config(config).unwrap();
        let context_a = browser.create_context().unwrap();
        let context_b = browser.create_context().unwrap();

        let state_a = browser.navigate(context_a, "https://shell.test/a").unwrap();
        let state_b = browser.navigate(context_b, "https://shell.test/b").unwrap();
        assert_eq!(state_a.title.as_deref(), Some("A"));
        assert_eq!(state_b.title.as_deref(), Some("B"));
        assert_ne!(state_a.document_id, state_b.document_id);
        assert_eq!(browser.context_ids().count(), 2);

        let next_a = browser
            .navigate(context_a, "https://shell.test/next")
            .unwrap();
        assert!(next_a.can_go_back);
        assert_eq!(next_a.history_length, 2);
        let back_a = browser.go_back(context_a).unwrap();
        assert_eq!(back_a.url, "https://shell.test/a");
        assert_eq!(browser.state(context_b).unwrap().url, state_b.url);

        browser.activate_context(context_b).unwrap();
        assert_eq!(browser.active_context(), Some(context_b));
        let paint = browser.paint_snapshot(context_b, (320, 200)).unwrap();
        assert_eq!(paint.context_id, context_b);
        assert_eq!(paint.document_id, state_b.document_id);
        assert!(!paint.commands.is_empty());

        browser.close_context(context_a).unwrap();
        assert!(browser.state(context_a).is_none());
        assert_eq!(browser.context_ids().collect::<Vec<_>>(), vec![context_b]);
        drop(browser);
        assert!(profile_path.exists());
    }

    #[test]
    fn profile_session_and_clear_data_use_the_open_browser_store() {
        let profile = tempfile::tempdir().unwrap();
        let profile_path = profile.path().join("profile.redb");
        let mut browser = ShellBrowser::open(&profile_path).unwrap();
        let session = ProfileSessionState {
            tabs: vec!["about:vixen".to_owned(), "https://example.test/".to_owned()],
            active_index: 1,
        };

        browser.save_profile_session(session.clone()).unwrap();
        assert_eq!(browser.load_profile_session().unwrap(), session);

        browser
            .clear_profile_data(ProfileDataSelection::browsing_data())
            .unwrap();
        assert_eq!(browser.load_profile_session().unwrap(), session);

        browser
            .clear_profile_data(ProfileDataSelection {
                session: true,
                ..ProfileDataSelection::default()
            })
            .unwrap();
        assert_eq!(
            browser.load_profile_session().unwrap(),
            ProfileSessionState::default()
        );
    }
}
