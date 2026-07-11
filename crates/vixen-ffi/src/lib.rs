//! GUI-neutral controller core for foreign-language Vixen frontends.

#![deny(unsafe_code)]

pub mod c_abi;

use std::path::PathBuf;
use std::time::Duration;

pub use vixen_api::{
    BrowserError, BrowserEvent, BrowserSnapshot, BrowsingContextId, BrowsingContextState,
    NavigationId, ProfileSessionState,
};
pub use vixen_engine::browser::BrowserConfig;

use vixen_api::{BrowserCommand, BrowserCommandResult, browser_error_codes};
use vixen_engine::browser::{EngineBrowserHandle, spawn_browser};

/// Version of the exported C ABI and its JSON wire projections.
pub const ABI_VERSION: u32 = 1;

/// Return the C ABI and JSON wire version from safe Rust.
pub const fn vixen_abi_version() -> u32 {
    ABI_VERSION
}

/// The frontend operations intentionally supported by this migration seam.
#[derive(Debug, Clone, PartialEq)]
pub enum ControllerCommand {
    LoadProfileSession,
    SaveCurrentProfileSession,
    BrowserSnapshot,
    CreateContext,
    CloseContext(BrowsingContextId),
    ActivateContext(BrowsingContextId),
    Navigate {
        context_id: BrowsingContextId,
        url: String,
    },
    Reload(BrowsingContextId),
    Stop(BrowsingContextId),
    TraverseHistory {
        context_id: BrowsingContextId,
        delta: i32,
    },
    ContextState(BrowsingContextId),
}

/// Immediate, typed acknowledgement of a [`ControllerCommand`].
#[derive(Debug, Clone, PartialEq)]
pub enum ControllerResponse {
    Accepted,
    ProfileSession(ProfileSessionState),
    BrowserSnapshot(BrowserSnapshot),
    ContextCreated(BrowsingContextId),
    NavigationAccepted(NavigationId),
    ContextState(BrowsingContextState),
}

/// One browser/profile owner and the sole consumer of its ordered event queue.
///
/// The controller is intentionally not `Clone` and does not maintain a second
/// background model. Frontends drive it from their chosen execution context.
pub struct FlutterBrowserController {
    handle: EngineBrowserHandle,
}

impl FlutterBrowserController {
    /// Open one browser profile and start its engine owner thread.
    pub fn open(profile_path: impl Into<PathBuf>) -> Result<Self, BrowserError> {
        Self::from_config(BrowserConfig::new(profile_path))
    }

    /// Open a browser with explicit engine configuration.
    ///
    /// This is useful to embedded hosts and deterministic tests without adding
    /// frontend-specific configuration or test hooks to this crate.
    pub fn from_config(config: BrowserConfig) -> Result<Self, BrowserError> {
        Ok(Self {
            handle: spawn_browser(config)?,
        })
    }

    /// Validate and enqueue a high-level command, returning only its immediate
    /// acknowledgement. Navigation settlement is delivered through events.
    pub fn dispatch(
        &mut self,
        command: ControllerCommand,
    ) -> Result<ControllerResponse, BrowserError> {
        let command = match command {
            ControllerCommand::LoadProfileSession => BrowserCommand::LoadProfileSession,
            ControllerCommand::SaveCurrentProfileSession => {
                BrowserCommand::SaveCurrentProfileSession
            }
            ControllerCommand::BrowserSnapshot => BrowserCommand::GetBrowserSnapshot,
            ControllerCommand::CreateContext => BrowserCommand::CreateBrowsingContext,
            ControllerCommand::CloseContext(context_id) => {
                BrowserCommand::CloseBrowsingContext { context_id }
            }
            ControllerCommand::ActivateContext(context_id) => {
                BrowserCommand::ActivateBrowsingContext { context_id }
            }
            ControllerCommand::Navigate { context_id, url } => {
                BrowserCommand::Navigate { context_id, url }
            }
            ControllerCommand::Reload(context_id) => BrowserCommand::Reload { context_id },
            ControllerCommand::Stop(context_id) => BrowserCommand::Stop { context_id },
            ControllerCommand::TraverseHistory { context_id, delta } => {
                BrowserCommand::TraverseHistory { context_id, delta }
            }
            ControllerCommand::ContextState(context_id) => {
                BrowserCommand::GetBrowsingContextState { context_id }
            }
        };

        match self.handle.dispatch(command)? {
            BrowserCommandResult::Accepted => Ok(ControllerResponse::Accepted),
            BrowserCommandResult::ProfileSession(session) => {
                Ok(ControllerResponse::ProfileSession(session))
            }
            BrowserCommandResult::BrowserSnapshot(snapshot) => {
                Ok(ControllerResponse::BrowserSnapshot(snapshot))
            }
            BrowserCommandResult::BrowsingContextCreated { context_id } => {
                Ok(ControllerResponse::ContextCreated(context_id))
            }
            BrowserCommandResult::NavigationAccepted { navigation_id } => {
                Ok(ControllerResponse::NavigationAccepted(navigation_id))
            }
            BrowserCommandResult::BrowsingContextState(state) => {
                Ok(ControllerResponse::ContextState(state))
            }
            result => Err(unexpected_result(result)),
        }
    }

    pub fn load_profile_session(&mut self) -> Result<ProfileSessionState, BrowserError> {
        match self.dispatch(ControllerCommand::LoadProfileSession)? {
            ControllerResponse::ProfileSession(session) => Ok(session),
            response => Err(unexpected_response("load profile session", response)),
        }
    }

    /// Persist BrowserCore's authoritative ordered context registry and active
    /// context; the frontend does not supply a parallel tab snapshot.
    pub fn save_current_profile_session(&mut self) -> Result<(), BrowserError> {
        self.expect_accepted(
            "save current profile session",
            ControllerCommand::SaveCurrentProfileSession,
        )
    }

    /// Capture BrowserCore's active context and ordered context states atomically.
    ///
    /// If event consumption reports `browser.event-lagged`, pending frontend
    /// operations are indeterminate and must be reconciled from this snapshot,
    /// not assumed to have succeeded.
    pub fn browser_snapshot(&mut self) -> Result<BrowserSnapshot, BrowserError> {
        match self.dispatch(ControllerCommand::BrowserSnapshot)? {
            ControllerResponse::BrowserSnapshot(snapshot) => Ok(snapshot),
            response => Err(unexpected_response("get browser snapshot", response)),
        }
    }

    pub fn create_context(&mut self) -> Result<BrowsingContextId, BrowserError> {
        match self.dispatch(ControllerCommand::CreateContext)? {
            ControllerResponse::ContextCreated(context_id) => Ok(context_id),
            response => Err(unexpected_response("create context", response)),
        }
    }

    pub fn close_context(&mut self, context_id: BrowsingContextId) -> Result<(), BrowserError> {
        self.expect_accepted("close context", ControllerCommand::CloseContext(context_id))
    }

    pub fn activate_context(&mut self, context_id: BrowsingContextId) -> Result<(), BrowserError> {
        self.expect_accepted(
            "activate context",
            ControllerCommand::ActivateContext(context_id),
        )
    }

    /// Accept a navigation without waiting for load, script, or render work.
    pub fn navigate(
        &mut self,
        context_id: BrowsingContextId,
        url: impl Into<String>,
    ) -> Result<NavigationId, BrowserError> {
        self.expect_navigation(
            "navigate",
            ControllerCommand::Navigate {
                context_id,
                url: url.into(),
            },
        )
    }

    /// Accept a reload without waiting for it to settle.
    pub fn reload(&mut self, context_id: BrowsingContextId) -> Result<NavigationId, BrowserError> {
        self.expect_navigation("reload", ControllerCommand::Reload(context_id))
    }

    pub fn stop(&mut self, context_id: BrowsingContextId) -> Result<(), BrowserError> {
        self.expect_accepted("stop", ControllerCommand::Stop(context_id))
    }

    /// Traverse history, returning `None` when the requested entry does not
    /// exist and no navigation was started.
    pub fn traverse_history(
        &mut self,
        context_id: BrowsingContextId,
        delta: i32,
    ) -> Result<Option<NavigationId>, BrowserError> {
        match self.dispatch(ControllerCommand::TraverseHistory { context_id, delta })? {
            ControllerResponse::NavigationAccepted(navigation_id) => Ok(Some(navigation_id)),
            ControllerResponse::Accepted => Ok(None),
            response => Err(unexpected_response("traverse history", response)),
        }
    }

    pub fn context_state(
        &mut self,
        context_id: BrowsingContextId,
    ) -> Result<BrowsingContextState, BrowserError> {
        match self.dispatch(ControllerCommand::ContextState(context_id))? {
            ControllerResponse::ContextState(state) => Ok(state),
            response => Err(unexpected_response("get context state", response)),
        }
    }

    /// Return the next ordered browser event without blocking.
    ///
    /// On `browser.event-lagged`, pending frontend operations are indeterminate;
    /// call [`Self::browser_snapshot`] and reconcile instead of assuming success.
    pub fn try_next_event(&mut self) -> Result<Option<BrowserEvent>, BrowserError> {
        self.handle.try_next_event()
    }

    /// Wait at most `timeout` for the next ordered browser event.
    ///
    /// On `browser.event-lagged`, pending frontend operations are indeterminate;
    /// call [`Self::browser_snapshot`] and reconcile instead of assuming success.
    pub fn wait_next_event(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<BrowserEvent>, BrowserError> {
        self.handle.wait_next_event(timeout)
    }

    fn expect_accepted(
        &mut self,
        action: &'static str,
        command: ControllerCommand,
    ) -> Result<(), BrowserError> {
        match self.dispatch(command)? {
            ControllerResponse::Accepted => Ok(()),
            response => Err(unexpected_response(action, response)),
        }
    }

    fn expect_navigation(
        &mut self,
        action: &'static str,
        command: ControllerCommand,
    ) -> Result<NavigationId, BrowserError> {
        match self.dispatch(command)? {
            ControllerResponse::NavigationAccepted(navigation_id) => Ok(navigation_id),
            response => Err(unexpected_response(action, response)),
        }
    }
}

fn unexpected_result(result: BrowserCommandResult) -> BrowserError {
    BrowserError::new(
        browser_error_codes::CLOSED,
        format!("unsupported browser result at controller boundary: {result:?}"),
    )
}

fn unexpected_response(action: &str, response: ControllerResponse) -> BrowserError {
    BrowserError::new(
        browser_error_codes::CLOSED,
        format!("unexpected response for {action}: {response:?}"),
    )
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use vixen_api::{BrowserEvent, NavigationCancellationReason, NavigationPhase};
    use vixen_engine::browser::BrowserConfig;

    use super::*;

    static NEXT_PROFILE: AtomicU64 = AtomicU64::new(1);
    const EVENT_TIMEOUT: Duration = Duration::from_secs(10);

    struct TestProfile(PathBuf);

    impl TestProfile {
        fn new() -> Self {
            let serial = NEXT_PROFILE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("vixen-ffi-{}-{serial}.redb", std::process::id()));
            let _ = std::fs::remove_file(&path);
            Self(path)
        }
    }

    impl Drop for TestProfile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn abi_version_stays_at_one() {
        assert_eq!(ABI_VERSION, 1);
        assert_eq!(vixen_abi_version(), 1);
    }

    #[test]
    fn core_owned_session_save_restores_only_current_contexts_and_active_tab() {
        let profile = TestProfile::new();
        let mut config = BrowserConfig::new(&profile.0);
        config.document_overrides.insert(
            "https://ffi.test/a".to_owned(),
            "<!doctype html><title>A</title>".to_owned(),
        );
        config.document_overrides.insert(
            "https://ffi.test/b".to_owned(),
            "<!doctype html><title>B</title>".to_owned(),
        );
        config.document_overrides.insert(
            "https://ffi.test/c".to_owned(),
            "<!doctype html><title>C</title>".to_owned(),
        );
        let mut controller = FlutterBrowserController::from_config(config).unwrap();
        let context_a = controller.create_context().unwrap();
        let context_b = controller.create_context().unwrap();
        let context_c = controller.create_context().unwrap();

        let navigation_a = controller
            .navigate(context_a, "https://ffi.test/a")
            .unwrap();
        wait_for_settled(&mut controller, navigation_a);
        let navigation_b = controller
            .navigate(context_b, "https://ffi.test/b")
            .unwrap();
        wait_for_settled(&mut controller, navigation_b);
        let navigation_c = controller
            .navigate(context_c, "https://ffi.test/c")
            .unwrap();
        wait_for_settled(&mut controller, navigation_c);

        controller.close_context(context_b).unwrap();
        controller.activate_context(context_c).unwrap();
        controller.save_current_profile_session().unwrap();
        let expected = ProfileSessionState {
            tabs: vec![
                "https://ffi.test/a".to_owned(),
                "https://ffi.test/c".to_owned(),
            ],
            active_index: 1,
        };
        assert_eq!(controller.load_profile_session().unwrap(), expected);
        let snapshot = controller.browser_snapshot().unwrap();
        assert_eq!(snapshot.active_context_id, Some(context_c));
        assert_eq!(
            snapshot
                .contexts
                .iter()
                .map(|state| (state.context_id, state.url.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (context_a, "https://ffi.test/a"),
                (context_c, "https://ffi.test/c"),
            ]
        );

        drop(controller);
        let mut reopened = FlutterBrowserController::open(&profile.0).unwrap();
        assert_eq!(reopened.load_profile_session().unwrap(), expected);
    }

    #[test]
    fn lagged_events_are_recovered_from_the_authoritative_browser_snapshot() {
        let profile = TestProfile::new();
        let server = GatedServer::start();
        let mut config = BrowserConfig::new(&profile.0);
        config.event_capacity = 1;
        server.configure(&mut config);
        let mut controller = FlutterBrowserController::from_config(config).unwrap();
        let removed_context = controller.create_context().unwrap();
        let current_context = controller.create_context().unwrap();
        controller.close_context(removed_context).unwrap();
        controller.activate_context(current_context).unwrap();

        let navigation_id = controller.navigate(current_context, &server.url).unwrap();
        server.wait_for_request();
        let lag = controller.try_next_event().unwrap_err();
        assert_eq!(lag.code, browser_error_codes::EVENT_LAGGED);

        let loading = controller.browser_snapshot().unwrap();
        assert_eq!(loading.active_context_id, Some(current_context));
        assert_eq!(loading.contexts.len(), 1);
        assert_eq!(loading.contexts[0].context_id, current_context);
        assert_eq!(
            loading.contexts[0].active_navigation_id,
            Some(navigation_id)
        );
        assert!(loading.contexts[0].is_loading);

        let expected_url = server.url.clone();
        server.finish();
        let deadline = std::time::Instant::now() + EVENT_TIMEOUT;
        let settled = loop {
            let snapshot = controller.browser_snapshot().unwrap();
            if snapshot.contexts[0].active_navigation_id.is_none() {
                break snapshot;
            }
            assert!(std::time::Instant::now() < deadline, "snapshot watchdog");
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(settled.active_context_id, Some(current_context));
        assert_eq!(settled.contexts.len(), 1);
        assert_eq!(settled.contexts[0].context_id, current_context);
        assert_eq!(settled.contexts[0].url, expected_url);
        assert!(!settled.contexts[0].is_loading);
    }

    #[test]
    fn navigation_acceptance_is_followed_by_one_exact_terminal_outcome() {
        let profile = TestProfile::new();
        let url = "https://ffi.test/settled";
        let mut config = BrowserConfig::new(&profile.0);
        config.document_overrides.insert(
            url.to_owned(),
            "<!doctype html><title>Settled</title>".to_owned(),
        );
        let mut controller = FlutterBrowserController::from_config(config).unwrap();
        let context_id = controller.create_context().unwrap();
        drain_events(&mut controller);

        let navigation_id = controller.navigate(context_id, url).unwrap();
        let events = events_through_terminal_phase(&mut controller, navigation_id);

        assert_eq!(terminal_phase_count(&events, navigation_id), 1);
        assert!(events.iter().any(|event| matches!(
            event,
            BrowserEvent::NavigationPhaseChanged {
                navigation_id: event_id,
                phase: NavigationPhase::Settled,
                ..
            } if *event_id == navigation_id
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            BrowserEvent::NavigationFailed { navigation_id: event_id, .. }
                | BrowserEvent::NavigationCancelled { navigation_id: event_id, .. }
                if *event_id == navigation_id
        )));
    }

    #[test]
    fn stop_is_dispatched_while_a_gated_navigation_is_active() {
        let profile = TestProfile::new();
        let server = GatedServer::start();
        let mut config = BrowserConfig::new(&profile.0);
        server.configure(&mut config);
        let mut controller = FlutterBrowserController::from_config(config).unwrap();
        let context_id = controller.create_context().unwrap();
        drain_events(&mut controller);

        let navigation_id = controller.navigate(context_id, &server.url).unwrap();
        server.wait_for_request();
        assert_eq!(
            controller
                .context_state(context_id)
                .unwrap()
                .active_navigation_id,
            Some(navigation_id)
        );

        controller.stop(context_id).unwrap();
        let mut events = events_through_cancellation(&mut controller, navigation_id);
        events.extend(drain_events(&mut controller));
        server.finish();

        assert_eq!(terminal_phase_count(&events, navigation_id), 1);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    BrowserEvent::NavigationCancelled {
                        navigation_id: event_id,
                        reason: NavigationCancellationReason::Stopped,
                        ..
                    } if *event_id == navigation_id
                ))
                .count(),
            1
        );
        assert_eq!(
            controller
                .context_state(context_id)
                .unwrap()
                .active_navigation_id,
            None
        );
    }

    fn wait_for_settled(controller: &mut FlutterBrowserController, navigation_id: NavigationId) {
        let events = events_through_terminal_phase(controller, navigation_id);
        assert!(events.iter().any(|event| matches!(
            event,
            BrowserEvent::NavigationPhaseChanged {
                navigation_id: event_id,
                phase: NavigationPhase::Settled,
                ..
            } if *event_id == navigation_id
        )));
    }

    fn events_through_terminal_phase(
        controller: &mut FlutterBrowserController,
        navigation_id: NavigationId,
    ) -> Vec<BrowserEvent> {
        let mut events = Vec::new();
        loop {
            let event = controller
                .wait_next_event(EVENT_TIMEOUT)
                .unwrap()
                .expect("navigation terminal event watchdog");
            let terminal = matches!(
                &event,
                BrowserEvent::NavigationPhaseChanged {
                    navigation_id: event_id,
                    phase,
                    ..
                } if *event_id == navigation_id && phase.is_terminal()
            );
            events.push(event);
            if terminal {
                events.extend(drain_events(controller));
                return events;
            }
        }
    }

    fn events_through_cancellation(
        controller: &mut FlutterBrowserController,
        navigation_id: NavigationId,
    ) -> Vec<BrowserEvent> {
        let mut events = Vec::new();
        loop {
            let event = controller
                .wait_next_event(EVENT_TIMEOUT)
                .unwrap()
                .expect("navigation cancellation watchdog");
            let cancelled = matches!(
                &event,
                BrowserEvent::NavigationCancelled {
                    navigation_id: event_id,
                    ..
                } if *event_id == navigation_id
            );
            events.push(event);
            if cancelled {
                return events;
            }
        }
    }

    fn terminal_phase_count(events: &[BrowserEvent], navigation_id: NavigationId) -> usize {
        events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    BrowserEvent::NavigationPhaseChanged {
                        navigation_id: event_id,
                        phase,
                        ..
                    } if *event_id == navigation_id && phase.is_terminal()
                )
            })
            .count()
    }

    fn drain_events(controller: &mut FlutterBrowserController) -> Vec<BrowserEvent> {
        let mut events = Vec::new();
        while let Some(event) = controller.try_next_event().unwrap() {
            events.push(event);
        }
        events
    }

    struct GatedServer {
        address: std::net::SocketAddr,
        url: String,
        request_received: mpsc::Receiver<()>,
        release_response: mpsc::SyncSender<()>,
        join: thread::JoinHandle<()>,
    }

    impl GatedServer {
        fn start() -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let address = listener.local_addr().unwrap();
            let (request_tx, request_received) = mpsc::sync_channel(1);
            let (release_response, release_rx) = mpsc::sync_channel(1);
            let join = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request).unwrap();
                request_tx.send(()).unwrap();
                release_rx
                    .recv_timeout(EVENT_TIMEOUT)
                    .expect("gated response watchdog");
                let body = "<!doctype html><title>Too late</title>";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            });
            Self {
                address,
                url: format!("http://vixen-ffi-browser.com:{}/slow", address.port()),
                request_received,
                release_response,
                join,
            }
        }

        fn configure(&self, config: &mut BrowserConfig) {
            config
                .network
                .dns_overrides
                .push(("vixen-ffi-browser.com".to_owned(), vec![self.address]));
        }

        fn wait_for_request(&self) {
            self.request_received
                .recv_timeout(EVENT_TIMEOUT)
                .expect("gated request watchdog");
        }

        fn finish(self) {
            self.release_response.send(()).unwrap();
            self.join.join().unwrap();
        }
    }
}
