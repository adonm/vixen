//! GUI-neutral controller core for foreign-language Vixen frontends.

#![deny(unsafe_code)]

pub mod c_abi;
pub mod c_renderer;
mod cdp_host;
mod render_wire;
mod renderer_broker;
mod sync_renderer;

pub use renderer_broker::{
    RENDER_BROKER_MAX_UPDATE_SOURCE_BYTES, RenderBroker, RenderBrokerError, RenderBrokerMessage,
};

use std::path::PathBuf;
use std::time::Duration;

pub use vixen_api::{
    AccessibilityAction, AccessibilityNode, AccessibilityRect, AccessibilitySnapshot, BrowserError,
    BrowserEvent, BrowserSnapshot, BrowsingContextId, BrowsingContextState, DocumentId,
    FindTextResult, HostLifecycle, HostViewState, InputDispatchResult, KeyEventData,
    MouseEventData, NavigationId, ProfileSessionState, RuntimeContextId, TextInputState,
};
pub use vixen_engine::browser::BrowserConfig;

use vixen_api::{
    BrowserCommand, BrowserCommandResult, FullRenderSnapshot, RenderCommit, RenderHitTestQuery,
    RenderInputTarget, browser_error_codes,
};
use vixen_engine::browser::{EngineBrowserClient, EngineBrowserHandle, spawn_browser};

/// Version of the exported C ABI and its JSON wire projections.
pub const ABI_VERSION: u32 = 1;
pub(crate) const ACCESSIBILITY_ABI_MAX_NODES: usize = 192;

/// Return the C ABI and JSON wire version from safe Rust.
pub const fn vixen_abi_version() -> u32 {
    ABI_VERSION
}

#[derive(Debug, Clone, PartialEq)]
pub struct MouseEventDispatch {
    pub context_id: BrowsingContextId,
    pub document_id: DocumentId,
    pub runtime_context_id: RuntimeContextId,
    pub viewport: (u32, u32),
    pub event_type: String,
    pub event: MouseEventData,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RendererMouseEventDispatch {
    pub mouse: MouseEventDispatch,
    pub query: RenderHitTestQuery,
    pub target: Option<RenderInputTarget>,
}

/// The frontend operations intentionally supported by this migration seam.
#[derive(Debug, Clone, PartialEq)]
pub enum ControllerCommand {
    StartCdp {
        port: u16,
    },
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
    UpdateHostViewState {
        context_id: BrowsingContextId,
        state: HostViewState,
    },
    SetPageZoom {
        context_id: BrowsingContextId,
        zoom: f64,
    },
    AccessibilitySnapshot {
        context_id: BrowsingContextId,
        document_id: DocumentId,
        viewport: (u32, u32),
    },
    PublishRendererSnapshot {
        context_id: BrowsingContextId,
        document_id: DocumentId,
        viewport: (u32, u32),
        viewport_generation: u64,
        page_zoom: f64,
    },
    FlushRendererSubmissions,
    DispatchAccessibilityAction {
        context_id: BrowsingContextId,
        document_id: DocumentId,
        runtime_context_id: RuntimeContextId,
        viewport: (u32, u32),
        source_generation: u64,
        generation: u64,
        node_id: usize,
        action: AccessibilityAction,
    },
    DispatchRendererMouseEvent(Box<RendererMouseEventDispatch>),
    DispatchKeyEvent {
        context_id: BrowsingContextId,
        document_id: DocumentId,
        runtime_context_id: RuntimeContextId,
        viewport: (u32, u32),
        event_type: String,
        event: KeyEventData,
    },
    DispatchTextInput {
        context_id: BrowsingContextId,
        document_id: DocumentId,
        runtime_context_id: RuntimeContextId,
        viewport: (u32, u32),
        state: TextInputState,
    },
    FindText {
        context_id: BrowsingContextId,
        document_id: DocumentId,
        query: String,
        case_sensitive: bool,
        forward: bool,
    },
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
    AccessibilitySnapshot(AccessibilitySnapshot),
    InputDispatched(InputDispatchResult),
    FindText(FindTextResult),
    RendererUpdate(FullRenderSnapshot),
}

/// One browser/profile owner and the sole consumer of its ordered event queue.
///
/// The controller is intentionally not `Clone` and does not maintain a second
/// background model. Frontends drive it from their chosen execution context.
pub struct FlutterBrowserController {
    handle: EngineBrowserHandle,
    primary_mouse_press: Option<PrimaryMousePress>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PrimaryMousePress {
    context_id: BrowsingContextId,
    document_id: DocumentId,
    runtime_context_id: RuntimeContextId,
    node_id: usize,
}

struct AccessibilityActionDispatch {
    context_id: BrowsingContextId,
    document_id: DocumentId,
    runtime_context_id: RuntimeContextId,
    viewport: (u32, u32),
    source_generation: u64,
    generation: u64,
    node_id: usize,
    action: AccessibilityAction,
}

impl FlutterBrowserController {
    pub(crate) fn apply_renderer_commit(
        &mut self,
        commit: RenderCommit,
    ) -> Result<(), BrowserError> {
        match self
            .handle
            .dispatch(BrowserCommand::ApplyRendererCommit { commit })?
        {
            BrowserCommandResult::Accepted => Ok(()),
            result => Err(unexpected_result(result)),
        }
    }

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
            primary_mouse_press: None,
        })
    }

    pub(crate) fn subscribe_browser(&self) -> EngineBrowserClient {
        self.handle.subscribe()
    }

    /// Validate and enqueue a high-level command, returning only its immediate
    /// acknowledgement. Navigation settlement is delivered through events.
    pub fn dispatch(
        &mut self,
        command: ControllerCommand,
    ) -> Result<ControllerResponse, BrowserError> {
        let command = match command {
            ControllerCommand::StartCdp { .. } => {
                return Err(BrowserError::new(
                    browser_error_codes::INVALID_ARGUMENT,
                    "CDP startup is owned by the embedding boundary",
                ));
            }
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
            ControllerCommand::UpdateHostViewState { context_id, state } => {
                self.primary_mouse_press = None;
                BrowserCommand::UpdateHostViewState { context_id, state }
            }
            ControllerCommand::SetPageZoom { context_id, zoom } => {
                self.primary_mouse_press = None;
                BrowserCommand::SetPageZoom { context_id, zoom }
            }
            ControllerCommand::AccessibilitySnapshot {
                context_id,
                document_id,
                viewport,
            } => BrowserCommand::AccessibilitySnapshot {
                context_id,
                document_id,
                viewport,
            },
            ControllerCommand::PublishRendererSnapshot {
                context_id,
                document_id,
                viewport,
                viewport_generation,
                page_zoom,
            } => {
                return self.renderer_snapshot(
                    context_id,
                    document_id,
                    viewport,
                    viewport_generation,
                    page_zoom,
                );
            }
            ControllerCommand::FlushRendererSubmissions => {
                return Ok(ControllerResponse::Accepted);
            }
            ControllerCommand::DispatchAccessibilityAction {
                context_id,
                document_id,
                runtime_context_id,
                viewport,
                source_generation,
                generation,
                node_id,
                action,
            } => {
                return self.dispatch_accessibility_action(AccessibilityActionDispatch {
                    context_id,
                    document_id,
                    runtime_context_id,
                    viewport,
                    source_generation,
                    generation,
                    node_id,
                    action,
                });
            }
            ControllerCommand::DispatchRendererMouseEvent(_) => {
                return Err(BrowserError::new(
                    browser_error_codes::INVALID_ARGUMENT,
                    "renderer mouse input requires commit validation at the C boundary",
                ));
            }
            ControllerCommand::DispatchKeyEvent {
                context_id,
                document_id,
                runtime_context_id,
                viewport: _,
                event_type,
                event,
            } => BrowserCommand::DispatchKeyEvent {
                context_id,
                document_id,
                runtime_context_id,
                event_type: match event_type.as_str() {
                    "keydown" => "keyDown".to_owned(),
                    "keyup" => "keyUp".to_owned(),
                    _ => event_type,
                },
                event,
            },
            ControllerCommand::DispatchTextInput {
                context_id,
                document_id,
                runtime_context_id,
                viewport: _,
                state,
            } => BrowserCommand::DispatchTextInput {
                context_id,
                document_id,
                runtime_context_id,
                state,
            },
            ControllerCommand::FindText {
                context_id,
                document_id,
                query,
                case_sensitive,
                forward,
            } => BrowserCommand::FindText {
                context_id,
                document_id,
                query,
                case_sensitive,
                forward,
            },
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
            BrowserCommandResult::AccessibilitySnapshot(snapshot) => {
                Ok(ControllerResponse::AccessibilitySnapshot(snapshot))
            }
            BrowserCommandResult::InputDispatched(result) => {
                Ok(ControllerResponse::InputDispatched(result))
            }
            BrowserCommandResult::FindText(result) => Ok(ControllerResponse::FindText(result)),
            result => Err(unexpected_result(result)),
        }
    }

    fn dispatch_mouse_event(
        &mut self,
        dispatch: MouseEventDispatch,
        renderer_target: Option<usize>,
    ) -> Result<ControllerResponse, BrowserError> {
        let MouseEventDispatch {
            context_id,
            document_id,
            runtime_context_id,
            viewport: _,
            event_type,
            event,
        } = dispatch;
        if !matches!(
            event_type.as_str(),
            "mousemove" | "mousedown" | "mouseup" | "wheel" | "cancel"
        ) {
            self.primary_mouse_press = None;
            return Err(BrowserError::new(
                browser_error_codes::INVALID_ARGUMENT,
                "unsupported mouse event type",
            ));
        }

        if event_type == "cancel" {
            if self.primary_mouse_press.is_some_and(|press| {
                press.context_id == context_id
                    && press.document_id == document_id
                    && press.runtime_context_id == runtime_context_id
            }) {
                self.primary_mouse_press = None;
            }
            return Ok(ControllerResponse::InputDispatched(empty_input_result()));
        }

        let generation_matches = self.primary_mouse_press.is_none_or(|press| {
            press.context_id == context_id
                && press.document_id == document_id
                && press.runtime_context_id == runtime_context_id
        });
        if !generation_matches || event_type == "mousedown" {
            self.primary_mouse_press = None;
        }
        let pressed = if event_type == "mouseup" {
            self.primary_mouse_press.take()
        } else {
            None
        };

        let target_node_id = match renderer_target {
            Some(node_id) => node_id,
            None if event_type == "wheel" => 0,
            None => {
                self.primary_mouse_press = None;
                return Ok(ControllerResponse::InputDispatched(empty_input_result()));
            }
        };

        let mut result = match self.dispatch_mouse_to_node(
            context_id,
            document_id,
            runtime_context_id,
            target_node_id,
            event_type.clone(),
            event.clone(),
        ) {
            Ok(result) => result,
            Err(error) => {
                self.primary_mouse_press = None;
                return Err(error);
            }
        };

        if event_type == "mousedown" && event.button == 0 {
            self.primary_mouse_press = Some(PrimaryMousePress {
                context_id,
                document_id,
                runtime_context_id,
                node_id: target_node_id,
            });
        } else if event_type == "mouseup"
            && event.button == 0
            && pressed
                == Some(PrimaryMousePress {
                    context_id,
                    document_id,
                    runtime_context_id,
                    node_id: target_node_id,
                })
        {
            let clicked = match self.dispatch_mouse_to_node(
                context_id,
                document_id,
                runtime_context_id,
                target_node_id,
                "click".to_owned(),
                event,
            ) {
                Ok(result) => result,
                Err(error) => {
                    self.primary_mouse_press = None;
                    return Err(error);
                }
            };
            result.effects.extend(clicked.effects);
            result.navigation_actions.extend(clicked.navigation_actions);
        }

        Ok(ControllerResponse::InputDispatched(result))
    }

    pub(crate) fn dispatch_renderer_mouse_event(
        &mut self,
        dispatch: MouseEventDispatch,
        target_node_id: Option<usize>,
    ) -> Result<ControllerResponse, BrowserError> {
        self.dispatch_mouse_event(dispatch, target_node_id)
    }

    fn dispatch_mouse_to_node(
        &mut self,
        context_id: BrowsingContextId,
        document_id: DocumentId,
        runtime_context_id: RuntimeContextId,
        node_id: usize,
        event_type: String,
        event: MouseEventData,
    ) -> Result<InputDispatchResult, BrowserError> {
        match self.handle.dispatch(BrowserCommand::DispatchMouseEvent {
            context_id,
            document_id,
            runtime_context_id,
            node_id,
            event_type,
            event,
        })? {
            BrowserCommandResult::InputDispatched(result) => Ok(result),
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

    pub fn set_page_zoom(
        &mut self,
        context_id: BrowsingContextId,
        zoom: f64,
    ) -> Result<BrowsingContextState, BrowserError> {
        match self.dispatch(ControllerCommand::SetPageZoom { context_id, zoom })? {
            ControllerResponse::ContextState(state) => Ok(state),
            response => Err(unexpected_response("set page zoom", response)),
        }
    }

    pub fn accessibility_snapshot(
        &mut self,
        context_id: BrowsingContextId,
        document_id: DocumentId,
        viewport: (u32, u32),
    ) -> Result<AccessibilitySnapshot, BrowserError> {
        match self.dispatch(ControllerCommand::AccessibilitySnapshot {
            context_id,
            document_id,
            viewport,
        })? {
            ControllerResponse::AccessibilitySnapshot(snapshot) => Ok(snapshot),
            response => Err(unexpected_response("get accessibility snapshot", response)),
        }
    }

    fn renderer_snapshot(
        &mut self,
        context_id: BrowsingContextId,
        document_id: DocumentId,
        viewport: (u32, u32),
        viewport_generation: u64,
        page_zoom: f64,
    ) -> Result<ControllerResponse, BrowserError> {
        if viewport_generation == 0 {
            return Err(BrowserError::new(
                browser_error_codes::INVALID_ARGUMENT,
                "renderer viewport generation must be nonzero",
            ));
        }
        if !page_zoom.is_finite() || !(0.25..=5.0).contains(&page_zoom) {
            return Err(BrowserError::new(
                browser_error_codes::INVALID_ARGUMENT,
                "renderer page zoom is outside the supported range",
            ));
        }
        match self.handle.dispatch(BrowserCommand::RenderSnapshot {
            context_id,
            document_id,
            viewport,
            viewport_generation,
            page_zoom,
        })? {
            BrowserCommandResult::RenderSnapshot(snapshot) => {
                Ok(ControllerResponse::RendererUpdate(snapshot))
            }
            result => Err(unexpected_result(result)),
        }
    }

    pub fn find_text(
        &mut self,
        context_id: BrowsingContextId,
        document_id: DocumentId,
        query: impl Into<String>,
        case_sensitive: bool,
        forward: bool,
    ) -> Result<FindTextResult, BrowserError> {
        match self.dispatch(ControllerCommand::FindText {
            context_id,
            document_id,
            query: query.into(),
            case_sensitive,
            forward,
        })? {
            ControllerResponse::FindText(result) => Ok(result),
            response => Err(unexpected_response("find text", response)),
        }
    }

    fn dispatch_accessibility_action(
        &mut self,
        request: AccessibilityActionDispatch,
    ) -> Result<ControllerResponse, BrowserError> {
        let AccessibilityActionDispatch {
            context_id,
            document_id,
            runtime_context_id,
            viewport,
            source_generation,
            generation,
            node_id,
            action,
        } = request;
        let snapshot = bounded_accessibility_snapshot(self.accessibility_snapshot(
            context_id,
            document_id,
            viewport,
        )?);
        if snapshot.source_generation != source_generation || snapshot.generation != generation {
            return Err(BrowserError::new(
                browser_error_codes::STALE_ACCESSIBILITY,
                "accessibility projection is no longer current",
            ));
        }
        let supported = snapshot.nodes.iter().any(|node| {
            node.id == node_id
                && node
                    .actions
                    .iter()
                    .any(|candidate| candidate == action.as_str())
        });
        if !supported {
            return Err(BrowserError::new(
                browser_error_codes::INVALID_ARGUMENT,
                "accessibility node does not advertise the requested action",
            ));
        }
        match self
            .handle
            .dispatch(BrowserCommand::DispatchAccessibilityAction {
                context_id,
                document_id,
                runtime_context_id,
                viewport,
                source_generation,
                node_id,
                action,
            })? {
            BrowserCommandResult::InputDispatched(result) => {
                Ok(ControllerResponse::InputDispatched(result))
            }
            result => Err(unexpected_result(result)),
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

fn empty_input_result() -> InputDispatchResult {
    InputDispatchResult {
        effects: Default::default(),
        navigation_actions: Vec::new(),
    }
}

pub(crate) fn bounded_accessibility_snapshot(
    mut snapshot: AccessibilitySnapshot,
) -> AccessibilitySnapshot {
    if snapshot.nodes.len() > ACCESSIBILITY_ABI_MAX_NODES {
        snapshot.nodes.truncate(ACCESSIBILITY_ABI_MAX_NODES);
        snapshot.truncated = true;
    }
    let retained_ids = snapshot
        .nodes
        .iter()
        .map(|node| node.id)
        .collect::<std::collections::HashSet<_>>();
    for node in &mut snapshot.nodes {
        let before = node.controls_ids.len()
            + node.described_by_ids.len()
            + node.details_ids.len()
            + node.owns_ids.len();
        node.controls_ids
            .retain(|target| retained_ids.contains(target));
        node.described_by_ids
            .retain(|target| retained_ids.contains(target));
        node.details_ids
            .retain(|target| retained_ids.contains(target));
        node.owns_ids.retain(|target| retained_ids.contains(target));
        let after = node.controls_ids.len()
            + node.described_by_ids.len()
            + node.details_ids.len()
            + node.owns_ids.len();
        snapshot.truncated |= after != before;
    }
    snapshot.refresh_generation();
    snapshot
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
    fn controller_returns_authoritative_accessibility_snapshot_and_rejects_stale_document() {
        let profile = TestProfile::new();
        let url = "https://ffi.test/accessibility";
        let mut config = BrowserConfig::new(&profile.0);
        config.document_overrides.insert(
            url.to_owned(),
            "<!doctype html><button id='go' aria-label='Continue'>Ignored</button><script>document.querySelector('#go').focus()</script>"
                .to_owned(),
        );
        let mut controller = FlutterBrowserController::from_config(config).unwrap();
        let context_id = controller.create_context().unwrap();
        let navigation_id = controller.navigate(context_id, url).unwrap();
        wait_for_settled(&mut controller, navigation_id);
        let state = controller.context_state(context_id).unwrap();

        let snapshot = controller
            .accessibility_snapshot(context_id, state.document_id, (320, 240))
            .unwrap();
        assert_eq!(snapshot.context_id, context_id);
        assert_eq!(snapshot.document_id, state.document_id);
        assert_ne!(snapshot.generation, 0);
        assert_eq!(
            controller
                .accessibility_snapshot(context_id, state.document_id, (320, 240))
                .unwrap()
                .generation,
            snapshot.generation
        );
        let button = snapshot
            .nodes
            .iter()
            .find(|node| node.label == "Continue")
            .unwrap();
        assert_eq!(button.role, "button");
        assert!(button.focused);
        assert!(button.bbox.is_none());

        let stale_document = DocumentId::new(state.document_id.get() + 1).unwrap();
        let error = controller
            .accessibility_snapshot(context_id, stale_document, (320, 240))
            .unwrap_err();
        assert_eq!(error.code, browser_error_codes::STALE_DOCUMENT);
    }

    #[test]
    fn controller_requires_exact_wire_generation_for_accessibility_focus() {
        let profile = TestProfile::new();
        let url = "https://ffi.test/accessibility-action";
        let mut config = BrowserConfig::new(&profile.0);
        config.document_overrides.insert(
            url.to_owned(),
            "<!doctype html><button aria-label='Continue'>Ignored</button>".to_owned(),
        );
        let mut controller = FlutterBrowserController::from_config(config).unwrap();
        let context_id = controller.create_context().unwrap();
        let navigation_id = controller.navigate(context_id, url).unwrap();
        wait_for_settled(&mut controller, navigation_id);
        let state = controller.context_state(context_id).unwrap();
        let snapshot = bounded_accessibility_snapshot(
            controller
                .accessibility_snapshot(context_id, state.document_id, (320, 240))
                .unwrap(),
        );
        let button = snapshot
            .nodes
            .iter()
            .find(|node| node.label == "Continue")
            .unwrap();
        let node_id = button.id;
        assert!(button.actions.iter().any(|action| action == "focus"));

        let response = controller
            .dispatch(ControllerCommand::DispatchAccessibilityAction {
                context_id,
                document_id: state.document_id,
                runtime_context_id: state.runtime_context_id.unwrap(),
                viewport: snapshot.viewport,
                source_generation: snapshot.source_generation,
                generation: snapshot.generation,
                node_id,
                action: AccessibilityAction::Focus,
            })
            .unwrap();
        assert!(matches!(response, ControllerResponse::InputDispatched(_)));
        let focused = controller
            .accessibility_snapshot(context_id, state.document_id, snapshot.viewport)
            .unwrap();
        assert!(
            focused
                .nodes
                .iter()
                .any(|node| node.id == node_id && node.focused)
        );

        let stale = controller
            .dispatch(ControllerCommand::DispatchAccessibilityAction {
                context_id,
                document_id: state.document_id,
                runtime_context_id: state.runtime_context_id.unwrap(),
                viewport: snapshot.viewport,
                source_generation: snapshot.source_generation,
                generation: snapshot.generation,
                node_id,
                action: AccessibilityAction::Focus,
            })
            .unwrap_err();
        assert_eq!(stale.code, browser_error_codes::STALE_ACCESSIBILITY);
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

    #[test]
    fn controller_find_text_uses_exact_document_and_bounded_query() {
        let profile = TestProfile::new();
        let url = "https://ffi.test/find";
        let mut config = BrowserConfig::new(&profile.0);
        config.document_overrides.insert(
            url.to_owned(),
            "<!doctype html><title>Hidden Vixen</title><main>Vixen vixen river</main>".to_owned(),
        );
        let mut controller = FlutterBrowserController::from_config(config).unwrap();
        let context_id = controller.create_context().unwrap();
        let navigation_id = controller.navigate(context_id, url).unwrap();
        wait_for_settled(&mut controller, navigation_id);
        let state = controller.context_state(context_id).unwrap();

        assert_eq!(
            controller
                .find_text(context_id, state.document_id, "Vixen", true, true)
                .unwrap()
                .matches,
            1
        );
        assert_eq!(
            controller
                .find_text(context_id, state.document_id, "vixen", false, true)
                .unwrap()
                .matches,
            2
        );
        let next = controller
            .find_text(context_id, state.document_id, "vixen", false, true)
            .unwrap();
        assert_eq!(next.active_match, Some(2));
        let previous = controller
            .find_text(context_id, state.document_id, "vixen", false, false)
            .unwrap();
        assert_eq!(previous.active_match, Some(1));
        let stale_document = DocumentId::new(state.document_id.get() + 1).unwrap();
        assert_eq!(
            controller
                .find_text(context_id, stale_document, "Vixen", false, true)
                .unwrap_err()
                .code,
            browser_error_codes::STALE_DOCUMENT
        );
        assert_eq!(
            controller
                .find_text(context_id, state.document_id, "x".repeat(4097), false, true,)
                .unwrap_err()
                .code,
            browser_error_codes::INVALID_ARGUMENT
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
