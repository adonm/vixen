//! Engine-owned browser/profile/context lifecycle.
//!
//! This is the first production owner selected by ADR-017. All `Page`, V8,
//! history, and context-registry mutation runs on one dedicated thread. The
//! main-document source loader runs off-thread with generation-tagged results;
//! parsing advances cooperatively while DOM/runtime creation and commit remain
//! on the dedicated owner thread.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::marker::PhantomData;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Duration;

use vixen_api::{
    ACCESSIBILITY_MAX_VALUE_BYTES, AccessibilityAction, AutomationEvaluation, BrowserCommand,
    BrowserCommandResult, BrowserError, BrowserEvent, BrowserHandle, BrowserId, BrowserSnapshot,
    BrowsingContextConfig, BrowsingContextId, BrowsingContextState, CrossDocumentNavigationKind,
    DocumentId, DocumentTextKind, EvaluationResult, FocusEventInfo, FocusProjection, FormEntryInfo,
    FormEntryValueInfo, FormSubmissionInfo, FrameId, HostViewState, InputDispatchResult,
    KeyEventData, MouseEventData, NavigationActionOutcome, NavigationCancellationReason,
    NavigationHistoryEntry, NavigationHistorySnapshot, NavigationId, NavigationPhase,
    ProfileDataSelection, ProfileId, ProfileSessionState, RequestId, RuntimeBindingEvent,
    RuntimeConsoleArg, RuntimeConsoleEvent, RuntimeConsoleValue, RuntimeContextId,
    RuntimeDialogEvent, RuntimeEffects, RuntimeExceptionEvent, RuntimeNetworkEvent, ScriptValue,
    TextInputState, browser_error_codes,
};
use vixen_net::{
    CookieJar, CookieJarDelta, Method, Network, NetworkConfig, NetworkEvent, RedirectMode,
    TextRequest, TextResponse,
};
use vixen_store::{ClearDataSelection, SessionRecord, Store};

use crate::data_url::parse_data_url;
use crate::display_list::PaintCommand;
use crate::history::{HistoryEntry, SessionHistory};
use crate::page::{Page, PageParser};
use crate::script::{
    ExternalPageScript, JsConsoleValue, JsNavigationAction, JsNetworkEvent, JsRuntime, JsValue,
    PageScriptRunner, PreparedPageScript, merge_profile_cookies, persist_profile_cookies,
    script_response_allowed,
};

const DEFAULT_MAX_CONTEXTS: usize = 128;

struct AccessibilityActionDispatch {
    context_id: BrowsingContextId,
    document_id: DocumentId,
    runtime_context_id: RuntimeContextId,
    viewport: (u32, u32),
    source_generation: u64,
    node_id: usize,
    action: AccessibilityAction,
}
const DEFAULT_COMMAND_CAPACITY: usize = 256;
const DEFAULT_EVENT_CAPACITY: usize = 2048;
const MAX_SCRIPT_BYTES: usize = 1024 * 1024;
const MAX_SELECTOR_BYTES: usize = 64 * 1024;
const MAX_FIND_QUERY_BYTES: usize = 4 * 1024;
const MIN_PAGE_ZOOM: f64 = 0.25;
const MAX_PAGE_ZOOM: f64 = 5.0;
const KEYBOARD_SCROLL_LINE_PX: f64 = 40.0;
const KEYBOARD_SCROLL_PAGE_FRACTION: f64 = 0.875;
const MAX_TEXT_INPUT_BYTES: usize = 16 * 1024;
const MAX_URL_BYTES: usize = 16 * 1024;
const MAX_VIEWPORT_DIMENSION: u32 = 16_384;
const MAX_RUNTIME_SLOTS: usize = 512;
const PARSER_WORK_BYTES: usize = 64 * 1024;
const MAX_NAVIGATION_ACTIONS_PER_COMMAND: usize = 64;

static NEXT_PROFILE_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_BROWSER_ID: AtomicU64 = AtomicU64::new(1);

/// Browser startup inputs supplied by a composition root.
#[derive(Debug, Clone)]
pub struct BrowserConfig {
    pub profile_path: PathBuf,
    pub network: NetworkConfig,
    pub max_contexts: usize,
    pub command_capacity: usize,
    pub event_capacity: usize,
    /// Deterministic document sources used by tests/embedded hosts. Exact URL
    /// matches bypass transport but still use the normal commit/runtime path.
    pub document_overrides: BTreeMap<String, String>,
}

impl BrowserConfig {
    pub fn new(profile_path: impl Into<PathBuf>) -> Self {
        Self {
            profile_path: profile_path.into(),
            network: NetworkConfig::default(),
            max_contexts: DEFAULT_MAX_CONTEXTS,
            command_capacity: DEFAULT_COMMAND_CAPACITY,
            event_capacity: DEFAULT_EVENT_CAPACITY,
            document_overrides: BTreeMap::new(),
        }
    }
}

/// The production channel-backed browser handle.
pub struct EngineBrowserHandle {
    commands: mpsc::SyncSender<CoreMessage>,
    events: Arc<EventChannel>,
    join: Option<std::thread::JoinHandle<()>>,
}

/// Immutable, generation-tagged renderer input. Host surfaces own GL/EGL;
/// BrowserCore owns the Page/display-list generation.
#[derive(Debug, Clone)]
pub struct PaintSnapshot {
    pub context_id: BrowsingContextId,
    pub document_id: DocumentId,
    pub viewport: (u32, u32),
    pub commands: Vec<PaintCommand>,
}

impl EngineBrowserHandle {
    pub fn dispatch(
        &mut self,
        command: BrowserCommand,
    ) -> Result<BrowserCommandResult, BrowserError> {
        BrowserHandle::dispatch(self, command)
    }

    pub fn try_next_event(&mut self) -> Result<Option<BrowserEvent>, BrowserError> {
        BrowserHandle::try_next_event(self)
    }

    /// Wait for one ordered browser event, bounded by `timeout`.
    ///
    /// On `browser.event-lagged`, pending frontend operations are indeterminate;
    /// dispatch [`BrowserCommand::GetBrowserSnapshot`] and reconcile instead of
    /// assuming those operations succeeded.
    pub fn wait_next_event(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<BrowserEvent>, BrowserError> {
        self.events.pop(Some(timeout))
    }

    pub fn capture_paint_snapshot(
        &mut self,
        context_id: BrowsingContextId,
        document_id: DocumentId,
        viewport: (u32, u32),
    ) -> Result<PaintSnapshot, BrowserError> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.commands
            .try_send(CoreMessage::CapturePaint {
                context_id,
                document_id,
                viewport,
                reply: reply_tx,
            })
            .map_err(command_send_error)?;
        reply_rx.recv().map_err(|_| {
            BrowserError::new(
                browser_error_codes::CLOSED,
                "browser core closed before capturing paint",
            )
        })?
    }

    /// Test/embedded-host source injection that still traverses BrowserCore's
    /// normal navigation, commit, runtime, history, and event path.
    pub fn navigate_html(
        &mut self,
        context_id: BrowsingContextId,
        url: String,
        html: String,
    ) -> Result<BrowserCommandResult, BrowserError> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.commands
            .try_send(CoreMessage::NavigateHtml {
                context_id,
                url,
                html,
                reply: reply_tx,
            })
            .map_err(command_send_error)?;
        reply_rx.recv().map_err(|_| {
            BrowserError::new(
                browser_error_codes::CLOSED,
                "browser core closed before loading injected HTML",
            )
        })?
    }
}

impl BrowserHandle for EngineBrowserHandle {
    fn dispatch(&mut self, command: BrowserCommand) -> Result<BrowserCommandResult, BrowserError> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.commands
            .try_send(CoreMessage::Dispatch {
                command,
                reply: reply_tx,
            })
            .map_err(command_send_error)?;
        reply_rx.recv().map_err(|_| {
            BrowserError::new(
                browser_error_codes::CLOSED,
                "browser core closed before acknowledging the command",
            )
        })?
    }

    fn try_next_event(&mut self) -> Result<Option<BrowserEvent>, BrowserError> {
        self.events.pop(None)
    }
}

impl Drop for EngineBrowserHandle {
    fn drop(&mut self) {
        let _ = self.commands.send(CoreMessage::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Start one browser core and open its profile exactly once on the engine
/// thread. Startup is acknowledged before the handle is returned.
pub fn spawn_browser(config: BrowserConfig) -> Result<EngineBrowserHandle, BrowserError> {
    let command_capacity = config.command_capacity.max(1);
    let events = Arc::new(EventChannel::new(config.event_capacity.max(1)));
    let core_events = Arc::clone(&events);
    let (command_tx, command_rx) = mpsc::sync_channel(command_capacity);
    let core_commands = command_tx.clone();
    let (start_tx, start_rx) = mpsc::sync_channel(1);
    let join = std::thread::Builder::new()
        .name("vixen-browser-core".to_owned())
        .spawn(move || {
            let mut core = match BrowserCore::new(config, core_events, core_commands) {
                Ok(core) => {
                    let _ = start_tx.send(Ok(()));
                    core
                }
                Err(error) => {
                    let _ = start_tx.send(Err(error));
                    return;
                }
            };
            'owner: while let Ok(message) = command_rx.recv() {
                if handle_core_message(&mut core, message) {
                    break;
                }
                loop {
                    core.advance_navigation_work();
                    match command_rx.try_recv() {
                        Ok(message) => {
                            if handle_core_message(&mut core, message) {
                                break 'owner;
                            }
                        }
                        Err(mpsc::TryRecvError::Empty) if core.has_navigation_work() => {}
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => break 'owner,
                    }
                }
            }
            core.shutdown();
        })
        .map_err(|error| {
            BrowserError::new(
                browser_error_codes::CLOSED,
                format!("failed to start browser core thread: {error}"),
            )
        })?;

    match start_rx.recv() {
        Ok(Ok(())) => Ok(EngineBrowserHandle {
            commands: command_tx,
            events,
            join: Some(join),
        }),
        Ok(Err(error)) => {
            let _ = join.join();
            Err(error)
        }
        Err(_) => {
            let _ = join.join();
            Err(BrowserError::new(
                browser_error_codes::CLOSED,
                "browser core exited during startup",
            ))
        }
    }
}

enum CoreMessage {
    Dispatch {
        command: BrowserCommand,
        reply: mpsc::SyncSender<Result<BrowserCommandResult, BrowserError>>,
    },
    CapturePaint {
        context_id: BrowsingContextId,
        document_id: DocumentId,
        viewport: (u32, u32),
        reply: mpsc::SyncSender<Result<PaintSnapshot, BrowserError>>,
    },
    NavigateHtml {
        context_id: BrowsingContextId,
        url: String,
        html: String,
        reply: mpsc::SyncSender<Result<BrowserCommandResult, BrowserError>>,
    },
    SourceLoadProgress(SourceLoadProgress),
    SourceLoaded(SourceLoadCompletion),
    ExternalScriptLoaded(ExternalScriptLoadCompletion),
    Shutdown,
}

fn handle_core_message(core: &mut BrowserCore, message: CoreMessage) -> bool {
    match message {
        CoreMessage::Dispatch { command, reply } => {
            let _ = reply.send(core.dispatch(command));
            core.start_pending_loads();
        }
        CoreMessage::CapturePaint {
            context_id,
            document_id,
            viewport,
            reply,
        } => {
            let _ = reply.send(core.capture_paint_snapshot(context_id, document_id, viewport));
        }
        CoreMessage::NavigateHtml {
            context_id,
            url,
            html,
            reply,
        } => {
            let _ = reply.send(core.navigate_html(context_id, url, html));
            core.start_pending_loads();
        }
        CoreMessage::SourceLoaded(completion) => {
            core.complete_navigation(completion);
            core.start_pending_loads();
        }
        CoreMessage::SourceLoadProgress(progress) => {
            core.progress_navigation(progress);
        }
        CoreMessage::ExternalScriptLoaded(completion) => {
            core.complete_external_script(completion);
        }
        CoreMessage::Shutdown => {
            core.shutdown();
            return true;
        }
    }
    false
}

struct EventChannel {
    queue: Mutex<EventQueue>,
    ready: Condvar,
}

impl EventChannel {
    fn new(capacity: usize) -> Self {
        Self {
            queue: Mutex::new(EventQueue::new(capacity)),
            ready: Condvar::new(),
        }
    }

    fn push(&self, event: BrowserEvent) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.push(event);
            self.ready.notify_all();
        }
    }

    fn pop(&self, timeout: Option<Duration>) -> Result<Option<BrowserEvent>, BrowserError> {
        let mut queue = self.queue.lock().map_err(|_| event_queue_poisoned())?;
        if let Some(timeout) = timeout
            && queue.dropped == 0
            && queue.events.is_empty()
        {
            let (next, _) = self
                .ready
                .wait_timeout_while(queue, timeout, |queue| {
                    queue.dropped == 0 && queue.events.is_empty()
                })
                .map_err(|_| event_queue_poisoned())?;
            queue = next;
        }
        if queue.dropped > 0 {
            let dropped = std::mem::take(&mut queue.dropped);
            return Err(BrowserError::new(
                browser_error_codes::EVENT_LAGGED,
                format!("browser event consumer lagged; dropped {dropped} events"),
            ));
        }
        Ok(queue.events.pop_front())
    }
}

struct EventQueue {
    capacity: usize,
    dropped: usize,
    events: VecDeque<BrowserEvent>,
}

impl EventQueue {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            dropped: 0,
            events: VecDeque::with_capacity(capacity),
        }
    }

    fn push(&mut self, event: BrowserEvent) {
        if self.events.len() == self.capacity {
            self.events.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.events.push_back(event);
    }
}

struct BrowserCore {
    _profile_id: ProfileId,
    _browser_id: BrowserId,
    ids: IdAllocator,
    contexts: BTreeMap<BrowsingContextId, BrowsingContext>,
    runtime_slots: Vec<RuntimeSlot>,
    closed_contexts: BTreeSet<BrowsingContextId>,
    active_context: Option<BrowsingContextId>,
    max_contexts: usize,
    events: Arc<EventChannel>,
    command_tx: mpsc::SyncSender<CoreMessage>,
    store: Arc<Store>,
    network_config: NetworkConfig,
    network: Network,
    cookies: CookieJar,
    network_runtime: Option<tokio::runtime::Runtime>,
    pending_load_starts: Vec<tokio::sync::oneshot::Sender<()>>,
    pending_navigation_work: VecDeque<(BrowsingContextId, NavigationId)>,
    document_overrides: BTreeMap<String, String>,
    // Makes accidental movement of the DOM/V8 owner across threads impossible.
    _local_only: PhantomData<Rc<()>>,
}

struct BrowsingContext {
    frame_id: FrameId,
    document_id: DocumentId,
    runtime_context_id: RuntimeContextId,
    active_navigation: Option<ActiveNavigation>,
    page: Page,
    runtime_slot: usize,
    config: BrowsingContextConfig,
    host_view: HostViewState,
    page_zoom: f64,
}

struct RuntimeSlot {
    runtime: JsRuntime,
    active: bool,
}

struct ActiveNavigation {
    navigation_id: NavigationId,
    request_id: RequestId,
    history_update: HistoryUpdate,
    work: Option<NavigationWork>,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    load_task: Option<tokio::task::AbortHandle>,
    pending_script: Option<PendingExternalScript>,
}

enum PreparedNavigationAction {
    CrossDocument {
        url: String,
        injected_html: Option<String>,
        history_update: HistoryUpdate,
        kind: CrossDocumentNavigationKind,
    },
    SameDocument {
        history: SessionHistory,
        url: String,
    },
}

enum NavigationWork {
    Parsing(Box<PageParser>),
    Scripts(NavigationScriptWork),
    Lifecycle {
        stage: LifecycleStage,
        actions: Vec<JsNavigationAction>,
    },
}

struct NavigationScriptWork {
    preload_scripts: VecDeque<String>,
    new_document_scripts: VecDeque<String>,
    author_scripts: Option<PageScriptRunner>,
    bypass_csp: bool,
    actions: Vec<JsNavigationAction>,
}

enum NavigationScriptStep {
    Host(String),
    Author(PreparedPageScript),
    Complete,
}

struct PendingExternalScript {
    key: ExternalScriptLoadKey,
    request: ExternalPageScript,
    work: NavigationScriptWork,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ExternalScriptLoadKey {
    context_id: BrowsingContextId,
    navigation_id: NavigationId,
    document_id: DocumentId,
    runtime_context_id: RuntimeContextId,
    request_id: RequestId,
}

#[derive(Clone, Copy)]
enum LifecycleStage {
    DomContentLoaded,
    Load,
    Settle,
}

enum NavigationTerminal {
    Settled,
    Failed {
        request_id: Option<RequestId>,
        error: BrowserError,
    },
    Cancelled {
        reason: NavigationCancellationReason,
    },
}

#[derive(Default)]
struct IdAllocator {
    next_context: u64,
    next_frame: u64,
    next_navigation: u64,
    next_document: u64,
    next_request: u64,
    next_runtime: u64,
}

impl IdAllocator {
    fn new() -> Self {
        Self {
            next_context: 1,
            next_frame: 1,
            next_navigation: 1,
            next_document: 1,
            next_request: 1,
            next_runtime: 1,
        }
    }

    fn context(&mut self) -> Result<BrowsingContextId, BrowserError> {
        next_typed_id(&mut self.next_context, BrowsingContextId::new)
    }

    fn frame(&mut self) -> Result<FrameId, BrowserError> {
        next_typed_id(&mut self.next_frame, FrameId::new)
    }

    fn navigation(&mut self) -> Result<NavigationId, BrowserError> {
        next_typed_id(&mut self.next_navigation, NavigationId::new)
    }

    fn document(&mut self) -> Result<DocumentId, BrowserError> {
        next_typed_id(&mut self.next_document, DocumentId::new)
    }

    fn request(&mut self) -> Result<RequestId, BrowserError> {
        next_typed_id(&mut self.next_request, RequestId::new)
    }

    fn runtime(&mut self) -> Result<RuntimeContextId, BrowserError> {
        next_typed_id(&mut self.next_runtime, RuntimeContextId::new)
    }
}

fn next_typed_id<T>(
    counter: &mut u64,
    constructor: impl FnOnce(u64) -> Option<T>,
) -> Result<T, BrowserError> {
    let raw = *counter;
    if raw == 0 {
        return Err(BrowserError::new(
            browser_error_codes::ID_EXHAUSTED,
            "browser id space is exhausted",
        ));
    }
    *counter = raw.checked_add(1).unwrap_or(0);
    constructor(raw).ok_or_else(|| {
        BrowserError::new(
            browser_error_codes::ID_EXHAUSTED,
            "browser id allocation produced zero",
        )
    })
}

#[derive(Clone)]
enum HistoryUpdate {
    Push,
    Replace,
    Preserve(SessionHistory),
}

struct LoadedSource {
    final_url: String,
    html: String,
    headers: Vec<(String, String)>,
}

struct SourceLoadProgress {
    context_id: BrowsingContextId,
    navigation_id: NavigationId,
    event: NetworkEvent,
}

struct SourceLoadCompletion {
    context_id: BrowsingContextId,
    navigation_id: NavigationId,
    result: Result<LoadedSource, BrowserError>,
    cookie_delta: CookieJarDelta,
}

struct SourceLoadInput {
    url: String,
    injected_html: Option<String>,
}

struct ExternalScriptLoadCompletion {
    key: ExternalScriptLoadKey,
    result: Result<LoadedExternalScript, ExternalScriptLoadFailure>,
    cookie_delta: CookieJarDelta,
}

enum LoadedExternalScript {
    File {
        final_url: url::Url,
        source: String,
    },
    Http {
        response: TextResponse,
        requested_urls: Vec<url::Url>,
    },
}

struct ExternalScriptLoadFailure {
    error: BrowserError,
    url: String,
    events: Vec<NetworkEvent>,
    blocked_reason: &'static str,
}

impl Drop for BrowserCore {
    fn drop(&mut self) {
        self.cancel_all_loads();
        // rusty_v8 enters isolates for their whole lifetime. Runtime slots are
        // therefore retained (bounded above) and destroyed in strict reverse
        // construction order so each drop exits the currently entered isolate.
        while let Some(slot) = self.runtime_slots.pop() {
            drop(slot);
        }
        if let Some(runtime) = self.network_runtime.take() {
            runtime.shutdown_timeout(Duration::from_secs(1));
        }
    }
}

impl BrowserCore {
    fn new(
        config: BrowserConfig,
        events: Arc<EventChannel>,
        command_tx: mpsc::SyncSender<CoreMessage>,
    ) -> Result<Self, BrowserError> {
        if let Some(parent) = config.profile_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                BrowserError::new(
                    browser_error_codes::INVALID_ARGUMENT,
                    format!("failed to create profile directory: {error}"),
                )
            })?;
        }
        let store = Arc::new(Store::open(&config.profile_path).map_err(|error| {
            BrowserError::new(
                browser_error_codes::INVALID_ARGUMENT,
                format!("failed to open browser profile: {error}"),
            )
        })?);
        let network = Network::new(config.network.clone()).map_err(|error| {
            BrowserError::new(
                browser_error_codes::NAVIGATION_LOAD,
                format!("failed to initialise profile network service: {error}"),
            )
        })?;
        let network_runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("vixen-source-loader")
            .enable_all()
            .build()
            .map_err(|error| {
                BrowserError::new(
                    browser_error_codes::NAVIGATION_LOAD,
                    format!("failed to initialise profile network runtime: {error}"),
                )
            })?;

        Ok(Self {
            _profile_id: next_process_id(&NEXT_PROFILE_ID, ProfileId::new)?,
            _browser_id: next_process_id(&NEXT_BROWSER_ID, BrowserId::new)?,
            ids: IdAllocator::new(),
            contexts: BTreeMap::new(),
            runtime_slots: Vec::new(),
            closed_contexts: BTreeSet::new(),
            active_context: None,
            max_contexts: config.max_contexts.max(1),
            events,
            command_tx,
            store,
            network_config: config.network,
            network,
            cookies: CookieJar::default(),
            network_runtime: Some(network_runtime),
            pending_load_starts: Vec::new(),
            pending_navigation_work: VecDeque::new(),
            document_overrides: config.document_overrides,
            _local_only: PhantomData,
        })
    }

    fn dispatch(&mut self, command: BrowserCommand) -> Result<BrowserCommandResult, BrowserError> {
        match command {
            BrowserCommand::LoadProfileSession => self.load_profile_session(),
            BrowserCommand::SaveCurrentProfileSession => self.save_current_profile_session(),
            BrowserCommand::SaveProfileSession { session } => self.save_profile_session(session),
            BrowserCommand::ClearProfileData { selection } => self.clear_profile_data(selection),
            BrowserCommand::CreateBrowsingContext => self.create_context(),
            BrowserCommand::CloseBrowsingContext { context_id } => self.close_context(context_id),
            BrowserCommand::ActivateBrowsingContext { context_id } => {
                self.ensure_context(context_id)?;
                self.active_context = Some(context_id);
                self.emit(BrowserEvent::ActiveBrowsingContextChanged {
                    context_id: Some(context_id),
                });
                Ok(BrowserCommandResult::Accepted)
            }
            BrowserCommand::Navigate { context_id, url } => {
                self.navigate(context_id, url, HistoryUpdate::Push)
            }
            BrowserCommand::Reload { context_id } => {
                let (url, history) = {
                    let context = self.context(context_id)?;
                    (
                        context.page.url().to_owned(),
                        context.page.session_history().clone(),
                    )
                };
                self.navigate(context_id, url, HistoryUpdate::Preserve(history))
            }
            BrowserCommand::Stop { context_id } => self.stop(context_id),
            BrowserCommand::TraverseHistory { context_id, delta } => {
                let context = self.context(context_id)?;
                let mut history = context.page.session_history().clone();
                let Some(entry) = history.go(delta).cloned() else {
                    return Ok(BrowserCommandResult::Accepted);
                };
                self.navigate(context_id, entry.url, HistoryUpdate::Preserve(history))
            }
            BrowserCommand::GetBrowsingContextState { context_id } => Ok(
                BrowserCommandResult::BrowsingContextState(self.context_state(context_id)?),
            ),
            BrowserCommand::UpdateHostViewState { context_id, state } => {
                self.update_host_view_state(context_id, state)
            }
            BrowserCommand::SetPageZoom { context_id, zoom } => {
                if !zoom.is_finite() || !(MIN_PAGE_ZOOM..=MAX_PAGE_ZOOM).contains(&zoom) {
                    return Err(BrowserError::new(
                        browser_error_codes::INVALID_ARGUMENT,
                        format!(
                            "page zoom must be finite and between {MIN_PAGE_ZOOM} and {MAX_PAGE_ZOOM}"
                        ),
                    ));
                }
                let context = self.context_mut(context_id)?;
                context.page_zoom = zoom;
                let layout_viewport = page_layout_viewport(context.host_view.viewport, zoom);
                context.page.set_layout_viewport(layout_viewport);
                let document_id = context.document_id;
                let runtime_context_id = context.runtime_context_id;
                let source = host_view_runtime_source(&context.page, context.host_view);
                self.automation_evaluation(context_id, document_id, runtime_context_id, source)?;
                let state = self.context_state(context_id)?;
                Ok(BrowserCommandResult::BrowsingContextState(state))
            }
            BrowserCommand::GetBrowserSnapshot => Ok(BrowserCommandResult::BrowserSnapshot(
                self.browser_snapshot(),
            )),
            BrowserCommand::ConfigureBrowsingContext { context_id, config } => {
                self.configure_context(context_id, config)
            }
            BrowserCommand::GetNavigationHistory { context_id } => {
                let context = self.context(context_id)?;
                let history = context.page.session_history();
                Ok(BrowserCommandResult::NavigationHistory(
                    NavigationHistorySnapshot {
                        current_index: history.index(),
                        entries: history
                            .entries()
                            .iter()
                            .map(|entry| NavigationHistoryEntry {
                                url: entry.url.clone(),
                                title: entry.title.clone(),
                                same_document: entry.state.is_some(),
                            })
                            .collect(),
                    },
                ))
            }
            BrowserCommand::ResetNavigationHistory { context_id } => {
                let context = self.context_mut(context_id)?;
                let entry = HistoryEntry::navigation(context.page.url().to_owned());
                context.page.set_session_history(SessionHistory::new(entry));
                Ok(BrowserCommandResult::Accepted)
            }
            BrowserCommand::Evaluate {
                context_id,
                document_id,
                runtime_context_id,
                source,
            } => self.evaluate(context_id, document_id, runtime_context_id, source),
            BrowserCommand::EvaluateForAutomation {
                context_id,
                document_id,
                runtime_context_id,
                source,
            } => self.evaluate_for_automation(context_id, document_id, runtime_context_id, source),
            BrowserCommand::DispatchMouseEvent {
                context_id,
                document_id,
                runtime_context_id,
                node_id,
                event_type,
                event,
            } => self.dispatch_mouse_event(
                context_id,
                document_id,
                runtime_context_id,
                node_id,
                event_type,
                event,
            ),
            BrowserCommand::DispatchKeyEvent {
                context_id,
                document_id,
                runtime_context_id,
                event_type,
                event,
            } => self.dispatch_key_event(
                context_id,
                document_id,
                runtime_context_id,
                event_type,
                event,
            ),
            BrowserCommand::DispatchTextInput {
                context_id,
                document_id,
                runtime_context_id,
                state,
            } => self.dispatch_text_input(context_id, document_id, runtime_context_id, state),
            BrowserCommand::FindText {
                context_id,
                document_id,
                query,
                case_sensitive,
                forward,
            } => {
                if query.len() > MAX_FIND_QUERY_BYTES {
                    return Err(BrowserError::new(
                        browser_error_codes::INVALID_ARGUMENT,
                        "find query exceeds the browser limit",
                    ));
                }
                let context = self.context_for_document_mut(context_id, document_id)?;
                let viewport = page_layout_viewport(context.host_view.viewport, context.page_zoom);
                Ok(BrowserCommandResult::FindText(context.page.find_text(
                    &query,
                    case_sensitive,
                    forward,
                    viewport,
                )))
            }
            BrowserCommand::Snapshot {
                context_id,
                document_id,
                viewport,
            } => {
                validate_viewport(viewport)?;
                let context = self.context_for_document(context_id, document_id)?;
                Ok(BrowserCommandResult::Snapshot(
                    context.page.snapshot(viewport),
                ))
            }
            BrowserCommand::AccessibilitySnapshot {
                context_id,
                document_id,
                viewport,
            } => {
                validate_viewport(viewport)?;
                Ok(BrowserCommandResult::AccessibilitySnapshot(
                    self.accessibility_snapshot(context_id, document_id, viewport)?,
                ))
            }
            BrowserCommand::DispatchAccessibilityAction {
                context_id,
                document_id,
                runtime_context_id,
                viewport,
                source_generation,
                node_id,
                action,
            } => self.dispatch_accessibility_action(AccessibilityActionDispatch {
                context_id,
                document_id,
                runtime_context_id,
                viewport,
                source_generation,
                node_id,
                action,
            }),
            BrowserCommand::QuerySelectorAll {
                context_id,
                document_id,
                selector,
                viewport,
            } => {
                validate_viewport(viewport)?;
                if selector.len() > MAX_SELECTOR_BYTES {
                    return Err(BrowserError::new(
                        browser_error_codes::INVALID_ARGUMENT,
                        "selector exceeds the browser query limit",
                    ));
                }
                let context = self.context_for_document(context_id, document_id)?;
                let matches = context
                    .page
                    .query_selector_all_in_viewport(&selector, viewport)
                    .map_err(|message| {
                        BrowserError::new(browser_error_codes::INVALID_ARGUMENT, message)
                    })?;
                Ok(BrowserCommandResult::SelectorMatches(matches))
            }
            BrowserCommand::ComputedStyle {
                context_id,
                document_id,
                node_id,
                viewport,
            } => {
                validate_viewport(viewport)?;
                let context = self.context_for_document(context_id, document_id)?;
                Ok(BrowserCommandResult::ComputedStyle(
                    context.page.computed_style_for_viewport(node_id, viewport),
                ))
            }
            BrowserCommand::DisplayListText {
                context_id,
                document_id,
                viewport,
            } => {
                validate_viewport(viewport)?;
                let context = self.context_for_document(context_id, document_id)?;
                Ok(BrowserCommandResult::DisplayListText(
                    context.page.dump_display_list(viewport),
                ))
            }
            BrowserCommand::Diagnostics {
                context_id,
                document_id,
            } => {
                let context = self.context_for_document(context_id, document_id)?;
                Ok(BrowserCommandResult::Diagnostics(
                    context.page.diagnostics(),
                ))
            }
            BrowserCommand::DocumentText {
                context_id,
                document_id,
                viewport,
                kind,
            } => {
                validate_viewport(viewport)?;
                let context = self.context_for_document(context_id, document_id)?;
                let text = match kind {
                    DocumentTextKind::Dom => context.page.dump_dom(),
                    DocumentTextKind::TextContent => context.page.text_content(),
                    DocumentTextKind::LayoutTree => context.page.dump_layout_tree(viewport),
                    DocumentTextKind::Lines => context.page.dump_lines(viewport),
                    DocumentTextKind::PaintStats => context.page.dump_paint_stats(viewport),
                };
                Ok(BrowserCommandResult::DocumentText(text))
            }
            BrowserCommand::HitTest {
                context_id,
                document_id,
                viewport,
                x,
                y,
            } => {
                validate_viewport(viewport)?;
                if !x.is_finite() || !y.is_finite() {
                    return Err(BrowserError::new(
                        browser_error_codes::INVALID_ARGUMENT,
                        "hit-test coordinates must be finite",
                    ));
                }
                let context = self.context_for_document(context_id, document_id)?;
                let layout_viewport = page_layout_viewport(viewport, context.page_zoom);
                Ok(BrowserCommandResult::HitTest(context.page.element_at(
                    layout_viewport,
                    x / context.page_zoom,
                    y / context.page_zoom,
                )))
            }
            BrowserCommand::FocusProjection {
                context_id,
                document_id,
                element_id,
            } => {
                validate_lookup_id(&element_id)?;
                let context = self.context_for_document(context_id, document_id)?;
                let target = context.page.element_by_id(&element_id).ok_or_else(|| {
                    BrowserError::new(
                        browser_error_codes::INVALID_ARGUMENT,
                        format!("no element with id '{element_id}'"),
                    )
                })?;
                let events = crate::event_path::focus_event_sequence(None, Some(target.node_id))
                    .into_iter()
                    .map(|event| FocusEventInfo {
                        event: event.event.to_owned(),
                        target: event.target,
                        bubbles: event.bubbles,
                    })
                    .collect();
                Ok(BrowserCommandResult::FocusProjection(FocusProjection {
                    target,
                    events,
                }))
            }
            BrowserCommand::FormSubmission {
                context_id,
                document_id,
                form_id,
            } => {
                validate_lookup_id(&form_id)?;
                let context = self.context_for_document(context_id, document_id)?;
                let submission = context.page.form_submission(&form_id).map_err(|message| {
                    BrowserError::new(browser_error_codes::INVALID_ARGUMENT, message)
                })?;
                Ok(BrowserCommandResult::FormSubmission(form_submission_info(
                    submission,
                )))
            }
        }
    }

    fn load_profile_session(&self) -> Result<BrowserCommandResult, BrowserError> {
        let record = self.store.load_session_record().map_err(profile_error)?;
        Ok(BrowserCommandResult::ProfileSession(ProfileSessionState {
            tabs: record.tabs,
            active_index: record.active_index,
        }))
    }

    fn save_profile_session(
        &self,
        session: ProfileSessionState,
    ) -> Result<BrowserCommandResult, BrowserError> {
        self.store
            .save_session_record(&SessionRecord {
                tabs: session.tabs,
                active_index: session.active_index,
                tab_states: Vec::new(),
            })
            .map_err(profile_error)?;
        Ok(BrowserCommandResult::Accepted)
    }

    fn save_current_profile_session(&self) -> Result<BrowserCommandResult, BrowserError> {
        let contexts = self.contexts.keys().copied().collect::<Vec<_>>();
        let session = ProfileSessionState {
            tabs: contexts
                .iter()
                .map(|context_id| self.contexts[context_id].page.url().to_owned())
                .collect(),
            active_index: self
                .active_context
                .and_then(|active| contexts.iter().position(|context_id| *context_id == active))
                .unwrap_or(0),
        };
        self.save_profile_session(session)
    }

    fn browser_snapshot(&self) -> BrowserSnapshot {
        BrowserSnapshot {
            active_context_id: self.active_context,
            contexts: self
                .contexts
                .iter()
                .map(|(context_id, context)| context_state(*context_id, context))
                .collect(),
        }
    }

    fn clear_profile_data(
        &mut self,
        selection: ProfileDataSelection,
    ) -> Result<BrowserCommandResult, BrowserError> {
        self.store
            .clear_profile_data(ClearDataSelection {
                cookies: selection.cookies,
                fetch_cache: selection.fetch_cache,
                history: selection.history,
                session: selection.session,
                web_storage: selection.web_storage,
                downloads: selection.downloads,
                permissions: selection.permissions,
                security_state: selection.security_state,
            })
            .map_err(profile_error)?;
        if selection.cookies {
            self.cookies = CookieJar::default();
        }
        if selection.cookies || selection.fetch_cache {
            for slot in self.runtime_slots.iter().filter(|slot| slot.active) {
                slot.runtime
                    .clear_profile_network_state(selection.cookies, selection.fetch_cache);
            }
        }
        Ok(BrowserCommandResult::Accepted)
    }

    fn capture_paint_snapshot(
        &self,
        context_id: BrowsingContextId,
        document_id: DocumentId,
        viewport: (u32, u32),
    ) -> Result<PaintSnapshot, BrowserError> {
        validate_viewport(viewport)?;
        let context = self.context_for_document(context_id, document_id)?;
        let layout_viewport = page_layout_viewport(viewport, context.page_zoom);
        let mut commands = context.page.display_list(layout_viewport);
        scale_paint_commands(&mut commands, context.page_zoom as f32);
        Ok(PaintSnapshot {
            context_id,
            document_id,
            viewport,
            commands,
        })
    }

    fn accessibility_snapshot(
        &self,
        context_id: BrowsingContextId,
        document_id: DocumentId,
        viewport: (u32, u32),
    ) -> Result<vixen_api::AccessibilitySnapshot, BrowserError> {
        let context = self.context_for_document(context_id, document_id)?;
        let layout_viewport = page_layout_viewport(viewport, context.page_zoom);
        let mut snapshot =
            context
                .page
                .accessibility_snapshot(context_id, document_id, layout_viewport);
        snapshot.viewport = viewport;
        for node in &mut snapshot.nodes {
            if let Some(bounds) = &mut node.bbox {
                bounds.x *= context.page_zoom;
                bounds.y *= context.page_zoom;
                bounds.width *= context.page_zoom;
                bounds.height *= context.page_zoom;
            }
        }
        snapshot.refresh_generation();
        Ok(snapshot)
    }

    fn navigate_html(
        &mut self,
        context_id: BrowsingContextId,
        url: String,
        html: String,
    ) -> Result<BrowserCommandResult, BrowserError> {
        self.navigate_injected(context_id, url, html, HistoryUpdate::Push)
    }

    fn navigate_injected(
        &mut self,
        context_id: BrowsingContextId,
        url: String,
        html: String,
        history_update: HistoryUpdate,
    ) -> Result<BrowserCommandResult, BrowserError> {
        if html.len() > self.network_config.max_body_bytes as usize {
            return Err(BrowserError::new(
                browser_error_codes::INVALID_ARGUMENT,
                "injected document exceeds the browser body limit",
            ));
        }
        self.begin_navigation(
            context_id,
            url,
            Some(html),
            history_update,
            CrossDocumentNavigationKind::Regular,
            None,
        )
    }

    fn create_context(&mut self) -> Result<BrowserCommandResult, BrowserError> {
        if self.contexts.len() >= self.max_contexts {
            return Err(BrowserError::new(
                browser_error_codes::CONTEXT_LIMIT,
                format!("browser context limit {} reached", self.max_contexts),
            ));
        }
        let context_id = self.ids.context()?;
        let frame_id = self.ids.frame()?;
        let document_id = self.ids.document()?;
        let runtime_context_id = self.ids.runtime()?;
        let page =
            Page::from_html("about:blank", "<!doctype html><title></title>").map_err(|error| {
                BrowserError::new(browser_error_codes::NAVIGATION_LOAD, error.to_string())
            })?;
        let runtime = JsRuntime::with_browser_storage(
            self.network_config.clone(),
            Arc::clone(&self.store),
            format!("context-{}", context_id.get()),
            &page,
        )
        .map_err(engine_error)?;
        let runtime_slot = self.runtime_slots.len();
        self.runtime_slots.push(RuntimeSlot {
            runtime,
            active: true,
        });
        self.contexts.insert(
            context_id,
            BrowsingContext {
                frame_id,
                document_id,
                runtime_context_id,
                active_navigation: None,
                page,
                runtime_slot,
                config: BrowsingContextConfig::default(),
                host_view: HostViewState::default(),
                page_zoom: 1.0,
            },
        );
        let state = self.context_state(context_id)?;
        self.emit(BrowserEvent::BrowsingContextCreated { state });
        if self.active_context.is_none() {
            self.active_context = Some(context_id);
            self.emit(BrowserEvent::ActiveBrowsingContextChanged {
                context_id: Some(context_id),
            });
        }
        Ok(BrowserCommandResult::BrowsingContextCreated { context_id })
    }

    fn close_context(
        &mut self,
        context_id: BrowsingContextId,
    ) -> Result<BrowserCommandResult, BrowserError> {
        self.ensure_context(context_id)?;
        if let Some(navigation_id) = self
            .context(context_id)?
            .active_navigation
            .as_ref()
            .map(|navigation| navigation.navigation_id)
        {
            self.finish_navigation(
                context_id,
                navigation_id,
                NavigationTerminal::Cancelled {
                    reason: NavigationCancellationReason::ContextClosed,
                },
                false,
            )?;
        }
        let context = self.contexts.remove(&context_id).expect("context checked");
        self.runtime_slots[context.runtime_slot].active = false;
        self.emit(BrowserEvent::RuntimeContextDestroyed {
            context_id,
            frame_id: context.frame_id,
            document_id: context.document_id,
            runtime_context_id: context.runtime_context_id,
        });
        self.emit(BrowserEvent::DocumentDiscarded {
            context_id,
            frame_id: context.frame_id,
            document_id: context.document_id,
            replaced_by: None,
        });
        self.closed_contexts.insert(context_id);
        self.emit(BrowserEvent::BrowsingContextClosed { context_id });
        if self.active_context == Some(context_id) {
            self.active_context = self.contexts.keys().next().copied();
            self.emit(BrowserEvent::ActiveBrowsingContextChanged {
                context_id: self.active_context,
            });
        }
        self.prune_runtime_slots();
        Ok(BrowserCommandResult::Accepted)
    }

    fn navigate(
        &mut self,
        context_id: BrowsingContextId,
        url: String,
        history_update: HistoryUpdate,
    ) -> Result<BrowserCommandResult, BrowserError> {
        self.begin_navigation(
            context_id,
            url,
            None,
            history_update,
            CrossDocumentNavigationKind::Regular,
            None,
        )
    }

    fn begin_navigation(
        &mut self,
        context_id: BrowsingContextId,
        url: String,
        injected_html: Option<String>,
        history_update: HistoryUpdate,
        kind: CrossDocumentNavigationKind,
        predecessor_navigation_id: Option<NavigationId>,
    ) -> Result<BrowserCommandResult, BrowserError> {
        self.ensure_context(context_id)?;
        if url.len() > MAX_URL_BYTES {
            return Err(BrowserError::new(
                browser_error_codes::INVALID_ARGUMENT,
                "navigation URL exceeds the browser limit",
            ));
        }
        let navigation_id = self.ids.navigation()?;
        let initial_request_id = self.ids.request()?;
        let frame_id = self.context(context_id)?.frame_id;
        self.cancel_active_navigation(context_id, NavigationCancellationReason::Superseded, false)?;
        let (cancel, cancel_rx) = tokio::sync::oneshot::channel();
        let (start, start_rx) = tokio::sync::oneshot::channel();
        self.context_mut(context_id)?.active_navigation = Some(ActiveNavigation {
            navigation_id,
            request_id: initial_request_id,
            history_update,
            work: None,
            cancel: Some(cancel),
            load_task: None,
            pending_script: None,
        });
        self.emit(BrowserEvent::NavigationRequested {
            context_id,
            frame_id,
            navigation_id,
            predecessor_navigation_id,
            kind,
            url: url.clone(),
        });
        self.emit_phase(context_id, frame_id, navigation_id, NavigationPhase::Intent);
        self.emit(BrowserEvent::BrowsingContextStateChanged {
            state: self.context_state(context_id)?,
        });
        self.emit_phase(context_id, frame_id, navigation_id, NavigationPhase::Policy);
        self.emit(BrowserEvent::NavigationStarted {
            context_id,
            frame_id,
            navigation_id,
            request_id: initial_request_id,
            url: url.clone(),
        });
        self.emit_phase(
            context_id,
            frame_id,
            navigation_id,
            NavigationPhase::Request,
        );

        let input = SourceLoadInput {
            injected_html: injected_html.or_else(|| self.document_overrides.get(&url).cloned()),
            url,
        };
        let baseline = self.cookies.snapshots();
        let mut worker_jar = CookieJar::from_snapshots(baseline.clone());
        let mut network = self.network.clone();
        let max_body_bytes = self.network_config.max_body_bytes;
        let command_tx = self.command_tx.clone();
        let load_task = self
            .network_runtime
            .as_ref()
            .expect("source runtime is available")
            .spawn(async move {
                if start_rx.await.is_err() {
                    return;
                }
                let result = tokio::select! {
                    _ = cancel_rx => return,
                    result = load_source(&mut network, &mut worker_jar, input, max_body_bytes, |event| {
                        let _ = command_tx.send(CoreMessage::SourceLoadProgress(
                            SourceLoadProgress {
                                context_id,
                                navigation_id,
                                event: event.clone(),
                            },
                        ));
                    }) => result,
                };
                let cookie_delta = worker_jar.delta_from_snapshots(&baseline);
                let _ = command_tx.send(CoreMessage::SourceLoaded(SourceLoadCompletion {
                    context_id,
                    navigation_id,
                    result,
                    cookie_delta,
                }));
            });
        self.context_mut(context_id)?
            .active_navigation
            .as_mut()
            .expect("navigation was just installed")
            .load_task = Some(load_task.abort_handle());
        self.pending_load_starts.push(start);
        Ok(BrowserCommandResult::NavigationAccepted { navigation_id })
    }

    fn complete_navigation(&mut self, completion: SourceLoadCompletion) {
        let SourceLoadCompletion {
            context_id,
            navigation_id,
            result,
            cookie_delta,
        } = completion;
        let Ok(context) = self.context(context_id) else {
            return;
        };
        let Some(active) = context.active_navigation.as_ref() else {
            return;
        };
        if active.navigation_id != navigation_id {
            return;
        }
        let frame_id = context.frame_id;
        let request_id = active.request_id;
        let active = self
            .context_mut(context_id)
            .expect("completion context was checked")
            .active_navigation
            .as_mut()
            .expect("completion navigation was checked");
        active.cancel.take();
        active.load_task.take();
        self.cookies.apply_delta(cookie_delta);
        let loaded = match result {
            Ok(loaded) => loaded,
            Err(error) => {
                self.fail_navigation(context_id, navigation_id, Some(request_id), error);
                return;
            }
        };
        self.emit_phase(
            context_id,
            frame_id,
            navigation_id,
            NavigationPhase::Response,
        );
        self.emit_phase(context_id, frame_id, navigation_id, NavigationPhase::Commit);
        let parser = PageParser::with_headers(
            loaded.final_url,
            loaded.html,
            loaded
                .headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
        );
        self.context_mut(context_id)
            .expect("active navigation context exists")
            .active_navigation
            .as_mut()
            .expect("active navigation exists")
            .work = Some(NavigationWork::Parsing(Box::new(parser)));
        self.emit_phase(context_id, frame_id, navigation_id, NavigationPhase::Parse);
        self.pending_navigation_work
            .push_back((context_id, navigation_id));
    }

    fn has_navigation_work(&self) -> bool {
        !self.pending_navigation_work.is_empty()
    }

    fn advance_navigation_work(&mut self) -> bool {
        let Some((context_id, navigation_id)) = self.pending_navigation_work.pop_front() else {
            return false;
        };
        let Some((work, request_id)) = self
            .contexts
            .get_mut(&context_id)
            .and_then(|context| context.active_navigation.as_mut())
            .filter(|active| active.navigation_id == navigation_id)
            .and_then(|active| active.work.take().map(|work| (work, active.request_id)))
        else {
            return true;
        };

        match work {
            NavigationWork::Parsing(mut parser) => match parser.advance(PARSER_WORK_BYTES) {
                Ok(Some(page)) => self.commit_parsed_navigation(context_id, navigation_id, page),
                Ok(None) => self.restore_navigation_work(
                    context_id,
                    navigation_id,
                    NavigationWork::Parsing(parser),
                ),
                Err(error) => self.fail_navigation(
                    context_id,
                    navigation_id,
                    Some(request_id),
                    BrowserError::new(browser_error_codes::NAVIGATION_LOAD, error.to_string()),
                ),
            },
            NavigationWork::Scripts(scripts) => {
                self.advance_script_work(context_id, navigation_id, scripts)
            }
            NavigationWork::Lifecycle { stage, actions } => {
                self.advance_lifecycle(context_id, navigation_id, stage, actions)
            }
        }
        true
    }

    fn restore_navigation_work(
        &mut self,
        context_id: BrowsingContextId,
        navigation_id: NavigationId,
        work: NavigationWork,
    ) {
        let is_current = self
            .contexts
            .get_mut(&context_id)
            .and_then(|context| context.active_navigation.as_mut())
            .filter(|active| active.navigation_id == navigation_id && active.work.is_none());
        if let Some(active) = is_current {
            active.work = Some(work);
            self.pending_navigation_work
                .push_back((context_id, navigation_id));
        }
    }

    fn commit_parsed_navigation(
        &mut self,
        context_id: BrowsingContextId,
        navigation_id: NavigationId,
        mut page: Page,
    ) {
        let Ok(context) = self.context(context_id) else {
            return;
        };
        let Some(active) = context.active_navigation.as_ref() else {
            return;
        };
        if active.navigation_id != navigation_id {
            return;
        }
        let frame_id = context.frame_id;
        let request_id = active.request_id;
        let history_update = active.history_update.clone();
        let final_url = page.url().to_owned();

        let (mut history, history_disposition) = match history_update {
            HistoryUpdate::Push => {
                let history = self
                    .context(context_id)
                    .expect("active navigation context exists")
                    .page
                    .session_history()
                    .clone();
                let disposition = if history.length() == 1 && history.url() == Some("about:blank") {
                    HistoryDisposition::Replace
                } else {
                    HistoryDisposition::Push
                };
                (history, disposition)
            }
            HistoryUpdate::Replace => (
                self.context(context_id)
                    .expect("active navigation context exists")
                    .page
                    .session_history()
                    .clone(),
                HistoryDisposition::Replace,
            ),
            HistoryUpdate::Preserve(history) => {
                let disposition = if history.url() == Some(final_url.as_str()) {
                    HistoryDisposition::Keep
                } else {
                    HistoryDisposition::Replace
                };
                (history, disposition)
            }
        };
        match history_disposition {
            HistoryDisposition::Push => {
                history.push(HistoryEntry::navigation(final_url.clone()));
            }
            HistoryDisposition::Replace => {
                history.replace(HistoryEntry::navigation(final_url.clone()));
            }
            HistoryDisposition::Keep => {}
        }
        page.set_session_history(history);

        let document_id = match self.ids.document() {
            Ok(document_id) => document_id,
            Err(error) => {
                self.fail_navigation(context_id, navigation_id, Some(request_id), error);
                return;
            }
        };
        let runtime_context_id = match self.ids.runtime() {
            Ok(runtime_context_id) => runtime_context_id,
            Err(error) => {
                self.fail_navigation(context_id, navigation_id, Some(request_id), error);
                return;
            }
        };
        let (
            old_document_id,
            old_runtime_context_id,
            old_runtime_slot,
            context_config,
            host_view,
            page_zoom,
        ) = {
            let context = self
                .context(context_id)
                .expect("active navigation context exists");
            (
                context.document_id,
                context.runtime_context_id,
                context.runtime_slot,
                context.config.clone(),
                context.host_view,
                context.page_zoom,
            )
        };
        page.set_layout_viewport(page_layout_viewport(host_view.viewport, page_zoom));
        if self.runtime_slots.len() >= MAX_RUNTIME_SLOTS {
            self.fail_navigation(
                context_id,
                navigation_id,
                Some(request_id),
                BrowserError::new(
                    browser_error_codes::CONTEXT_LIMIT,
                    "browser runtime-generation limit reached",
                ),
            );
            return;
        }
        let mut runtime = match JsRuntime::with_browser_storage(
            self.network_config.clone(),
            Arc::clone(&self.store),
            format!("context-{}", context_id.get()),
            &page,
        ) {
            Ok(runtime) => runtime,
            Err(error) => {
                self.fail_navigation(
                    context_id,
                    navigation_id,
                    Some(request_id),
                    engine_error(error),
                );
                return;
            }
        };
        apply_runtime_config(&mut runtime, &context_config);
        let host_view_source = host_view_runtime_source(&page, host_view);
        if let Err(error) = runtime.with_entered_isolate(|runtime| {
            runtime.evaluate_with_page_mut(&host_view_source, &mut page)
        }) {
            self.fail_navigation(
                context_id,
                navigation_id,
                Some(request_id),
                engine_error(error),
            );
            return;
        }
        if let Err(error) = self.record_visit_url(&final_url) {
            self.fail_navigation(context_id, navigation_id, Some(request_id), error);
            return;
        }
        let runtime_slot = self.runtime_slots.len();
        self.runtime_slots.push(RuntimeSlot {
            runtime,
            active: true,
        });
        self.runtime_slots[old_runtime_slot].active = false;
        let script_work = NavigationScriptWork {
            preload_scripts: context_config.preload_scripts.into(),
            new_document_scripts: context_config.new_document_scripts.into(),
            author_scripts: None,
            bypass_csp: context_config.bypass_csp,
            actions: Vec::new(),
        };
        self.emit(BrowserEvent::RuntimeContextDestroyed {
            context_id,
            frame_id,
            document_id: old_document_id,
            runtime_context_id: old_runtime_context_id,
        });
        self.emit(BrowserEvent::DocumentDiscarded {
            context_id,
            frame_id,
            document_id: old_document_id,
            replaced_by: Some(navigation_id),
        });
        {
            let context = self
                .context_mut(context_id)
                .expect("active navigation context exists");
            context.page = page;
            context.document_id = document_id;
            context.runtime_context_id = runtime_context_id;
            context.runtime_slot = runtime_slot;
            context
                .active_navigation
                .as_mut()
                .expect("active navigation exists")
                .work = Some(NavigationWork::Scripts(script_work));
        }
        self.emit(BrowserEvent::NavigationCommitted {
            context_id,
            frame_id,
            navigation_id,
            request_id: Some(request_id),
            document_id,
            runtime_context_id: Some(runtime_context_id),
            url: final_url,
        });
        self.emit(BrowserEvent::RuntimeContextCreated {
            context_id,
            frame_id,
            document_id,
            runtime_context_id,
        });
        self.emit_phase(
            context_id,
            frame_id,
            navigation_id,
            NavigationPhase::ScriptsAndSubresources,
        );
        self.pending_navigation_work
            .push_back((context_id, navigation_id));
    }

    fn advance_script_work(
        &mut self,
        context_id: BrowsingContextId,
        navigation_id: NavigationId,
        mut work: NavigationScriptWork,
    ) {
        let Ok(context) = self.context(context_id) else {
            return;
        };
        let Some(active) = context.active_navigation.as_ref() else {
            return;
        };
        if active.navigation_id != navigation_id {
            return;
        }
        let document_id = context.document_id;
        let runtime_context_id = context.runtime_context_id;
        let runtime_slot = context.runtime_slot;
        let frame_id = context.frame_id;
        let document_url = context.page.url().to_owned();
        let host_source = work
            .preload_scripts
            .pop_front()
            .or_else(|| work.new_document_scripts.pop_front());

        let step = if let Some(source) = host_source {
            NavigationScriptStep::Host(source)
        } else {
            let context = self.context(context_id).expect("context checked");
            let author_scripts = work
                .author_scripts
                .get_or_insert_with(|| PageScriptRunner::new(&context.page, work.bypass_csp));
            match author_scripts.prepare_next(&context.page) {
                Some(item) => NavigationScriptStep::Author(item),
                None => NavigationScriptStep::Complete,
            }
        };

        let step = match step {
            NavigationScriptStep::Author(PreparedPageScript::External(request)) => {
                let mut baseline = self.cookies.snapshots();
                match self.runtime_slots[runtime_slot]
                    .runtime
                    .network_cookie_snapshots()
                {
                    Ok(snapshots) => baseline.extend(snapshots),
                    Err(error) => {
                        let effects = RuntimeEffects {
                            exceptions: vec![RuntimeExceptionEvent {
                                error: script_error(error),
                            }],
                            ..RuntimeEffects::default()
                        };
                        self.emit(BrowserEvent::RuntimeEffects {
                            context_id,
                            frame_id,
                            document_id,
                            runtime_context_id,
                            url: document_url.clone(),
                            effects,
                        });
                        self.restore_navigation_work(
                            context_id,
                            navigation_id,
                            NavigationWork::Scripts(work),
                        );
                        return;
                    }
                }
                let request_id = match self.ids.request() {
                    Ok(request_id) => request_id,
                    Err(error) => {
                        let effects = RuntimeEffects {
                            exceptions: vec![RuntimeExceptionEvent { error }],
                            ..RuntimeEffects::default()
                        };
                        self.emit(BrowserEvent::RuntimeEffects {
                            context_id,
                            frame_id,
                            document_id,
                            runtime_context_id,
                            url: document_url.clone(),
                            effects,
                        });
                        self.restore_navigation_work(
                            context_id,
                            navigation_id,
                            NavigationWork::Scripts(work),
                        );
                        return;
                    }
                };
                self.start_external_script_load(
                    ExternalScriptLoadKey {
                        context_id,
                        navigation_id,
                        document_id,
                        runtime_context_id,
                        request_id,
                    },
                    request,
                    work,
                    baseline,
                );
                return;
            }
            step => step,
        };

        let (item_result, effects_result, actions_result) = {
            let (contexts, runtime_slots) = (&mut self.contexts, &mut self.runtime_slots);
            let context = contexts.get_mut(&context_id).expect("context checked");
            runtime_slots[runtime_slot]
                .runtime
                .with_entered_isolate(|runtime| {
                    let item_result = match step {
                        NavigationScriptStep::Host(source)
                        | NavigationScriptStep::Author(PreparedPageScript::Inline(source)) => Some(
                            runtime
                                .evaluate_with_page_mut(&source, &mut context.page)
                                .map(|_| ()),
                        ),
                        NavigationScriptStep::Author(PreparedPageScript::Skip) => Some(Ok(())),
                        NavigationScriptStep::Complete => None,
                        NavigationScriptStep::Author(PreparedPageScript::External(_)) => {
                            unreachable!("external scripts start before entering the isolate")
                        }
                    };
                    let effects = drain_runtime_effects(runtime);
                    let actions = runtime.drain_navigation_actions();
                    (item_result, effects, actions)
                })
        };

        let mut effects = RuntimeEffects::default();
        let advanced_item = item_result.is_some();
        if let Some(Err(error)) = item_result {
            effects.exceptions.push(RuntimeExceptionEvent {
                error: script_error(error),
            });
        }
        match effects_result {
            Ok(item_effects) => effects.extend(item_effects),
            Err(error) => effects.exceptions.push(RuntimeExceptionEvent {
                error: script_error(error),
            }),
        }
        match actions_result {
            Ok(actions) => append_navigation_actions_bounded(&mut work.actions, actions),
            Err(error) => effects.exceptions.push(RuntimeExceptionEvent {
                error: script_error(error),
            }),
        }

        let is_current = self
            .context(context_id)
            .ok()
            .and_then(|context| context.active_navigation.as_ref())
            .is_some_and(|active| active.navigation_id == navigation_id);
        if !is_current {
            return;
        }
        if !effects.is_empty() {
            self.emit(BrowserEvent::RuntimeEffects {
                context_id,
                frame_id,
                document_id,
                runtime_context_id,
                url: document_url,
                effects,
            });
        }

        let next = if advanced_item {
            NavigationWork::Scripts(work)
        } else {
            NavigationWork::Lifecycle {
                stage: LifecycleStage::DomContentLoaded,
                actions: work.actions,
            }
        };
        self.restore_navigation_work(context_id, navigation_id, next);
    }

    fn start_external_script_load(
        &mut self,
        key: ExternalScriptLoadKey,
        request: ExternalPageScript,
        work: NavigationScriptWork,
        baseline: Vec<vixen_net::CookieSnapshot>,
    ) {
        let mut worker_jar = CookieJar::from_snapshots(baseline.clone());
        let mut network = self.network.clone();
        let max_body_bytes = self.network_config.max_body_bytes;
        let max_redirects = self.network_config.max_redirects;
        let worker_request = request.clone();
        let store = Arc::clone(&self.store);
        let command_tx = self.command_tx.clone();
        let (cancel, cancel_rx) = tokio::sync::oneshot::channel();

        {
            let active = self
                .context_mut(key.context_id)
                .expect("external script context was checked")
                .active_navigation
                .as_mut()
                .expect("external script navigation was checked");
            active.cancel = Some(cancel);
            active.pending_script = Some(PendingExternalScript { key, request, work });
        }

        let task = self
            .network_runtime
            .as_ref()
            .expect("source runtime is available")
            .spawn(async move {
                let mut profile_baseline = baseline.clone();
                let result = tokio::select! {
                    _ = cancel_rx => return,
                    result = load_external_script(
                        &mut network,
                        &mut worker_jar,
                        ExternalScriptLoadInput {
                            store: &store,
                            profile_baseline: &mut profile_baseline,
                            request: worker_request,
                            max_body_bytes,
                            max_redirects,
                        },
                    ) => result,
                };
                let cookie_delta = worker_jar.delta_from_snapshots(&profile_baseline);
                let _ = command_tx.send(CoreMessage::ExternalScriptLoaded(
                    ExternalScriptLoadCompletion {
                        key,
                        result,
                        cookie_delta,
                    },
                ));
            });
        self.context_mut(key.context_id)
            .expect("external script context was checked")
            .active_navigation
            .as_mut()
            .expect("external script navigation was checked")
            .load_task = Some(task.abort_handle());
    }

    fn complete_external_script(&mut self, completion: ExternalScriptLoadCompletion) {
        let ExternalScriptLoadCompletion {
            key,
            result,
            cookie_delta,
        } = completion;
        let Some(context) = self.contexts.get(&key.context_id) else {
            return;
        };
        if context.document_id != key.document_id
            || context.runtime_context_id != key.runtime_context_id
        {
            return;
        }
        let runtime_slot = context.runtime_slot;
        let frame_id = context.frame_id;
        let document_url = context.page.url().to_owned();
        let Some(active) = context.active_navigation.as_ref() else {
            return;
        };
        if active.navigation_id != key.navigation_id
            || active
                .pending_script
                .as_ref()
                .is_none_or(|pending| pending.key != key)
        {
            return;
        }

        let mut pending = {
            let active = self
                .context_mut(key.context_id)
                .expect("external script context was checked")
                .active_navigation
                .as_mut()
                .expect("external script navigation was checked");
            active.cancel.take();
            active.load_task.take();
            active
                .pending_script
                .take()
                .expect("external script request was checked")
        };
        let request_url = pending.request.url().to_string();
        let mut effects = RuntimeEffects {
            network: external_script_network_events(key.request_id, &request_url, &result),
            ..RuntimeEffects::default()
        };
        let (source, blocked, requested_urls) = match result {
            Ok(LoadedExternalScript::File { final_url, source }) => {
                if pending.request.allows_url(&final_url) {
                    (Some(source), None, Vec::new())
                } else {
                    (None, Some((final_url.to_string(), "csp")), Vec::new())
                }
            }
            Ok(LoadedExternalScript::Http {
                response,
                requested_urls,
            }) => {
                let final_url = url::Url::parse(&response.final_url).ok();
                if final_url
                    .as_ref()
                    .is_none_or(|final_url| !pending.request.allows_url(final_url))
                {
                    (None, Some((response.final_url, "csp")), requested_urls)
                } else if !script_response_allowed(&response) {
                    (
                        None,
                        Some((response.final_url, "response-policy")),
                        requested_urls,
                    )
                } else {
                    (Some(response.body), None, requested_urls)
                }
            }
            Err(_) => (None, None, Vec::new()),
        };
        if let Some((url, blocked_reason)) = blocked {
            effects.network.push(RuntimeNetworkEvent::Failure {
                request_id: key.request_id.to_string(),
                url,
                error_text: "external script blocked".to_owned(),
                blocked_reason: Some(blocked_reason.to_owned()),
            });
        }

        let Some(source) = source else {
            if !effects.is_empty() {
                self.emit(BrowserEvent::RuntimeEffects {
                    context_id: key.context_id,
                    frame_id,
                    document_id: key.document_id,
                    runtime_context_id: key.runtime_context_id,
                    url: document_url.clone(),
                    effects,
                });
            }
            self.restore_navigation_work(
                key.context_id,
                key.navigation_id,
                NavigationWork::Scripts(pending.work),
            );
            return;
        };
        if let Err(error) = persist_profile_cookies(&self.store, &requested_urls, &cookie_delta) {
            effects.exceptions.push(RuntimeExceptionEvent {
                error: script_error(error),
            });
            self.emit(BrowserEvent::RuntimeEffects {
                context_id: key.context_id,
                frame_id,
                document_id: key.document_id,
                runtime_context_id: key.runtime_context_id,
                url: document_url.clone(),
                effects,
            });
            self.restore_navigation_work(
                key.context_id,
                key.navigation_id,
                NavigationWork::Scripts(pending.work),
            );
            return;
        }
        if let Err(error) = self.runtime_slots[runtime_slot]
            .runtime
            .apply_network_cookie_delta(cookie_delta.clone())
        {
            effects.exceptions.push(RuntimeExceptionEvent {
                error: script_error(error),
            });
            self.emit(BrowserEvent::RuntimeEffects {
                context_id: key.context_id,
                frame_id,
                document_id: key.document_id,
                runtime_context_id: key.runtime_context_id,
                url: document_url.clone(),
                effects,
            });
            self.restore_navigation_work(
                key.context_id,
                key.navigation_id,
                NavigationWork::Scripts(pending.work),
            );
            return;
        }
        self.cookies.apply_delta(cookie_delta);

        let (item_result, effects_result, actions_result) = {
            let (contexts, runtime_slots) = (&mut self.contexts, &mut self.runtime_slots);
            let context = contexts
                .get_mut(&key.context_id)
                .expect("external script context was checked");
            runtime_slots[runtime_slot]
                .runtime
                .with_entered_isolate(|runtime| {
                    let item_result = runtime
                        .evaluate_with_page_mut(&source, &mut context.page)
                        .map(|_| ());
                    let effects = drain_runtime_effects(runtime);
                    let actions = runtime.drain_navigation_actions();
                    (item_result, effects, actions)
                })
        };

        if let Err(error) = item_result {
            effects.exceptions.push(RuntimeExceptionEvent {
                error: script_error(error),
            });
        }
        match effects_result {
            Ok(item_effects) => effects.extend(item_effects),
            Err(error) => effects.exceptions.push(RuntimeExceptionEvent {
                error: script_error(error),
            }),
        }
        match actions_result {
            Ok(actions) => append_navigation_actions_bounded(&mut pending.work.actions, actions),
            Err(error) => effects.exceptions.push(RuntimeExceptionEvent {
                error: script_error(error),
            }),
        }

        let is_current = self.contexts.get(&key.context_id).is_some_and(|context| {
            context.document_id == key.document_id
                && context.runtime_context_id == key.runtime_context_id
                && context
                    .active_navigation
                    .as_ref()
                    .is_some_and(|active| active.navigation_id == key.navigation_id)
        });
        if !is_current {
            return;
        }
        if !effects.is_empty() {
            self.emit(BrowserEvent::RuntimeEffects {
                context_id: key.context_id,
                frame_id,
                document_id: key.document_id,
                runtime_context_id: key.runtime_context_id,
                url: document_url,
                effects,
            });
        }
        self.restore_navigation_work(
            key.context_id,
            key.navigation_id,
            NavigationWork::Scripts(pending.work),
        );
    }

    fn advance_lifecycle(
        &mut self,
        context_id: BrowsingContextId,
        navigation_id: NavigationId,
        stage: LifecycleStage,
        actions: Vec<JsNavigationAction>,
    ) {
        let Ok(context) = self.context(context_id) else {
            return;
        };
        let Some(active) = context.active_navigation.as_ref() else {
            return;
        };
        if active.navigation_id != navigation_id {
            return;
        }
        let frame_id = context.frame_id;
        let document_id = context.document_id;

        match stage {
            LifecycleStage::DomContentLoaded => {
                self.emit_phase(
                    context_id,
                    frame_id,
                    navigation_id,
                    NavigationPhase::DomContentLoaded,
                );
                self.emit(BrowserEvent::DomContentLoaded {
                    context_id,
                    frame_id,
                    navigation_id,
                    document_id,
                });
                self.restore_navigation_work(
                    context_id,
                    navigation_id,
                    NavigationWork::Lifecycle {
                        stage: LifecycleStage::Load,
                        actions,
                    },
                );
            }
            LifecycleStage::Load => {
                self.emit_phase(context_id, frame_id, navigation_id, NavigationPhase::Load);
                self.emit(BrowserEvent::DocumentLoadCompleted {
                    context_id,
                    frame_id,
                    navigation_id,
                    document_id,
                });
                self.restore_navigation_work(
                    context_id,
                    navigation_id,
                    NavigationWork::Lifecycle {
                        stage: LifecycleStage::Settle,
                        actions,
                    },
                );
            }
            LifecycleStage::Settle => {
                if matches!(
                    self.finish_navigation(
                        context_id,
                        navigation_id,
                        NavigationTerminal::Settled,
                        true,
                    ),
                    Ok(true)
                ) {
                    let _ = self.apply_navigation_actions(context_id, actions, Some(navigation_id));
                }
            }
        }
    }

    fn progress_navigation(&mut self, progress: SourceLoadProgress) {
        let SourceLoadProgress {
            context_id,
            navigation_id,
            event,
        } = progress;
        let Ok(context) = self.context(context_id) else {
            return;
        };
        let Some(active) = context.active_navigation.as_ref() else {
            return;
        };
        if active.navigation_id != navigation_id {
            return;
        }

        // NavigationStarted and the Response phase already expose one request
        // and response boundary. Network RequestStart/Response progress is
        // intentionally ignored here rather than duplicating those events.
        let NetworkEvent::Redirect { from, to, status } = event else {
            return;
        };
        let frame_id = context.frame_id;
        let request_id = active.request_id;
        let next_request_id = match self.ids.request() {
            Ok(request_id) => request_id,
            Err(error) => {
                self.fail_navigation(context_id, navigation_id, Some(request_id), error);
                return;
            }
        };
        self.context_mut(context_id)
            .expect("progress context was checked")
            .active_navigation
            .as_mut()
            .expect("progress navigation was checked")
            .request_id = next_request_id;
        self.emit(BrowserEvent::NavigationRedirected {
            context_id,
            frame_id,
            navigation_id,
            request_id,
            next_request_id,
            from_url: from,
            to_url: to,
            status,
        });
    }

    fn stop(
        &mut self,
        context_id: BrowsingContextId,
    ) -> Result<BrowserCommandResult, BrowserError> {
        self.cancel_active_navigation(context_id, NavigationCancellationReason::Stopped, true)?;
        Ok(BrowserCommandResult::Accepted)
    }

    fn cancel_active_navigation(
        &mut self,
        context_id: BrowsingContextId,
        reason: NavigationCancellationReason,
        emit_state: bool,
    ) -> Result<bool, BrowserError> {
        let Some(navigation_id) = self
            .context(context_id)?
            .active_navigation
            .as_ref()
            .map(|navigation| navigation.navigation_id)
        else {
            return Ok(false);
        };
        self.finish_navigation(
            context_id,
            navigation_id,
            NavigationTerminal::Cancelled { reason },
            emit_state,
        )?;
        Ok(true)
    }

    fn configure_context(
        &mut self,
        context_id: BrowsingContextId,
        config: BrowsingContextConfig,
    ) -> Result<BrowserCommandResult, BrowserError> {
        self.ensure_context(context_id)?;
        let runtime_slot = self.context(context_id)?.runtime_slot;
        apply_runtime_config(&mut self.runtime_slots[runtime_slot].runtime, &config);
        let preload_scripts = config.preload_scripts.clone();
        {
            let (contexts, runtime_slots) = (&mut self.contexts, &mut self.runtime_slots);
            let context = contexts.get_mut(&context_id).expect("context checked");
            for source in preload_scripts {
                runtime_slots[runtime_slot]
                    .runtime
                    .with_entered_isolate(|runtime| {
                        runtime.evaluate_with_page_mut(&source, &mut context.page)
                    })
                    .map_err(engine_error)?;
            }
            context.config = config;
        }
        Ok(BrowserCommandResult::Accepted)
    }

    fn update_host_view_state(
        &mut self,
        context_id: BrowsingContextId,
        state: HostViewState,
    ) -> Result<BrowserCommandResult, BrowserError> {
        validate_viewport(state.viewport)?;
        if state.generation == 0
            || !state.scale_factor.is_finite()
            || !(0.1..=16.0).contains(&state.scale_factor)
            || (state.focused && !matches!(state.lifecycle, vixen_api::HostLifecycle::Resumed))
            || (state.visible
                && matches!(
                    state.lifecycle,
                    vixen_api::HostLifecycle::Hidden
                        | vixen_api::HostLifecycle::Paused
                        | vixen_api::HostLifecycle::Detached
                ))
        {
            return Err(BrowserError::new(
                browser_error_codes::INVALID_ARGUMENT,
                "host view generation and scale factor must be valid",
            ));
        }
        let (document_id, runtime_context_id, current_generation) = {
            let context = self.context(context_id)?;
            (
                context.document_id,
                context.runtime_context_id,
                context.host_view.generation,
            )
        };
        if state.generation <= current_generation {
            return Err(BrowserError::new(
                browser_error_codes::STALE_HOST_VIEW,
                format!(
                    "host view generation {} is not newer than {current_generation}",
                    state.generation
                ),
            ));
        }
        let source = {
            let context = self.context_mut(context_id)?;
            context.host_view = state;
            context
                .page
                .set_layout_viewport(page_layout_viewport(state.viewport, context.page_zoom));
            host_view_runtime_source(&context.page, state)
        };
        let evaluation =
            self.automation_evaluation(context_id, document_id, runtime_context_id, source)?;
        Ok(BrowserCommandResult::InputDispatched(InputDispatchResult {
            effects: evaluation.effects,
            navigation_actions: evaluation.navigation_actions,
        }))
    }

    fn ensure_host_accepts_input(&self, context_id: BrowsingContextId) -> Result<(), BrowserError> {
        if self.context(context_id)?.host_view.accepts_input() {
            return Ok(());
        }
        Err(BrowserError::new(
            browser_error_codes::INVALID_ARGUMENT,
            "host view is not active for content input",
        ))
    }

    fn evaluate(
        &mut self,
        context_id: BrowsingContextId,
        document_id: DocumentId,
        runtime_context_id: RuntimeContextId,
        source: String,
    ) -> Result<BrowserCommandResult, BrowserError> {
        let evaluation =
            self.automation_evaluation(context_id, document_id, runtime_context_id, source)?;
        Ok(BrowserCommandResult::Evaluation(EvaluationResult {
            value: evaluation.value,
            navigation_actions: evaluation.navigation_actions,
        }))
    }

    fn evaluate_for_automation(
        &mut self,
        context_id: BrowsingContextId,
        document_id: DocumentId,
        runtime_context_id: RuntimeContextId,
        source: String,
    ) -> Result<BrowserCommandResult, BrowserError> {
        let evaluation =
            self.automation_evaluation(context_id, document_id, runtime_context_id, source)?;
        Ok(BrowserCommandResult::AutomationEvaluation(evaluation))
    }

    fn automation_evaluation(
        &mut self,
        context_id: BrowsingContextId,
        document_id: DocumentId,
        runtime_context_id: RuntimeContextId,
        source: String,
    ) -> Result<AutomationEvaluation, BrowserError> {
        if source.len() > MAX_SCRIPT_BYTES {
            return Err(BrowserError::new(
                browser_error_codes::INVALID_ARGUMENT,
                "script source exceeds the browser evaluation limit",
            ));
        }
        self.ensure_context(context_id)?;
        let context = self.context(context_id)?;
        if context.document_id != document_id {
            return Err(BrowserError::new(
                browser_error_codes::STALE_DOCUMENT,
                format!("document {document_id} is no longer active in context {context_id}"),
            ));
        }
        if context.runtime_context_id != runtime_context_id {
            return Err(BrowserError::new(
                browser_error_codes::STALE_RUNTIME,
                format!("runtime {runtime_context_id} is no longer active in context {context_id}"),
            ));
        }
        let runtime_slot = context.runtime_slot;
        let (contexts, runtime_slots) = (&mut self.contexts, &mut self.runtime_slots);
        let context = contexts.get_mut(&context_id).expect("context checked");
        let evaluation = runtime_slots[runtime_slot]
            .runtime
            .with_entered_isolate(|runtime| {
                let value = match runtime.evaluate_with_page_mut(&source, &mut context.page) {
                    Ok(value) => value,
                    Err(error) => {
                        discard_runtime_outputs(runtime);
                        return Err(error);
                    }
                };
                let effects = drain_runtime_effects(runtime);
                let actions = runtime.drain_navigation_actions();
                match (effects, actions) {
                    (Ok(effects), Ok(actions)) => Ok((value, effects, actions)),
                    (Err(error), _) | (_, Err(error)) => {
                        discard_runtime_outputs(runtime);
                        Err(error)
                    }
                }
            });
        let (value, effects, actions) = evaluation.map_err(script_error)?;
        let state = context_state(context_id, context);
        self.emit(BrowserEvent::BrowsingContextStateChanged { state });
        let navigation_actions = self.apply_navigation_actions(context_id, actions, None)?;
        Ok(AutomationEvaluation {
            value: script_value(value),
            effects,
            navigation_actions,
        })
    }

    fn dispatch_mouse_event(
        &mut self,
        context_id: BrowsingContextId,
        document_id: DocumentId,
        runtime_context_id: RuntimeContextId,
        node_id: usize,
        event_type: String,
        event: MouseEventData,
    ) -> Result<BrowserCommandResult, BrowserError> {
        self.ensure_host_accepts_input(context_id)?;
        if !event.x.is_finite()
            || !event.y.is_finite()
            || !event.delta_x.is_finite()
            || !event.delta_y.is_finite()
        {
            return Err(BrowserError::new(
                browser_error_codes::INVALID_ARGUMENT,
                "mouse event coordinates and deltas must be finite",
            ));
        }
        let page_zoom = self
            .context_for_document(context_id, document_id)?
            .page_zoom;
        let event = MouseEventData {
            x: event.x / page_zoom,
            y: event.y / page_zoom,
            delta_x: event.delta_x / page_zoom,
            delta_y: event.delta_y / page_zoom,
            ..event
        };
        let is_wheel = event_type == "wheel";
        let node_id = if is_wheel && node_id == 0 {
            self.context_for_document(context_id, document_id)?
                .page
                .default_pointer_event_target_node_id()
                .unwrap_or(0)
        } else {
            node_id
        };
        if node_id == 0 {
            return Ok(BrowserCommandResult::InputDispatched(InputDispatchResult {
                effects: RuntimeEffects::default(),
                navigation_actions: Vec::new(),
            }));
        }
        let event_type = json_string(&event_type)?;
        let init = deno_core::serde_json::json!({
            "bubbles": event.bubbles,
            "clientX": event.x,
            "clientY": event.y,
            "screenX": event.x,
            "screenY": event.y,
            "button": event.button,
            "buttons": event.buttons,
            "detail": event.detail,
            "relatedNodeId": event.related_node_id,
            "ctrlKey": event.ctrl_key,
            "shiftKey": event.shift_key,
            "altKey": event.alt_key,
            "metaKey": event.meta_key,
            "deltaX": event.delta_x,
            "deltaY": event.delta_y,
            "deltaZ": 0,
            "deltaMode": 0,
        });
        let source = format!(
            "globalThis.__vixenDispatchMouseEvent ? globalThis.__vixenDispatchMouseEvent({node_id}, {event_type}, {init}) : false"
        );
        let evaluation =
            self.automation_evaluation(context_id, document_id, runtime_context_id, source)?;
        if is_wheel
            && !event.ctrl_key
            && !event.meta_key
            && evaluation.value == ScriptValue::Bool(true)
        {
            let viewport =
                page_layout_viewport(self.context(context_id)?.host_view.viewport, page_zoom);
            self.context_mut(context_id)?
                .page
                .scroll_root_by(viewport, (event.delta_x, event.delta_y));
        }
        Ok(BrowserCommandResult::InputDispatched(InputDispatchResult {
            effects: evaluation.effects,
            navigation_actions: evaluation.navigation_actions,
        }))
    }

    fn dispatch_key_event(
        &mut self,
        context_id: BrowsingContextId,
        document_id: DocumentId,
        runtime_context_id: RuntimeContextId,
        event_type: String,
        event: KeyEventData,
    ) -> Result<BrowserCommandResult, BrowserError> {
        self.ensure_host_accepts_input(context_id)?;
        let default_scroll = keyboard_scroll_delta(
            &event_type,
            &event,
            self.context_for_document(context_id, document_id)?,
        );
        let event_type = json_string(&event_type)?;
        let init = deno_core::serde_json::json!({
            "key": event.key,
            "code": event.code,
            "location": event.location,
            "ctrlKey": event.ctrl_key,
            "shiftKey": event.shift_key,
            "altKey": event.alt_key,
            "metaKey": event.meta_key,
            "repeat": event.repeat,
            "isComposing": false,
            "text": event.text,
            "inputText": event.text,
            "applyText": event.apply_text,
        });
        let source = format!(
            "globalThis.__vixenDispatchKeyEvent ? globalThis.__vixenDispatchKeyEvent({event_type}, {init}) : false"
        );
        let evaluation =
            self.automation_evaluation(context_id, document_id, runtime_context_id, source)?;
        if evaluation.value == ScriptValue::Bool(true)
            && let Some(delta) = default_scroll
        {
            let context = self.context_mut(context_id)?;
            let viewport = page_layout_viewport(context.host_view.viewport, context.page_zoom);
            context.page.scroll_root_by(viewport, delta);
        }
        Ok(BrowserCommandResult::InputDispatched(InputDispatchResult {
            effects: evaluation.effects,
            navigation_actions: evaluation.navigation_actions,
        }))
    }

    fn dispatch_text_input(
        &mut self,
        context_id: BrowsingContextId,
        document_id: DocumentId,
        runtime_context_id: RuntimeContextId,
        state: TextInputState,
    ) -> Result<BrowserCommandResult, BrowserError> {
        self.ensure_host_accepts_input(context_id)?;
        validate_text_input_state(&state)?;
        let source = format!(
            "globalThis.__vixenApplyTextInputState ? globalThis.__vixenApplyTextInputState({}) : false",
            deno_core::serde_json::json!({
                "text": state.text,
                "selectionBase": state.selection.base_offset,
                "selectionExtent": state.selection.extent_offset,
                "composingBase": state.composing.map(|range| range.base_offset),
                "composingExtent": state.composing.map(|range| range.extent_offset),
            })
        );
        let evaluation =
            self.automation_evaluation(context_id, document_id, runtime_context_id, source)?;
        if evaluation.value != ScriptValue::Bool(true) {
            return Err(BrowserError::new(
                browser_error_codes::INVALID_ARGUMENT,
                "text input requires a focused writable native text control",
            ));
        }
        Ok(BrowserCommandResult::InputDispatched(InputDispatchResult {
            effects: evaluation.effects,
            navigation_actions: evaluation.navigation_actions,
        }))
    }

    fn dispatch_accessibility_action(
        &mut self,
        request: AccessibilityActionDispatch,
    ) -> Result<BrowserCommandResult, BrowserError> {
        let AccessibilityActionDispatch {
            context_id,
            document_id,
            runtime_context_id,
            viewport,
            source_generation,
            node_id,
            action,
        } = request;
        self.ensure_host_accepts_input(context_id)?;
        validate_viewport(viewport)?;
        let context = self.context_for_document(context_id, document_id)?;
        if context.runtime_context_id != runtime_context_id {
            return Err(BrowserError::new(
                browser_error_codes::STALE_RUNTIME,
                format!("runtime {runtime_context_id} is no longer active in context {context_id}"),
            ));
        }
        let snapshot = self.accessibility_snapshot(context_id, document_id, viewport)?;
        if snapshot.source_generation != source_generation {
            return Err(BrowserError::new(
                browser_error_codes::STALE_ACCESSIBILITY,
                "accessibility source generation is no longer current",
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
        let value = match &action {
            AccessibilityAction::Focus => "null".to_owned(),
            AccessibilityAction::SetValue(value) => {
                if value.len() > ACCESSIBILITY_MAX_VALUE_BYTES {
                    return Err(BrowserError::new(
                        browser_error_codes::INVALID_ARGUMENT,
                        "accessibility value exceeds the browser limit",
                    ));
                }
                json_string(value)?
            }
            AccessibilityAction::Increase | AccessibilityAction::Decrease => "null".to_owned(),
        };
        let action = json_string(action.as_str())?;
        let source = format!(
            "globalThis.__vixenDispatchAccessibilityAction ? globalThis.__vixenDispatchAccessibilityAction({node_id}, {action}, {value}) : false"
        );
        let evaluation =
            self.automation_evaluation(context_id, document_id, runtime_context_id, source)?;
        Ok(BrowserCommandResult::InputDispatched(InputDispatchResult {
            effects: evaluation.effects,
            navigation_actions: evaluation.navigation_actions,
        }))
    }

    fn apply_navigation_actions(
        &mut self,
        context_id: BrowsingContextId,
        actions: Vec<JsNavigationAction>,
        mut predecessor_navigation_id: Option<NavigationId>,
    ) -> Result<Vec<NavigationActionOutcome>, BrowserError> {
        if actions.len() > MAX_NAVIGATION_ACTIONS_PER_COMMAND
            || actions
                .iter()
                .any(|action| matches!(action, JsNavigationAction::Overflow))
        {
            return Err(BrowserError::new(
                browser_error_codes::INVALID_ARGUMENT,
                format!(
                    "command produced more than {MAX_NAVIGATION_ACTIONS_PER_COMMAND} navigation actions"
                ),
            ));
        }
        let prepared = self.prepare_navigation_actions(context_id, actions)?;
        let mut outcomes = Vec::with_capacity(prepared.len());
        for action in prepared {
            match action {
                PreparedNavigationAction::CrossDocument {
                    url,
                    injected_html,
                    history_update,
                    kind,
                } => {
                    let result = self.begin_navigation(
                        context_id,
                        url,
                        injected_html,
                        history_update,
                        kind,
                        predecessor_navigation_id,
                    )?;
                    let navigation_id = navigation_id_from_result(result)?;
                    outcomes.push(NavigationActionOutcome::CrossDocument {
                        navigation_id,
                        kind,
                    });
                    predecessor_navigation_id = Some(navigation_id);
                }
                PreparedNavigationAction::SameDocument { history, url } => {
                    let runtime_slot = self.context(context_id)?.runtime_slot;
                    self.context_mut(context_id)?
                        .page
                        .set_session_history(history);
                    let (contexts, slots) = (&self.contexts, &mut self.runtime_slots);
                    let page = &contexts.get(&context_id).expect("context checked").page;
                    slots[runtime_slot].runtime.sync_page_realm_key(page);
                    self.emit(BrowserEvent::BrowsingContextStateChanged {
                        state: self.context_state(context_id)?,
                    });
                    outcomes.push(NavigationActionOutcome::SameDocument { url });
                }
            }
        }
        Ok(outcomes)
    }

    fn prepare_navigation_actions(
        &self,
        context_id: BrowsingContextId,
        actions: Vec<JsNavigationAction>,
    ) -> Result<Vec<PreparedNavigationAction>, BrowserError> {
        let context = self.context(context_id)?;
        let mut simulated_history = context.page.session_history().clone();
        let mut prepared = Vec::with_capacity(actions.len());
        for action in actions {
            match action {
                JsNavigationAction::Navigate { url, replace } => {
                    validate_navigation_url(&url)?;
                    prepared.push(PreparedNavigationAction::CrossDocument {
                        url,
                        injected_html: None,
                        history_update: if replace {
                            HistoryUpdate::Replace
                        } else {
                            HistoryUpdate::Push
                        },
                        kind: CrossDocumentNavigationKind::Regular,
                    });
                }
                JsNavigationAction::SetContent { html } => {
                    if html.len() > self.network_config.max_body_bytes as usize {
                        return Err(BrowserError::new(
                            browser_error_codes::INVALID_ARGUMENT,
                            "injected document exceeds the browser body limit",
                        ));
                    }
                    prepared.push(PreparedNavigationAction::CrossDocument {
                        url: simulated_history
                            .url()
                            .unwrap_or_else(|| context.page.url())
                            .to_owned(),
                        injected_html: Some(html),
                        history_update: HistoryUpdate::Preserve(simulated_history.clone()),
                        kind: CrossDocumentNavigationKind::ContentReplacement {
                            replaced_document_id: context.document_id,
                        },
                    });
                }
                JsNavigationAction::FormSubmit {
                    form_id,
                    form_node_id,
                    submitter_node_id,
                    action,
                    method,
                    ..
                } => {
                    let submission = if form_node_id != 0 {
                        match context
                            .page
                            .form_submission_by_node_id(form_node_id, submitter_node_id)
                        {
                            Ok(submission) => Some(submission),
                            Err(_) if !form_id.is_empty() => {
                                let mut submission =
                                    context.page.form_submission(&form_id).map_err(|message| {
                                        BrowserError::new(
                                            browser_error_codes::INVALID_ARGUMENT,
                                            message,
                                        )
                                    })?;
                                submission.action = context
                                    .page
                                    .resolve_url(&action)
                                    .unwrap_or_else(|| action.clone());
                                submission.method = method.clone();
                                Some(submission)
                            }
                            Err(message) => {
                                return Err(BrowserError::new(
                                    browser_error_codes::INVALID_ARGUMENT,
                                    message,
                                ));
                            }
                        }
                    } else if !form_id.is_empty() {
                        Some(context.page.form_submission(&form_id).map_err(|message| {
                            BrowserError::new(browser_error_codes::INVALID_ARGUMENT, message)
                        })?)
                    } else {
                        None
                    };
                    let target = submission
                        .as_ref()
                        .map(|submission| submission.action.clone())
                        .unwrap_or_else(|| {
                            context
                                .page
                                .resolve_url(&action)
                                .unwrap_or_else(|| action.clone())
                        });
                    let method = submission
                        .as_ref()
                        .map(|submission| submission.method.as_str())
                        .unwrap_or(&method)
                        .to_ascii_lowercase();
                    if method != "dialog" {
                        let target = if method == "post" {
                            target
                        } else if let Some(submission) = submission {
                            append_form_query(&target, &submission.body)?
                        } else {
                            target
                        };
                        validate_navigation_url(&target)?;
                        prepared.push(PreparedNavigationAction::CrossDocument {
                            url: target,
                            injected_html: None,
                            history_update: HistoryUpdate::Push,
                            kind: CrossDocumentNavigationKind::Regular,
                        });
                    }
                }
                JsNavigationAction::HistoryPush {
                    url,
                    state_json,
                    title,
                } => {
                    prepare_history_state(&mut simulated_history, url, state_json, title, false)?;
                    prepared.push(PreparedNavigationAction::SameDocument {
                        url: simulated_history.url().unwrap_or_default().to_owned(),
                        history: simulated_history.clone(),
                    });
                }
                JsNavigationAction::HistoryReplace {
                    url,
                    state_json,
                    title,
                } => {
                    prepare_history_state(&mut simulated_history, url, state_json, title, true)?;
                    prepared.push(PreparedNavigationAction::SameDocument {
                        url: simulated_history.url().unwrap_or_default().to_owned(),
                        history: simulated_history.clone(),
                    });
                }
                JsNavigationAction::HistoryTraverse { delta } => {
                    let mut history = simulated_history.clone();
                    let previous_index = history.index();
                    let Some(entry) = history.go(delta).cloned() else {
                        continue;
                    };
                    if entry.state.is_some() {
                        if history.index() != previous_index {
                            let url = entry.url;
                            simulated_history = history.clone();
                            prepared.push(PreparedNavigationAction::SameDocument { history, url });
                        }
                    } else {
                        validate_navigation_url(&entry.url)?;
                        prepared.push(PreparedNavigationAction::CrossDocument {
                            url: entry.url,
                            injected_html: None,
                            history_update: HistoryUpdate::Preserve(history),
                            kind: CrossDocumentNavigationKind::Regular,
                        });
                    }
                }
                JsNavigationAction::Overflow => unreachable!("overflow is rejected before prepare"),
            }
        }
        Ok(prepared)
    }

    fn record_visit_url(&self, url: &str) -> Result<(), BrowserError> {
        let origin = url::Url::parse(url)
            .map(|parsed| vixen_net::Origin::from_url(&parsed))
            .unwrap_or_else(|_| vixen_net::Origin::opaque())
            .partition_key();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
            .unwrap_or_default();
        self.store
            .record_visit(&origin, url, timestamp)
            .map_err(|error| {
                BrowserError::new(
                    browser_error_codes::NAVIGATION_LOAD,
                    format!("profile history write failed: {error}"),
                )
            })
    }

    fn fail_navigation(
        &mut self,
        context_id: BrowsingContextId,
        navigation_id: NavigationId,
        request_id: Option<RequestId>,
        error: BrowserError,
    ) {
        let _ = self.finish_navigation(
            context_id,
            navigation_id,
            NavigationTerminal::Failed { request_id, error },
            true,
        );
    }

    fn finish_navigation(
        &mut self,
        context_id: BrowsingContextId,
        navigation_id: NavigationId,
        terminal: NavigationTerminal,
        emit_state: bool,
    ) -> Result<bool, BrowserError> {
        let context = self.context_mut(context_id)?;
        let Some(active) = context.active_navigation.as_ref() else {
            return Ok(false);
        };
        if active.navigation_id != navigation_id {
            return Ok(false);
        }

        let mut active = context
            .active_navigation
            .take()
            .expect("matching navigation is active");
        let frame_id = context.frame_id;
        let document_id = context.document_id;
        let runtime_context_id = context.runtime_context_id;
        let document_url = context.page.url().to_owned();
        let active_request_id = active.request_id;
        if matches!(&terminal, NavigationTerminal::Cancelled { .. }) {
            let canceled_script = active.pending_script.as_ref().map(|pending| {
                let request_id = pending.key.request_id.to_string();
                let url = pending.request.url().to_string();
                vec![
                    RuntimeNetworkEvent::Request {
                        request_id: request_id.clone(),
                        url: url.clone(),
                        method: Method::Get.as_str().to_owned(),
                    },
                    RuntimeNetworkEvent::Failure {
                        request_id,
                        url,
                        error_text: "external script load canceled".to_owned(),
                        blocked_reason: Some("canceled".to_owned()),
                    },
                ]
            });
            if let Some(cancel) = active.cancel.take() {
                let _ = cancel.send(());
            }
            if let Some(load_task) = active.load_task.take() {
                load_task.abort();
            }
            if let Some(network) = canceled_script {
                self.emit(BrowserEvent::RuntimeEffects {
                    context_id,
                    frame_id,
                    document_id,
                    runtime_context_id,
                    url: document_url,
                    effects: RuntimeEffects {
                        network,
                        ..RuntimeEffects::default()
                    },
                });
            }
        }

        let phase = match &terminal {
            NavigationTerminal::Settled => NavigationPhase::Settled,
            NavigationTerminal::Failed { .. } => NavigationPhase::Failed,
            NavigationTerminal::Cancelled { .. } => NavigationPhase::Cancelled,
        };
        self.emit_phase(context_id, frame_id, navigation_id, phase);
        match terminal {
            NavigationTerminal::Settled => {}
            NavigationTerminal::Failed { request_id, error } => {
                self.emit(BrowserEvent::NavigationFailed {
                    context_id,
                    frame_id,
                    navigation_id,
                    request_id,
                    error,
                });
            }
            NavigationTerminal::Cancelled { reason } => {
                self.emit(BrowserEvent::NavigationCancelled {
                    context_id,
                    frame_id,
                    navigation_id,
                    request_id: Some(active_request_id),
                    reason,
                });
            }
        }
        if emit_state {
            self.emit(BrowserEvent::BrowsingContextStateChanged {
                state: self.context_state(context_id)?,
            });
        }
        Ok(true)
    }

    fn emit_phase(
        &self,
        context_id: BrowsingContextId,
        frame_id: FrameId,
        navigation_id: NavigationId,
        phase: NavigationPhase,
    ) {
        self.emit(BrowserEvent::NavigationPhaseChanged {
            context_id,
            frame_id,
            navigation_id,
            phase,
        });
    }

    fn emit(&self, event: BrowserEvent) {
        self.events.push(event);
    }

    fn shutdown(&mut self) {
        let context_ids: Vec<_> = self.contexts.keys().copied().collect();
        for context_id in context_ids {
            let _ = self.cancel_active_navigation(
                context_id,
                NavigationCancellationReason::BrowserShutdown,
                true,
            );
        }
    }

    fn start_pending_loads(&mut self) {
        for start in self.pending_load_starts.drain(..) {
            let _ = start.send(());
        }
    }

    fn cancel_all_loads(&mut self) {
        for context in self.contexts.values_mut() {
            if let Some(mut navigation) = context.active_navigation.take() {
                if let Some(cancel) = navigation.cancel.take() {
                    let _ = cancel.send(());
                }
                if let Some(load_task) = navigation.load_task.take() {
                    load_task.abort();
                }
            }
        }
    }

    fn prune_runtime_slots(&mut self) {
        while self.runtime_slots.last().is_some_and(|slot| !slot.active) {
            self.runtime_slots.pop();
        }
    }

    fn ensure_context(&self, context_id: BrowsingContextId) -> Result<(), BrowserError> {
        if self.contexts.contains_key(&context_id) {
            return Ok(());
        }
        let (code, state) = if self.closed_contexts.contains(&context_id) {
            (browser_error_codes::STALE_CONTEXT, "closed")
        } else {
            (browser_error_codes::UNKNOWN_CONTEXT, "unknown")
        };
        Err(BrowserError::new(
            code,
            format!("browsing context {context_id} is {state}"),
        ))
    }

    fn context(&self, context_id: BrowsingContextId) -> Result<&BrowsingContext, BrowserError> {
        self.ensure_context(context_id)?;
        Ok(self.contexts.get(&context_id).expect("context checked"))
    }

    fn context_mut(
        &mut self,
        context_id: BrowsingContextId,
    ) -> Result<&mut BrowsingContext, BrowserError> {
        self.ensure_context(context_id)?;
        Ok(self.contexts.get_mut(&context_id).expect("context checked"))
    }

    fn context_for_document(
        &self,
        context_id: BrowsingContextId,
        document_id: DocumentId,
    ) -> Result<&BrowsingContext, BrowserError> {
        let context = self.context(context_id)?;
        if context.document_id != document_id {
            return Err(BrowserError::new(
                browser_error_codes::STALE_DOCUMENT,
                format!("document {document_id} is no longer active in context {context_id}"),
            ));
        }
        Ok(context)
    }

    fn context_for_document_mut(
        &mut self,
        context_id: BrowsingContextId,
        document_id: DocumentId,
    ) -> Result<&mut BrowsingContext, BrowserError> {
        let context = self.context_mut(context_id)?;
        if context.document_id != document_id {
            return Err(BrowserError::new(
                browser_error_codes::STALE_DOCUMENT,
                format!("document {document_id} is no longer active in context {context_id}"),
            ));
        }
        Ok(context)
    }

    fn context_state(
        &self,
        context_id: BrowsingContextId,
    ) -> Result<BrowsingContextState, BrowserError> {
        Ok(context_state(context_id, self.context(context_id)?))
    }
}

fn context_state(context_id: BrowsingContextId, context: &BrowsingContext) -> BrowsingContextState {
    let active_navigation_id = context
        .active_navigation
        .as_ref()
        .map(|navigation| navigation.navigation_id);
    BrowsingContextState {
        context_id,
        main_frame_id: context.frame_id,
        document_id: context.document_id,
        runtime_context_id: Some(context.runtime_context_id),
        active_navigation_id,
        url: context.page.url().to_owned(),
        title: context.page.document().title(),
        history_length: context.page.session_history().length(),
        history_index: context.page.session_history().index(),
        can_go_back: context.page.session_history().can_go_back(),
        can_go_forward: context.page.session_history().can_go_forward(),
        is_loading: active_navigation_id.is_some(),
        load_progress: if active_navigation_id.is_some() {
            0.1
        } else {
            1.0
        },
        page_zoom: context.page_zoom,
    }
}

enum BoundedFileReadError {
    Inspect(std::io::Error),
    TooLarge,
    Open(std::io::Error),
    Read(std::io::Error),
}

async fn read_bounded_file(
    path: &std::path::Path,
    max_body_bytes: u64,
) -> Result<Vec<u8>, BoundedFileReadError> {
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(BoundedFileReadError::Inspect)?;
    if metadata.len() > max_body_bytes {
        return Err(BoundedFileReadError::TooLarge);
    }
    let file = tokio::fs::File::open(path)
        .await
        .map_err(BoundedFileReadError::Open)?;
    let capacity = usize::try_from(metadata.len().min(max_body_bytes)).unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    let mut bounded = tokio::io::AsyncReadExt::take(file, max_body_bytes.saturating_add(1));
    tokio::io::AsyncReadExt::read_to_end(&mut bounded, &mut bytes)
        .await
        .map_err(BoundedFileReadError::Read)?;
    if (bytes.len() as u64) > max_body_bytes {
        return Err(BoundedFileReadError::TooLarge);
    }
    Ok(bytes)
}

async fn load_source(
    network: &mut Network,
    cookies: &mut CookieJar,
    input: SourceLoadInput,
    max_body_bytes: u64,
    mut on_progress: impl FnMut(&NetworkEvent),
) -> Result<LoadedSource, BrowserError> {
    let SourceLoadInput { url, injected_html } = input;
    if let Some(html) = injected_html {
        return Ok(LoadedSource {
            final_url: url,
            html,
            headers: Vec::new(),
        });
    }
    let parsed = url::Url::parse(&url).map_err(|error| {
        BrowserError::new(
            browser_error_codes::INVALID_ARGUMENT,
            format!("invalid navigation URL: {error}"),
        )
    })?;
    match parsed.scheme() {
        "about" if parsed.path() == "blank" => Ok(LoadedSource {
            final_url: parsed.to_string(),
            html: "<!doctype html><title></title>".to_owned(),
            headers: Vec::new(),
        }),
        "about" if parsed.path() == "vixen" => Ok(LoadedSource {
            final_url: parsed.to_string(),
            html: "<!doctype html><title>Vixen</title><h1>Vixen</h1>".to_owned(),
            headers: Vec::new(),
        }),
        "data" => {
            let data = parse_data_url(&url).map_err(|error| {
                BrowserError::new(browser_error_codes::NAVIGATION_LOAD, error.to_string())
            })?;
            Ok(LoadedSource {
                final_url: parsed.to_string(),
                html: String::from_utf8_lossy(&data.data).into_owned(),
                headers: vec![("content-type".to_owned(), data.mime_type.essence())],
            })
        }
        "file" => {
            let mut path_url = parsed.clone();
            path_url.set_query(None);
            path_url.set_fragment(None);
            let path = path_url.to_file_path().map_err(|_| {
                BrowserError::new(
                    browser_error_codes::INVALID_ARGUMENT,
                    "file URL has no local path",
                )
            })?;
            let bytes =
                read_bounded_file(&path, max_body_bytes)
                    .await
                    .map_err(|error| match error {
                        BoundedFileReadError::TooLarge => BrowserError::new(
                            browser_error_codes::NAVIGATION_LOAD,
                            format!(
                                "navigation file body exceeds {max_body_bytes} bytes at {}",
                                path.display()
                            ),
                        ),
                        BoundedFileReadError::Inspect(error)
                        | BoundedFileReadError::Open(error)
                        | BoundedFileReadError::Read(error) => BrowserError::new(
                            browser_error_codes::NAVIGATION_LOAD,
                            format!("failed to read {}: {error}", path.display()),
                        ),
                    })?;
            let html = String::from_utf8(bytes).map_err(|_| {
                BrowserError::new(
                    browser_error_codes::NAVIGATION_LOAD,
                    format!(
                        "failed to read {}: stream did not contain valid UTF-8",
                        path.display()
                    ),
                )
            })?;
            Ok(LoadedSource {
                final_url: parsed.to_string(),
                html,
                headers: Vec::new(),
            })
        }
        "http" | "https" => {
            let response = network
                .get_text_with_cookies_request_with_progress(
                    cookies,
                    TextRequest {
                        url: parsed,
                        cross_site: false,
                        method: Method::Get,
                        redirect_mode: RedirectMode::Follow,
                        headers: Vec::new(),
                        body: None,
                    },
                    &mut on_progress,
                )
                .await
                .map_err(|error| {
                    BrowserError::new(
                        browser_error_codes::NAVIGATION_LOAD,
                        format!("navigation fetch failed: {error}"),
                    )
                })?;
            Ok(LoadedSource {
                final_url: response.final_url,
                html: response.body,
                headers: response.headers.into_iter().collect(),
            })
        }
        scheme => Err(BrowserError::new(
            browser_error_codes::INVALID_ARGUMENT,
            format!("unsupported navigation URL scheme: {scheme}"),
        )),
    }
}

struct ExternalScriptLoadInput<'a> {
    store: &'a Store,
    profile_baseline: &'a mut Vec<vixen_net::CookieSnapshot>,
    request: ExternalPageScript,
    max_body_bytes: u64,
    max_redirects: usize,
}

async fn load_external_script(
    network: &mut Network,
    cookies: &mut CookieJar,
    input: ExternalScriptLoadInput<'_>,
) -> Result<LoadedExternalScript, ExternalScriptLoadFailure> {
    let ExternalScriptLoadInput {
        store,
        profile_baseline,
        request,
        max_body_bytes,
        max_redirects,
    } = input;
    let url = request.url().clone();
    match url.scheme() {
        "file" => {
            let mut path_url = url.clone();
            path_url.set_query(None);
            path_url.set_fragment(None);
            let path = path_url
                .to_file_path()
                .map_err(|_| ExternalScriptLoadFailure {
                    error: BrowserError::new(
                        browser_error_codes::INVALID_ARGUMENT,
                        "external script file URL has no local path",
                    ),
                    url: url.to_string(),
                    events: Vec::new(),
                    blocked_reason: "load",
                })?;
            let bytes = read_bounded_file(&path, max_body_bytes)
                .await
                .map_err(|error| {
                    let error = match error {
                        BoundedFileReadError::Inspect(error) => BrowserError::new(
                            browser_error_codes::NAVIGATION_LOAD,
                            format!(
                                "failed to inspect external script {}: {error}",
                                path.display()
                            ),
                        ),
                        BoundedFileReadError::TooLarge => BrowserError::new(
                            browser_error_codes::NAVIGATION_LOAD,
                            format!(
                                "external script body exceeds {max_body_bytes} bytes at {}",
                                path.display()
                            ),
                        ),
                        BoundedFileReadError::Open(error) => BrowserError::new(
                            browser_error_codes::NAVIGATION_LOAD,
                            format!("failed to open external script {}: {error}", path.display()),
                        ),
                        BoundedFileReadError::Read(error) => BrowserError::new(
                            browser_error_codes::NAVIGATION_LOAD,
                            format!("failed to read external script {}: {error}", path.display()),
                        ),
                    };
                    ExternalScriptLoadFailure {
                        error,
                        url: url.to_string(),
                        events: Vec::new(),
                        blocked_reason: "load",
                    }
                })?;
            Ok(LoadedExternalScript::File {
                final_url: url,
                source: String::from_utf8_lossy(&bytes).into_owned(),
            })
        }
        "http" | "https" => {
            let mut current = url;
            let mut redirects = 0_u32;
            let mut visited = HashSet::new();
            let mut events = Vec::new();
            let mut requested_urls = Vec::new();
            loop {
                if let Some(blocked_reason) = request.blocked_reason(&current) {
                    return Err(ExternalScriptLoadFailure {
                        error: BrowserError::new(
                            browser_error_codes::NAVIGATION_LOAD,
                            format!("external script blocked by {blocked_reason}: {current}"),
                        ),
                        url: current.to_string(),
                        events,
                        blocked_reason,
                    });
                }
                visited.insert(current.to_string());
                merge_profile_cookies(store, &current, cookies, profile_baseline).map_err(
                    |error| ExternalScriptLoadFailure {
                        error: script_error(error),
                        url: current.to_string(),
                        events: events.clone(),
                        blocked_reason: "load",
                    },
                )?;
                requested_urls.push(current.clone());
                let mut hop_events = Vec::new();
                let response = network
                    .get_text_with_cookies_request_with_progress(
                        cookies,
                        TextRequest {
                            url: current.clone(),
                            cross_site: request.is_cross_site(&current),
                            method: Method::Get,
                            redirect_mode: RedirectMode::Manual,
                            headers: Vec::new(),
                            body: None,
                        },
                        |event| hop_events.push(event.clone()),
                    )
                    .await;
                let mut response = match response {
                    Ok(response) => response,
                    Err(error) => {
                        events.extend(hop_events);
                        return Err(ExternalScriptLoadFailure {
                            error: BrowserError::new(
                                browser_error_codes::NAVIGATION_LOAD,
                                format!("external script fetch failed: {error}"),
                            ),
                            url: current.to_string(),
                            events,
                            blocked_reason: "load",
                        });
                    }
                };
                events.append(&mut response.events);

                if !is_followable_redirect(response.status) {
                    response.events = events;
                    response.redirects = redirects;
                    return Ok(LoadedExternalScript::Http {
                        response,
                        requested_urls,
                    });
                }
                if redirects as usize >= max_redirects {
                    return Err(ExternalScriptLoadFailure {
                        error: BrowserError::new(
                            browser_error_codes::NAVIGATION_LOAD,
                            format!(
                                "external script fetch failed: too many redirects (>{max_redirects}) fetching {current}"
                            ),
                        ),
                        url: current.to_string(),
                        events,
                        blocked_reason: "load",
                    });
                }
                let location = response.header("location").ok_or_else(|| {
                    ExternalScriptLoadFailure {
                        error: BrowserError::new(
                            browser_error_codes::NAVIGATION_LOAD,
                            format!(
                                "external script fetch failed: invalid redirect target from {current}"
                            ),
                        ),
                        url: current.to_string(),
                        events: events.clone(),
                        blocked_reason: "load",
                    }
                })?;
                let next = current.join(location).map_err(|_| ExternalScriptLoadFailure {
                    error: BrowserError::new(
                        browser_error_codes::NAVIGATION_LOAD,
                        format!(
                            "external script fetch failed: invalid redirect target from {current}"
                        ),
                    ),
                    url: current.to_string(),
                    events: events.clone(),
                    blocked_reason: "load",
                })?;
                if visited.contains(&next.to_string()) {
                    return Err(ExternalScriptLoadFailure {
                        error: BrowserError::new(
                            browser_error_codes::NAVIGATION_LOAD,
                            format!(
                                "external script fetch failed: redirect loop detected at {next}"
                            ),
                        ),
                        url: next.to_string(),
                        events,
                        blocked_reason: "load",
                    });
                }
                if let Err(error) = vixen_net::validate_http_url(&next) {
                    return Err(ExternalScriptLoadFailure {
                        error: BrowserError::new(
                            browser_error_codes::NAVIGATION_LOAD,
                            format!(
                                "external script fetch failed: URL rejected by policy: {error}"
                            ),
                        ),
                        url: next.to_string(),
                        events,
                        blocked_reason: "load",
                    });
                }
                if matches!(
                    events.last(),
                    Some(NetworkEvent::Response { url, status })
                        if url == current.as_str() && *status == response.status
                ) {
                    events.pop();
                }
                events.push(NetworkEvent::Redirect {
                    from: current.to_string(),
                    to: next.to_string(),
                    status: response.status,
                });
                if let Some(blocked_reason) = request.blocked_reason(&next) {
                    return Err(ExternalScriptLoadFailure {
                        error: BrowserError::new(
                            browser_error_codes::NAVIGATION_LOAD,
                            format!("external script blocked by {blocked_reason}: {next}"),
                        ),
                        url: next.to_string(),
                        events,
                        blocked_reason,
                    });
                }
                redirects += 1;
                current = next;
            }
        }
        scheme => Err(ExternalScriptLoadFailure {
            error: BrowserError::new(
                browser_error_codes::INVALID_ARGUMENT,
                format!("unsupported external script URL scheme: {scheme}"),
            ),
            url: url.to_string(),
            events: Vec::new(),
            blocked_reason: "load",
        }),
    }
}

fn is_followable_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HistoryDisposition {
    Push,
    Replace,
    Keep,
}

fn next_process_id<T>(
    counter: &AtomicU64,
    constructor: impl FnOnce(u64) -> Option<T>,
) -> Result<T, BrowserError> {
    let raw = counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| {
            BrowserError::new(
                browser_error_codes::ID_EXHAUSTED,
                "process browser id space is exhausted",
            )
        })?;
    constructor(raw).ok_or_else(|| {
        BrowserError::new(
            browser_error_codes::ID_EXHAUSTED,
            "process browser id allocation produced zero",
        )
    })
}

fn command_send_error<T>(error: mpsc::TrySendError<T>) -> BrowserError {
    match error {
        mpsc::TrySendError::Full(_) => BrowserError::new(
            browser_error_codes::COMMAND_QUEUE_FULL,
            "browser command queue is full",
        ),
        mpsc::TrySendError::Disconnected(_) => {
            BrowserError::new(browser_error_codes::CLOSED, "browser core is closed")
        }
    }
}

fn navigation_id_from_result(result: BrowserCommandResult) -> Result<NavigationId, BrowserError> {
    match result {
        BrowserCommandResult::NavigationAccepted { navigation_id } => Ok(navigation_id),
        result => Err(BrowserError::new(
            browser_error_codes::CLOSED,
            format!("unexpected navigation result: {result:?}"),
        )),
    }
}

fn event_queue_poisoned() -> BrowserError {
    BrowserError::new(
        browser_error_codes::CLOSED,
        "browser event queue is poisoned",
    )
}

fn profile_error(error: vixen_store::StoreError) -> BrowserError {
    BrowserError::new(
        browser_error_codes::PROFILE,
        format!("profile operation failed: {error}"),
    )
}

fn validate_viewport(viewport: (u32, u32)) -> Result<(), BrowserError> {
    if viewport.0 == 0
        || viewport.1 == 0
        || viewport.0 > MAX_VIEWPORT_DIMENSION
        || viewport.1 > MAX_VIEWPORT_DIMENSION
    {
        return Err(BrowserError::new(
            browser_error_codes::INVALID_ARGUMENT,
            format!(
                "viewport must be within 1x1 and {MAX_VIEWPORT_DIMENSION}x{MAX_VIEWPORT_DIMENSION}"
            ),
        ));
    }
    Ok(())
}

fn validate_text_input_state(state: &TextInputState) -> Result<(), BrowserError> {
    if state.text.len() > MAX_TEXT_INPUT_BYTES {
        return Err(BrowserError::new(
            browser_error_codes::INVALID_ARGUMENT,
            "text input value exceeds the browser limit",
        ));
    }
    let utf16_len = u32::try_from(state.text.encode_utf16().count()).map_err(|_| {
        BrowserError::new(
            browser_error_codes::INVALID_ARGUMENT,
            "text input value exceeds the UTF-16 range limit",
        )
    })?;
    if state.selection.base_offset > utf16_len || state.selection.extent_offset > utf16_len {
        return Err(BrowserError::new(
            browser_error_codes::INVALID_ARGUMENT,
            "text input selection exceeds the value",
        ));
    }
    if let Some(composing) = state.composing
        && (composing.base_offset > composing.extent_offset || composing.extent_offset > utf16_len)
    {
        return Err(BrowserError::new(
            browser_error_codes::INVALID_ARGUMENT,
            "text input composing range is invalid",
        ));
    }
    Ok(())
}

fn page_layout_viewport(viewport: (u32, u32), zoom: f64) -> (u32, u32) {
    (
        ((f64::from(viewport.0) / zoom).ceil() as u32).max(1),
        ((f64::from(viewport.1) / zoom).ceil() as u32).max(1),
    )
}

fn keyboard_scroll_delta(
    event_type: &str,
    event: &KeyEventData,
    context: &BrowsingContext,
) -> Option<(f64, f64)> {
    if !matches!(event_type, "keyDown" | "keydown")
        || event.ctrl_key
        || event.alt_key
        || event.meta_key
        || context.page.focused_element_consumes_scroll_keys()
    {
        return None;
    }
    let viewport = page_layout_viewport(context.host_view.viewport, context.page_zoom);
    let page = f64::from(viewport.1) * KEYBOARD_SCROLL_PAGE_FRACTION;
    match event.key.as_str() {
        "ArrowLeft" => Some((-KEYBOARD_SCROLL_LINE_PX, 0.0)),
        "ArrowRight" => Some((KEYBOARD_SCROLL_LINE_PX, 0.0)),
        "ArrowUp" => Some((0.0, -KEYBOARD_SCROLL_LINE_PX)),
        "ArrowDown" => Some((0.0, KEYBOARD_SCROLL_LINE_PX)),
        "PageUp" => Some((0.0, -page)),
        "PageDown" => Some((0.0, page)),
        "Home" => Some((0.0, -f64::MAX)),
        "End" => Some((0.0, f64::MAX)),
        " " if event.shift_key => Some((0.0, -page)),
        " " => Some((0.0, page)),
        _ => None,
    }
}

fn scale_paint_commands(commands: &mut [PaintCommand], scale: f32) {
    for command in commands {
        let rect = match command {
            PaintCommand::Background { fill, .. } => fill,
            PaintCommand::Text { rect, .. } => rect,
        };
        rect.x *= scale;
        rect.y *= scale;
        rect.w *= scale;
        rect.h *= scale;
    }
}

fn validate_lookup_id(id: &str) -> Result<(), BrowserError> {
    if id.len() > MAX_SELECTOR_BYTES {
        return Err(BrowserError::new(
            browser_error_codes::INVALID_ARGUMENT,
            "element id exceeds the browser query limit",
        ));
    }
    Ok(())
}

fn form_submission_info(snapshot: crate::page::FormSubmissionSnapshot) -> FormSubmissionInfo {
    let entries = snapshot
        .entries
        .into_iter()
        .map(|entry| FormEntryInfo {
            name: entry.name,
            value: match entry.value {
                crate::form_submission::FormEntryValue::Text(value) => {
                    FormEntryValueInfo::Text(value)
                }
                crate::form_submission::FormEntryValue::File {
                    filename,
                    content_type,
                    body,
                } => FormEntryValueInfo::File {
                    filename,
                    content_type,
                    body,
                },
            },
        })
        .collect();
    FormSubmissionInfo {
        form: snapshot.form,
        action: snapshot.action,
        method: snapshot.method,
        enctype: snapshot.enctype,
        content_type: snapshot.content_type,
        entries,
        body: snapshot.body,
    }
}

fn script_value(value: JsValue) -> ScriptValue {
    match value {
        JsValue::Int32(value) => ScriptValue::Int32(value),
        JsValue::Number(value) => ScriptValue::Number(value),
        JsValue::String(value) => ScriptValue::String(value),
        JsValue::Bool(value) => ScriptValue::Bool(value),
        JsValue::Null => ScriptValue::Null,
        JsValue::Undefined => ScriptValue::Undefined,
        JsValue::Object => ScriptValue::Object,
    }
}

fn apply_runtime_config(runtime: &mut JsRuntime, config: &BrowsingContextConfig) {
    runtime.set_extra_http_headers(config.extra_http_headers.clone());
    runtime.set_cache_disabled(config.cache_disabled);
    runtime.reset_permission_overrides();
    for grant in &config.permission_grants {
        runtime.replace_permission_grants(grant.origin.clone(), grant.permissions.clone());
    }
}

fn external_script_network_events(
    request_id: RequestId,
    request_url: &str,
    result: &Result<LoadedExternalScript, ExternalScriptLoadFailure>,
) -> Vec<RuntimeNetworkEvent> {
    let request_id = request_id.to_string();
    match result {
        Ok(LoadedExternalScript::File { final_url, .. }) => vec![
            RuntimeNetworkEvent::Request {
                request_id: request_id.clone(),
                url: request_url.to_owned(),
                method: Method::Get.as_str().to_owned(),
            },
            RuntimeNetworkEvent::Response {
                request_id,
                url: final_url.to_string(),
                status: 200,
            },
        ],
        Ok(LoadedExternalScript::Http { response, .. }) => response
            .events
            .iter()
            .map(|event| match event {
                NetworkEvent::RequestStart { url, method } => RuntimeNetworkEvent::Request {
                    request_id: request_id.clone(),
                    url: url.clone(),
                    method: method.as_str().to_owned(),
                },
                NetworkEvent::Redirect { from, to, status } => RuntimeNetworkEvent::Redirect {
                    request_id: request_id.clone(),
                    from: from.clone(),
                    to: to.clone(),
                    status: *status,
                },
                NetworkEvent::Response { url, status } => RuntimeNetworkEvent::Response {
                    request_id: request_id.clone(),
                    url: url.clone(),
                    status: *status,
                },
            })
            .collect(),
        Err(failure) => {
            let mut events: Vec<_> = failure
                .events
                .iter()
                .map(|event| match event {
                    NetworkEvent::RequestStart { url, method } => RuntimeNetworkEvent::Request {
                        request_id: request_id.clone(),
                        url: url.clone(),
                        method: method.as_str().to_owned(),
                    },
                    NetworkEvent::Redirect { from, to, status } => RuntimeNetworkEvent::Redirect {
                        request_id: request_id.clone(),
                        from: from.clone(),
                        to: to.clone(),
                        status: *status,
                    },
                    NetworkEvent::Response { url, status } => RuntimeNetworkEvent::Response {
                        request_id: request_id.clone(),
                        url: url.clone(),
                        status: *status,
                    },
                })
                .collect();
            if events.is_empty() {
                events.push(RuntimeNetworkEvent::Request {
                    request_id: request_id.clone(),
                    url: request_url.to_owned(),
                    method: Method::Get.as_str().to_owned(),
                });
            }
            events.push(RuntimeNetworkEvent::Failure {
                request_id,
                url: failure.url.clone(),
                error_text: failure.error.to_string(),
                blocked_reason: Some(failure.blocked_reason.to_owned()),
            });
            events
        }
    }
}

fn drain_runtime_effects(
    runtime: &mut JsRuntime,
) -> Result<RuntimeEffects, crate::engine_error::EngineError> {
    let console = runtime
        .drain_console_events()?
        .into_iter()
        .map(|event| RuntimeConsoleEvent {
            kind: event.kind,
            args: event
                .args
                .into_iter()
                .map(|arg| RuntimeConsoleArg {
                    type_name: arg.type_name,
                    subtype: arg.subtype,
                    value: arg.value.map(|value| match value {
                        JsConsoleValue::String(value) => RuntimeConsoleValue::String(value),
                        JsConsoleValue::Number(value) => RuntimeConsoleValue::Number(value),
                        JsConsoleValue::Bool(value) => RuntimeConsoleValue::Bool(value),
                        JsConsoleValue::Null => RuntimeConsoleValue::Null,
                    }),
                    unserializable_value: arg.unserializable_value,
                    description: arg.description,
                })
                .collect(),
        })
        .collect();
    let dialogs = runtime
        .drain_dialog_events()?
        .into_iter()
        .map(|event| RuntimeDialogEvent {
            kind: event.kind,
            message: event.message,
            default_prompt: event.default_prompt,
        })
        .collect();
    let bindings = runtime
        .drain_binding_events()?
        .into_iter()
        .map(|event| RuntimeBindingEvent {
            name: event.name,
            payload: event.payload,
        })
        .collect();
    let network = runtime
        .drain_network_events()?
        .into_iter()
        .map(|event| match event {
            JsNetworkEvent::Request {
                request_id,
                url,
                method,
            } => RuntimeNetworkEvent::Request {
                request_id,
                url,
                method,
            },
            JsNetworkEvent::Redirect {
                request_id,
                from,
                to,
                status,
            } => RuntimeNetworkEvent::Redirect {
                request_id,
                from,
                to,
                status,
            },
            JsNetworkEvent::Response {
                request_id,
                url,
                status,
            } => RuntimeNetworkEvent::Response {
                request_id,
                url,
                status,
            },
            JsNetworkEvent::Failure {
                request_id,
                url,
                error_text,
                blocked_reason,
            } => RuntimeNetworkEvent::Failure {
                request_id,
                url,
                error_text,
                blocked_reason,
            },
        })
        .collect();
    Ok(RuntimeEffects {
        console,
        dialogs,
        bindings,
        network,
        exceptions: Vec::new(),
    })
}

fn discard_runtime_outputs(runtime: &mut JsRuntime) {
    let _ = runtime.drain_console_events();
    let _ = runtime.drain_dialog_events();
    let _ = runtime.drain_binding_events();
    let _ = runtime.drain_network_events();
    let _ = runtime.drain_navigation_actions();
}

fn append_navigation_actions_bounded(
    queued: &mut Vec<JsNavigationAction>,
    actions: Vec<JsNavigationAction>,
) {
    if queued.len() > MAX_NAVIGATION_ACTIONS_PER_COMMAND {
        return;
    }
    let remaining = MAX_NAVIGATION_ACTIONS_PER_COMMAND + 1 - queued.len();
    queued.extend(actions.into_iter().take(remaining));
}

fn json_string(value: &str) -> Result<String, BrowserError> {
    deno_core::serde_json::to_string(value).map_err(|error| {
        BrowserError::new(
            browser_error_codes::INVALID_ARGUMENT,
            format!("failed to encode script input: {error}"),
        )
    })
}

fn host_view_runtime_source(page: &Page, state: HostViewState) -> String {
    let (viewport_width, viewport_height) = page.layout_viewport();
    let (max_scroll_x, max_scroll_y) = page.root_scroll_max();
    let (scroll_x, scroll_y) = page.root_scroll();
    format!(
        "globalThis.__vixenApplyHostViewState ? globalThis.__vixenApplyHostViewState({}, {}, {viewport_width}, {viewport_height}, {max_scroll_x}, {max_scroll_y}, {scroll_x}, {scroll_y}) : false",
        state.focused, state.visible
    )
}

fn append_form_query(action: &str, body: &[u8]) -> Result<String, BrowserError> {
    if body.is_empty() {
        return Ok(action.to_owned());
    }
    let mut url = url::Url::parse(action).map_err(|error| {
        BrowserError::new(
            browser_error_codes::INVALID_ARGUMENT,
            format!("invalid form action URL: {error}"),
        )
    })?;
    let body = String::from_utf8_lossy(body);
    let query = match url.query() {
        Some(existing) if !existing.is_empty() => format!("{existing}&{body}"),
        _ => body.into_owned(),
    };
    url.set_query(Some(&query));
    Ok(url.to_string())
}

fn validate_navigation_url(url: &str) -> Result<(), BrowserError> {
    if url.len() > MAX_URL_BYTES {
        Err(BrowserError::new(
            browser_error_codes::INVALID_ARGUMENT,
            "navigation URL exceeds the browser limit",
        ))
    } else {
        Ok(())
    }
}

fn prepare_history_state(
    history: &mut SessionHistory,
    url: String,
    state_json: String,
    title: String,
    replace: bool,
) -> Result<(), BrowserError> {
    validate_navigation_url(&url)?;
    ensure_same_origin_history_url(history.url().unwrap_or("about:blank"), &url)?;
    let mut entry = HistoryEntry::push_state(url, state_json.into_bytes());
    if !title.is_empty() {
        entry.title = Some(title);
    }
    if replace {
        history.replace(entry);
    } else {
        history.push(entry);
    }
    Ok(())
}

fn ensure_same_origin_history_url(current: &str, next: &str) -> Result<(), BrowserError> {
    let current = url::Url::parse(current).map_err(|error| {
        BrowserError::new(
            browser_error_codes::INVALID_ARGUMENT,
            format!("invalid current URL: {error}"),
        )
    })?;
    let next = url::Url::parse(next).map_err(|error| {
        BrowserError::new(
            browser_error_codes::INVALID_ARGUMENT,
            format!("invalid history URL: {error}"),
        )
    })?;
    if (current.scheme() == "file" && next.scheme() == "file") || current.origin() == next.origin()
    {
        Ok(())
    } else {
        Err(BrowserError::new(
            browser_error_codes::INVALID_ARGUMENT,
            "history state URL must be same-origin",
        ))
    }
}

fn engine_error(error: impl std::fmt::Display) -> BrowserError {
    BrowserError::new(browser_error_codes::NAVIGATION_LOAD, error.to_string())
}

fn script_error(error: crate::engine_error::EngineError) -> BrowserError {
    BrowserError::new(error.code(), error.to_string())
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use super::*;

    static NEXT_TEST_PROFILE: AtomicU64 = AtomicU64::new(1);

    fn test_config() -> BrowserConfig {
        let serial = NEXT_TEST_PROFILE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "vixen-browser-core-{}-{serial}.redb",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut config = BrowserConfig::new(path);
        config.document_overrides.insert(
            "https://same.test/a".to_owned(),
            "<!doctype html><title>A</title><main id='page'>A</main>".to_owned(),
        );
        config.document_overrides.insert(
            "https://same.test/b".to_owned(),
            "<!doctype html><title>B</title><main id='page'>B</main>".to_owned(),
        );
        config.document_overrides.insert(
            "https://same.test/next".to_owned(),
            "<!doctype html><title>Next</title><main id='page'>Next</main>".to_owned(),
        );
        config.document_overrides.insert(
            "https://same.test/input".to_owned(),
            "<!doctype html><button id='same' aria-controls='name'>Same</button><a id='go' href='https://same.test/b'>Go</a><input id='name' aria-label='Name'><div id='editor' contenteditable aria-label='Editor'>draft</div><input id='volume' type='range' aria-label='Volume' min='0' max='10' step='2' value='4'><div id='brightness' role='slider' tabindex='0' aria-label='Brightness' aria-valuemin='0' aria-valuemax='10' aria-valuenow='3'></div><script>document.getElementById('brightness').addEventListener('keydown', event => { if (event.key === 'ArrowRight') event.currentTarget.setAttribute('aria-valuenow', '4'); });</script>".to_owned(),
        );
        config.document_overrides.insert(
            "https://same.test/scroll".to_owned(),
            "<!doctype html><style>body{margin:0}#spacer{height:800px}</style><div id='spacer'>Top</div><div id='marker'>Bottom</div><input id='field' value='abc'><script>document.addEventListener('keydown', event => { if (event.key === 'PageUp') event.preventDefault(); });</script>".to_owned(),
        );
        config
    }

    fn create(handle: &mut EngineBrowserHandle) -> BrowsingContextId {
        match handle
            .dispatch(BrowserCommand::CreateBrowsingContext)
            .unwrap()
        {
            BrowserCommandResult::BrowsingContextCreated { context_id } => context_id,
            other => panic!("unexpected create result: {other:?}"),
        }
    }

    fn navigate(handle: &mut EngineBrowserHandle, context_id: BrowsingContextId, url: &str) {
        let navigation_id = dispatch_navigation(handle, context_id, url);
        wait_for_navigation(handle, context_id, navigation_id).unwrap();
    }

    fn reload(handle: &mut EngineBrowserHandle, context_id: BrowsingContextId) -> NavigationId {
        match handle
            .dispatch(BrowserCommand::Reload { context_id })
            .unwrap()
        {
            BrowserCommandResult::NavigationAccepted { navigation_id } => navigation_id,
            other => panic!("unexpected reload result: {other:?}"),
        }
    }

    fn traverse_history(
        handle: &mut EngineBrowserHandle,
        context_id: BrowsingContextId,
        delta: i32,
    ) -> Option<NavigationId> {
        match handle
            .dispatch(BrowserCommand::TraverseHistory { context_id, delta })
            .unwrap()
        {
            BrowserCommandResult::NavigationAccepted { navigation_id } => Some(navigation_id),
            BrowserCommandResult::Accepted => None,
            other => panic!("unexpected history traversal result: {other:?}"),
        }
    }

    fn history(
        handle: &mut EngineBrowserHandle,
        context_id: BrowsingContextId,
    ) -> NavigationHistorySnapshot {
        match handle
            .dispatch(BrowserCommand::GetNavigationHistory { context_id })
            .unwrap()
        {
            BrowserCommandResult::NavigationHistory(history) => history,
            other => panic!("unexpected history result: {other:?}"),
        }
    }

    fn dispatch_navigation(
        handle: &mut EngineBrowserHandle,
        context_id: BrowsingContextId,
        url: &str,
    ) -> NavigationId {
        match handle
            .dispatch(BrowserCommand::Navigate {
                context_id,
                url: url.to_owned(),
            })
            .unwrap()
        {
            BrowserCommandResult::NavigationAccepted { navigation_id } => navigation_id,
            other => panic!("unexpected navigation result: {other:?}"),
        }
    }

    fn wait_for_navigation(
        handle: &mut EngineBrowserHandle,
        context_id: BrowsingContextId,
        navigation_id: NavigationId,
    ) -> Result<Vec<BrowserEvent>, BrowserError> {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut observed = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let Some(event) = handle.wait_next_event(remaining)? else {
                return Err(BrowserError::new(
                    browser_error_codes::CLOSED,
                    format!("timed out waiting for navigation {navigation_id}"),
                ));
            };
            let terminal = match &event {
                BrowserEvent::NavigationPhaseChanged {
                    context_id: event_context_id,
                    navigation_id: event_navigation_id,
                    phase: NavigationPhase::Settled,
                    ..
                } if *event_context_id == context_id && *event_navigation_id == navigation_id => {
                    Some(Ok(()))
                }
                BrowserEvent::NavigationFailed {
                    context_id: event_context_id,
                    navigation_id: event_navigation_id,
                    error,
                    ..
                } if *event_context_id == context_id && *event_navigation_id == navigation_id => {
                    Some(Err(error.clone()))
                }
                BrowserEvent::NavigationCancelled {
                    context_id: event_context_id,
                    navigation_id: event_navigation_id,
                    reason,
                    ..
                } if *event_context_id == context_id && *event_navigation_id == navigation_id => {
                    Some(Err(BrowserError::new(
                        browser_error_codes::NAVIGATION_LOAD,
                        format!("navigation {navigation_id} was cancelled: {reason:?}"),
                    )))
                }
                _ => None,
            };
            observed.push(event);
            if let Some(result) = terminal {
                result?;
                return Ok(observed);
            }
        }
    }

    fn drain_events(handle: &mut EngineBrowserHandle) -> Vec<BrowserEvent> {
        let mut events = Vec::new();
        while let Some(event) = handle.try_next_event().unwrap() {
            events.push(event);
        }
        events
    }

    fn direct_core() -> (
        BrowserCore,
        Arc<EventChannel>,
        mpsc::Receiver<CoreMessage>,
        PathBuf,
    ) {
        let config = test_config();
        let profile_path = config.profile_path.clone();
        let events = Arc::new(EventChannel::new(config.event_capacity));
        let (command_tx, command_rx) = mpsc::sync_channel(config.command_capacity);
        let core = BrowserCore::new(config, Arc::clone(&events), command_tx).unwrap();
        (core, events, command_rx, profile_path)
    }

    fn direct_create(core: &mut BrowserCore) -> BrowsingContextId {
        match core.create_context().unwrap() {
            BrowserCommandResult::BrowsingContextCreated { context_id } => context_id,
            other => panic!("unexpected create result: {other:?}"),
        }
    }

    fn direct_begin_navigation(
        core: &mut BrowserCore,
        context_id: BrowsingContextId,
        url: &str,
    ) -> NavigationId {
        match core
            .navigate(context_id, url.to_owned(), HistoryUpdate::Push)
            .unwrap()
        {
            BrowserCommandResult::NavigationAccepted { navigation_id } => navigation_id,
            other => panic!("unexpected navigation result: {other:?}"),
        }
    }

    fn direct_complete_source(
        core: &mut BrowserCore,
        context_id: BrowsingContextId,
        navigation_id: NavigationId,
        url: &str,
        html: String,
    ) {
        let baseline = Vec::new();
        let worker_jar = CookieJar::from_snapshots(baseline.clone());
        core.complete_navigation(SourceLoadCompletion {
            context_id,
            navigation_id,
            result: Ok(LoadedSource {
                final_url: url.to_owned(),
                html,
                headers: Vec::new(),
            }),
            cookie_delta: worker_jar.delta_from_snapshots(&baseline),
        });
    }

    fn direct_drive_navigation(
        core: &mut BrowserCore,
        context_id: BrowsingContextId,
        navigation_id: NavigationId,
    ) {
        while core
            .context(context_id)
            .unwrap()
            .active_navigation
            .as_ref()
            .is_some_and(|active| active.navigation_id == navigation_id)
        {
            assert!(core.advance_navigation_work(), "navigation work stalled");
        }
        core.start_pending_loads();
    }

    fn direct_navigate(
        core: &mut BrowserCore,
        context_id: BrowsingContextId,
        url: &str,
        html: &str,
    ) -> NavigationId {
        let navigation_id = direct_begin_navigation(core, context_id, url);
        direct_complete_source(core, context_id, navigation_id, url, html.to_owned());
        direct_drive_navigation(core, context_id, navigation_id);
        navigation_id
    }

    fn drain_direct_events(events: &EventChannel) -> Vec<BrowserEvent> {
        let mut observed = Vec::new();
        while let Some(event) = events.pop(None).unwrap() {
            observed.push(event);
        }
        observed
    }

    fn large_parser_document(title: &str) -> String {
        format!(
            "<!doctype html><title>{title}</title><main>{}</main>",
            "parser work ".repeat(PARSER_WORK_BYTES / 4)
        )
    }

    fn assert_parser_is_pending(
        core: &BrowserCore,
        context_id: BrowsingContextId,
        navigation_id: NavigationId,
    ) {
        assert!(
            core.context(context_id)
                .unwrap()
                .active_navigation
                .as_ref()
                .is_some_and(|active| {
                    active.navigation_id == navigation_id
                        && matches!(active.work.as_ref(), Some(NavigationWork::Parsing(_)))
                })
        );
    }

    fn direct_drive_to_scripts(
        core: &mut BrowserCore,
        context_id: BrowsingContextId,
        navigation_id: NavigationId,
    ) {
        loop {
            let work = core
                .context(context_id)
                .unwrap()
                .active_navigation
                .as_ref()
                .filter(|active| active.navigation_id == navigation_id)
                .and_then(|active| active.work.as_ref());
            match work {
                Some(NavigationWork::Scripts(_)) => return,
                Some(NavigationWork::Parsing(_)) => assert!(core.advance_navigation_work()),
                _ => panic!("navigation did not reach script work"),
            }
        }
    }

    fn direct_eval(
        core: &mut BrowserCore,
        context_id: BrowsingContextId,
        source: &str,
    ) -> ScriptValue {
        let state = core.context_state(context_id).unwrap();
        match core
            .dispatch(BrowserCommand::Evaluate {
                context_id,
                document_id: state.document_id,
                runtime_context_id: state.runtime_context_id.unwrap(),
                source: source.to_owned(),
            })
            .unwrap()
        {
            BrowserCommandResult::Evaluation(evaluation) => evaluation.value,
            other => panic!("unexpected eval result: {other:?}"),
        }
    }

    fn assert_no_lifecycle_success(events: &[BrowserEvent], navigation_id: NavigationId) {
        assert!(!events.iter().any(|event| matches!(
            event,
            BrowserEvent::DomContentLoaded { navigation_id: event_navigation_id, .. }
                | BrowserEvent::DocumentLoadCompleted { navigation_id: event_navigation_id, .. }
                | BrowserEvent::NavigationFailed { navigation_id: event_navigation_id, .. }
                if *event_navigation_id == navigation_id
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            BrowserEvent::NavigationPhaseChanged {
                navigation_id: event_navigation_id,
                phase: NavigationPhase::DomContentLoaded
                    | NavigationPhase::Load
                    | NavigationPhase::Settled
                    | NavigationPhase::Failed,
                ..
            } if *event_navigation_id == navigation_id
        )));
    }

    fn inject_late_source_completion(
        handle: &EngineBrowserHandle,
        context_id: BrowsingContextId,
        navigation_id: NavigationId,
        url: String,
        title: &str,
        set_cookie: Option<&str>,
    ) {
        let baseline = Vec::new();
        let mut worker_jar = CookieJar::from_snapshots(baseline.clone());
        if let Some(set_cookie) = set_cookie {
            worker_jar
                .set_cookie(set_cookie, &url::Url::parse(&url).unwrap(), true)
                .unwrap();
        }
        handle
            .commands
            .send(CoreMessage::SourceLoaded(SourceLoadCompletion {
                context_id,
                navigation_id,
                result: Ok(LoadedSource {
                    final_url: url,
                    html: format!("<!doctype html><title>{title}</title>"),
                    headers: Vec::new(),
                }),
                cookie_delta: worker_jar.delta_from_snapshots(&baseline),
            }))
            .unwrap();
    }

    fn inject_source_progress(
        handle: &EngineBrowserHandle,
        context_id: BrowsingContextId,
        navigation_id: NavigationId,
        event: NetworkEvent,
    ) {
        handle
            .commands
            .send(CoreMessage::SourceLoadProgress(SourceLoadProgress {
                context_id,
                navigation_id,
                event,
            }))
            .unwrap();
    }

    struct GatedRequest {
        path: String,
        cookie: Option<String>,
        respond: mpsc::SyncSender<String>,
        completed: mpsc::Receiver<()>,
    }

    struct GatedHttpServer {
        address: std::net::SocketAddr,
        requests: mpsc::Receiver<GatedRequest>,
        join: std::thread::JoinHandle<()>,
    }

    impl GatedHttpServer {
        fn start(expected_requests: usize) -> Self {
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let address = listener.local_addr().unwrap();
            let (request_tx, requests) = mpsc::sync_channel(expected_requests);
            let join = std::thread::spawn(move || {
                let mut handlers = Vec::new();
                for _ in 0..expected_requests {
                    let (stream, _) = listener.accept().unwrap();
                    let request_tx = request_tx.clone();
                    handlers.push(std::thread::spawn(move || {
                        let mut stream = stream;
                        let mut request = [0_u8; 4096];
                        let read = stream.read(&mut request).unwrap();
                        let request = String::from_utf8_lossy(&request[..read]);
                        let path = request
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1))
                            .unwrap_or("/")
                            .to_owned();
                        let cookie = request.lines().find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("cookie")
                                .then(|| value.trim().to_owned())
                        });
                        let cookie_value = path.trim_matches('/').to_owned();
                        let (respond, response) = mpsc::sync_channel(1);
                        let (completed, completed_rx) = mpsc::sync_channel(1);
                        request_tx
                            .send(GatedRequest {
                                path,
                                cookie,
                                respond,
                                completed: completed_rx,
                            })
                            .unwrap();
                        let body = response
                            .recv_timeout(Duration::from_secs(10))
                            .expect("gated response watchdog");
                        let response = if body.starts_with("HTTP/") {
                            body
                        } else {
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nSet-Cookie: source={}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                cookie_value,
                                body.len(),
                                body
                            )
                        };
                        let _ = stream.write_all(response.as_bytes());
                        let _ = completed.send(());
                    }));
                }
                for handler in handlers {
                    handler.join().unwrap();
                }
            });
            Self {
                address,
                requests,
                join,
            }
        }

        fn request(&self) -> GatedRequest {
            self.requests
                .recv_timeout(Duration::from_secs(10))
                .expect("request arrival watchdog")
        }

        fn url(&self, path: &str) -> String {
            format!(
                "http://browser-core-vixen.com:{}{path}",
                self.address.port()
            )
        }

        fn configure(&self, config: &mut BrowserConfig) {
            config
                .network
                .dns_overrides
                .push(("browser-core-vixen.com".to_owned(), vec![self.address]));
        }

        fn join(self) {
            self.join.join().unwrap();
        }
    }

    fn redirect_response(location: &str) -> String {
        format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
    }

    fn redirect_response_with_cookie(location: &str, cookie: &str) -> String {
        format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nSet-Cookie: {cookie}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
    }

    fn script_response(source: &str, set_cookie: Option<&str>) -> String {
        let cookie = set_cookie
            .map(|value| format!("Set-Cookie: {value}\r\n"))
            .unwrap_or_default();
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\n{cookie}Content-Length: {}\r\nConnection: close\r\n\r\n{source}",
            source.len()
        )
    }

    fn loaded_script_response(final_url: &str, source: &str) -> LoadedExternalScript {
        LoadedExternalScript::Http {
            response: TextResponse {
                body: source.to_owned(),
                headers: BTreeMap::from([(
                    "content-type".to_owned(),
                    "text/javascript".to_owned(),
                )]),
                status: 200,
                final_url: final_url.to_owned(),
                set_cookie: Vec::new(),
                redirects: 0,
                events: Vec::new(),
            },
            requested_urls: vec![url::Url::parse(final_url).unwrap()],
        }
    }

    fn assert_navigation_cancelled(
        events: &[BrowserEvent],
        navigation_id: NavigationId,
        reason: NavigationCancellationReason,
    ) {
        assert!(events.iter().any(|event| matches!(
            event,
            BrowserEvent::NavigationCancelled {
                navigation_id: event_navigation_id,
                reason: event_reason,
                ..
            } if *event_navigation_id == navigation_id && *event_reason == reason
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            BrowserEvent::NavigationPhaseChanged {
                navigation_id: event_navigation_id,
                phase: NavigationPhase::Cancelled,
                ..
            } if *event_navigation_id == navigation_id
        )));
        assert_exactly_one_terminal_phase(events, navigation_id);
    }

    fn assert_no_terminal_success(events: &[BrowserEvent], navigation_id: NavigationId) {
        assert!(!events.iter().any(|event| matches!(
            event,
            BrowserEvent::NavigationCommitted { navigation_id: event_navigation_id, .. }
                | BrowserEvent::NavigationFailed { navigation_id: event_navigation_id, .. }
                | BrowserEvent::DomContentLoaded { navigation_id: event_navigation_id, .. }
                | BrowserEvent::DocumentLoadCompleted { navigation_id: event_navigation_id, .. }
                if *event_navigation_id == navigation_id
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            BrowserEvent::NavigationPhaseChanged {
                navigation_id: event_navigation_id,
                phase: NavigationPhase::Settled | NavigationPhase::Failed,
                ..
            } if *event_navigation_id == navigation_id
        )));
    }

    fn phases_for(events: &[BrowserEvent], navigation_id: NavigationId) -> Vec<NavigationPhase> {
        events
            .iter()
            .filter_map(|event| match event {
                BrowserEvent::NavigationPhaseChanged {
                    navigation_id: event_navigation_id,
                    phase,
                    ..
                } if *event_navigation_id == navigation_id => Some(*phase),
                _ => None,
            })
            .collect()
    }

    fn assert_exactly_one_terminal_phase(events: &[BrowserEvent], navigation_id: NavigationId) {
        assert_eq!(
            phases_for(events, navigation_id)
                .into_iter()
                .filter(|phase| phase.is_terminal())
                .count(),
            1,
            "navigation {navigation_id} must have exactly one terminal phase"
        );
    }

    fn state(
        handle: &mut EngineBrowserHandle,
        context_id: BrowsingContextId,
    ) -> BrowsingContextState {
        match handle
            .dispatch(BrowserCommand::GetBrowsingContextState { context_id })
            .unwrap()
        {
            BrowserCommandResult::BrowsingContextState(state) => state,
            other => panic!("unexpected state result: {other:?}"),
        }
    }

    fn eval(
        handle: &mut EngineBrowserHandle,
        state: &BrowsingContextState,
        source: &str,
    ) -> ScriptValue {
        match handle
            .dispatch(BrowserCommand::Evaluate {
                context_id: state.context_id,
                document_id: state.document_id,
                runtime_context_id: state.runtime_context_id.unwrap(),
                source: source.to_owned(),
            })
            .unwrap()
        {
            BrowserCommandResult::Evaluation(evaluation) => evaluation.value,
            other => panic!("unexpected eval result: {other:?}"),
        }
    }

    fn dispatch_test_key(
        handle: &mut EngineBrowserHandle,
        state: &BrowsingContextState,
        key: &str,
        shift_key: bool,
    ) {
        let result = handle
            .dispatch(BrowserCommand::DispatchKeyEvent {
                context_id: state.context_id,
                document_id: state.document_id,
                runtime_context_id: state.runtime_context_id.unwrap(),
                event_type: "keyDown".to_owned(),
                event: KeyEventData {
                    key: key.to_owned(),
                    code: key.to_owned(),
                    text: String::new(),
                    apply_text: false,
                    ctrl_key: false,
                    shift_key,
                    alt_key: false,
                    meta_key: false,
                    repeat: false,
                    location: 0,
                },
            })
            .unwrap();
        assert!(matches!(result, BrowserCommandResult::InputDispatched(_)));
    }

    fn element_y(
        handle: &mut EngineBrowserHandle,
        state: &BrowsingContextState,
        selector: &str,
        viewport: (u32, u32),
    ) -> f64 {
        let result = handle
            .dispatch(BrowserCommand::QuerySelectorAll {
                context_id: state.context_id,
                document_id: state.document_id,
                selector: selector.to_owned(),
                viewport,
            })
            .unwrap();
        let BrowserCommandResult::SelectorMatches(matches) = result else {
            panic!("unexpected selector result: {result:?}");
        };
        matches[0].bbox.unwrap().1
    }

    #[test]
    fn runtime_and_input_results_report_exact_navigation_ids() {
        let mut handle = spawn_browser(test_config()).unwrap();
        let context_id = create(&mut handle);
        navigate(&mut handle, context_id, "https://same.test/a");
        let current = state(&mut handle, context_id);

        let navigation_actions = match handle
            .dispatch(BrowserCommand::EvaluateForAutomation {
                context_id,
                document_id: current.document_id,
                runtime_context_id: current.runtime_context_id.unwrap(),
                source: "location.assign('https://same.test/b'); location.assign('https://same.test/next'); 'queued'".to_owned(),
            })
            .unwrap()
        {
            BrowserCommandResult::AutomationEvaluation(evaluation) => evaluation.navigation_actions,
            other => panic!("unexpected automation evaluation result: {other:?}"),
        };
        let navigation_ids = navigation_actions
            .iter()
            .filter_map(|action| match action {
                NavigationActionOutcome::CrossDocument { navigation_id, .. } => {
                    Some(*navigation_id)
                }
                NavigationActionOutcome::SameDocument { .. } => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(navigation_ids.len(), 2);
        assert!(navigation_ids[0] < navigation_ids[1]);
        handle
            .dispatch(BrowserCommand::Stop { context_id })
            .unwrap();

        navigate(&mut handle, context_id, "https://same.test/input");
        let current = state(&mut handle, context_id);
        let same_document = match handle
            .dispatch(BrowserCommand::EvaluateForAutomation {
                context_id,
                document_id: current.document_id,
                runtime_context_id: current.runtime_context_id.unwrap(),
                source: "history.pushState({ step: 1 }, '', '/input-state'); document.querySelector('#same').addEventListener('click', () => history.replaceState({ step: 2 }, '', '/input-click')); 'same'".to_owned(),
            })
            .unwrap()
        {
            BrowserCommandResult::AutomationEvaluation(evaluation) => evaluation,
            other => panic!("unexpected automation evaluation result: {other:?}"),
        };
        assert!(matches!(
            same_document.navigation_actions.as_slice(),
            [NavigationActionOutcome::SameDocument { url }] if url == "https://same.test/input-state"
        ));

        let same_node_id = match handle
            .dispatch(BrowserCommand::QuerySelectorAll {
                context_id,
                document_id: current.document_id,
                selector: "#same".to_owned(),
                viewport: (800, 600),
            })
            .unwrap()
        {
            BrowserCommandResult::SelectorMatches(matches) => matches[0].node_id,
            other => panic!("unexpected selector result: {other:?}"),
        };
        let same_input = match handle
            .dispatch(BrowserCommand::DispatchMouseEvent {
                context_id,
                document_id: current.document_id,
                runtime_context_id: current.runtime_context_id.unwrap(),
                node_id: same_node_id,
                event_type: "click".to_owned(),
                event: MouseEventData {
                    x: 1.0,
                    y: 1.0,
                    button: 0,
                    buttons: 0,
                    detail: 1,
                    related_node_id: None,
                    bubbles: true,
                    ctrl_key: false,
                    shift_key: false,
                    alt_key: false,
                    meta_key: false,
                    delta_x: 0.0,
                    delta_y: 0.0,
                },
            })
            .unwrap()
        {
            BrowserCommandResult::InputDispatched(result) => result,
            other => panic!("unexpected input result: {other:?}"),
        };
        assert!(matches!(
            same_input.navigation_actions.as_slice(),
            [NavigationActionOutcome::SameDocument { url }] if url == "https://same.test/input-click"
        ));

        let node_id = match handle
            .dispatch(BrowserCommand::QuerySelectorAll {
                context_id,
                document_id: current.document_id,
                selector: "#go".to_owned(),
                viewport: (800, 600),
            })
            .unwrap()
        {
            BrowserCommandResult::SelectorMatches(matches) => matches[0].node_id,
            other => panic!("unexpected selector result: {other:?}"),
        };
        let input_navigation_ids = match handle
            .dispatch(BrowserCommand::DispatchMouseEvent {
                context_id,
                document_id: current.document_id,
                runtime_context_id: current.runtime_context_id.unwrap(),
                node_id,
                event_type: "click".to_owned(),
                event: MouseEventData {
                    x: 1.0,
                    y: 1.0,
                    button: 0,
                    buttons: 0,
                    detail: 1,
                    related_node_id: None,
                    bubbles: true,
                    ctrl_key: false,
                    shift_key: false,
                    alt_key: false,
                    meta_key: false,
                    delta_x: 0.0,
                    delta_y: 0.0,
                },
            })
            .unwrap()
        {
            BrowserCommandResult::InputDispatched(result) => result
                .navigation_actions
                .into_iter()
                .filter_map(|action| match action {
                    NavigationActionOutcome::CrossDocument { navigation_id, .. } => {
                        Some(navigation_id)
                    }
                    NavigationActionOutcome::SameDocument { .. } => None,
                })
                .collect::<Vec<_>>(),
            other => panic!("unexpected input result: {other:?}"),
        };
        assert_eq!(input_navigation_ids.len(), 1);
        assert!(input_navigation_ids[0] > navigation_ids[1]);
    }

    #[test]
    fn failed_and_oversized_evaluations_drain_actions_without_allocating_navigation_ids() {
        let mut handle = spawn_browser(test_config()).unwrap();
        let context_id = create(&mut handle);
        let first_navigation_id =
            dispatch_navigation(&mut handle, context_id, "https://same.test/a");
        wait_for_navigation(&mut handle, context_id, first_navigation_id).unwrap();
        let current = state(&mut handle, context_id);

        let error = handle
            .dispatch(BrowserCommand::EvaluateForAutomation {
                context_id,
                document_id: current.document_id,
                runtime_context_id: current.runtime_context_id.unwrap(),
                source: "location.assign('https://same.test/b'); throw new Error('boom')"
                    .to_owned(),
            })
            .unwrap_err();
        assert_eq!(error.code, crate::engine_error::codes::SCRIPT_EVAL);

        let oversized_source = (0..=MAX_NAVIGATION_ACTIONS_PER_COMMAND)
            .map(|_| "location.assign('https://same.test/b')")
            .collect::<Vec<_>>()
            .join(";");
        let error = handle
            .dispatch(BrowserCommand::EvaluateForAutomation {
                context_id,
                document_id: current.document_id,
                runtime_context_id: current.runtime_context_id.unwrap(),
                source: oversized_source,
            })
            .unwrap_err();
        assert_eq!(error.code, browser_error_codes::INVALID_ARGUMENT);
        assert!(error.message.contains("more than 64 navigation actions"));

        let invalid_late_action = format!(
            "location.assign('https://same.test/b'); history.pushState(null, '', '/{}')",
            "x".repeat(MAX_URL_BYTES)
        );
        let error = handle
            .dispatch(BrowserCommand::EvaluateForAutomation {
                context_id,
                document_id: current.document_id,
                runtime_context_id: current.runtime_context_id.unwrap(),
                source: invalid_late_action,
            })
            .unwrap_err();
        assert_eq!(error.code, browser_error_codes::INVALID_ARGUMENT);
        assert_eq!(state(&mut handle, context_id).url, current.url);

        let result = handle
            .dispatch(BrowserCommand::EvaluateForAutomation {
                context_id,
                document_id: current.document_id,
                runtime_context_id: current.runtime_context_id.unwrap(),
                source: "'clean'".to_owned(),
            })
            .unwrap();
        let BrowserCommandResult::AutomationEvaluation(evaluation) = result else {
            panic!("unexpected clean evaluation result: {result:?}");
        };
        assert!(evaluation.navigation_actions.is_empty());

        let next_navigation_id =
            dispatch_navigation(&mut handle, context_id, "https://same.test/next");
        assert_eq!(next_navigation_id.get(), first_navigation_id.get() + 1);
        wait_for_navigation(&mut handle, context_id, next_navigation_id).unwrap();
    }

    #[test]
    fn accessibility_snapshot_validates_viewport_and_exact_document_generation() {
        let mut handle = spawn_browser(test_config()).unwrap();
        let context_id = create(&mut handle);
        navigate(&mut handle, context_id, "https://same.test/input");
        let current = state(&mut handle, context_id);

        let result = handle
            .dispatch(BrowserCommand::AccessibilitySnapshot {
                context_id,
                document_id: current.document_id,
                viewport: (800, 600),
            })
            .unwrap();
        let BrowserCommandResult::AccessibilitySnapshot(snapshot) = result else {
            panic!("unexpected accessibility result: {result:?}");
        };
        assert_eq!(snapshot.context_id, context_id);
        assert_eq!(snapshot.document_id, current.document_id);
        assert_eq!(snapshot.viewport, (800, 600));
        assert_ne!(snapshot.generation, 0);
        let repeated = handle
            .dispatch(BrowserCommand::AccessibilitySnapshot {
                context_id,
                document_id: current.document_id,
                viewport: (800, 600),
            })
            .unwrap();
        let BrowserCommandResult::AccessibilitySnapshot(repeated) = repeated else {
            panic!("unexpected accessibility result: {repeated:?}");
        };
        assert_eq!(snapshot.generation, repeated.generation);
        let link = snapshot
            .nodes
            .iter()
            .find(|node| node.label == "Go")
            .unwrap();
        assert_eq!(link.role, "link");
        assert!(link.focusable);
        assert!(link.bbox.is_some());

        let invalid = handle
            .dispatch(BrowserCommand::AccessibilitySnapshot {
                context_id,
                document_id: current.document_id,
                viewport: (0, 600),
            })
            .unwrap_err();
        assert_eq!(invalid.code, browser_error_codes::INVALID_ARGUMENT);

        navigate(&mut handle, context_id, "https://same.test/b");
        let stale = handle
            .dispatch(BrowserCommand::AccessibilitySnapshot {
                context_id,
                document_id: current.document_id,
                viewport: (800, 600),
            })
            .unwrap_err();
        assert_eq!(stale.code, browser_error_codes::STALE_DOCUMENT);
    }

    #[test]
    fn host_view_generation_drives_live_focus_visibility_and_input_policy() {
        let mut handle = spawn_browser(test_config()).unwrap();
        let context_id = create(&mut handle);
        navigate(&mut handle, context_id, "https://same.test/input");
        let current = state(&mut handle, context_id);
        assert_eq!(
            eval(
                &mut handle,
                &current,
                "globalThis.visibilityChanges = 0; document.addEventListener('visibilitychange', () => visibilityChanges++); document.hasFocus()"
            ),
            ScriptValue::Bool(true)
        );

        let hidden = HostViewState {
            generation: 1,
            viewport: (640, 360),
            scale_factor: 2.0,
            focused: false,
            visible: false,
            lifecycle: vixen_api::HostLifecycle::Hidden,
        };
        assert!(matches!(
            handle
                .dispatch(BrowserCommand::UpdateHostViewState {
                    context_id,
                    state: hidden,
                })
                .unwrap(),
            BrowserCommandResult::InputDispatched(_)
        ));
        assert_eq!(
            eval(
                &mut handle,
                &current,
                "`${document.visibilityState}:${document.hidden}:${document.hasFocus()}:${visibilityChanges}`"
            ),
            ScriptValue::String("hidden:true:false:1".to_owned())
        );
        let blocked = handle
            .dispatch(BrowserCommand::DispatchKeyEvent {
                context_id,
                document_id: current.document_id,
                runtime_context_id: current.runtime_context_id.unwrap(),
                event_type: "keyDown".to_owned(),
                event: KeyEventData {
                    key: "a".to_owned(),
                    code: "KeyA".to_owned(),
                    text: "a".to_owned(),
                    apply_text: true,
                    ctrl_key: false,
                    shift_key: false,
                    alt_key: false,
                    meta_key: false,
                    repeat: false,
                    location: 0,
                },
            })
            .unwrap_err();
        assert_eq!(blocked.code, browser_error_codes::INVALID_ARGUMENT);
        let stale = handle
            .dispatch(BrowserCommand::UpdateHostViewState {
                context_id,
                state: hidden,
            })
            .unwrap_err();
        assert_eq!(stale.code, browser_error_codes::STALE_HOST_VIEW);

        let resumed = HostViewState {
            generation: 2,
            focused: true,
            visible: true,
            lifecycle: vixen_api::HostLifecycle::Resumed,
            ..hidden
        };
        handle
            .dispatch(BrowserCommand::UpdateHostViewState {
                context_id,
                state: resumed,
            })
            .unwrap();
        assert_eq!(
            eval(
                &mut handle,
                &current,
                "`${document.visibilityState}:${document.hasFocus()}:${visibilityChanges}`"
            ),
            ScriptValue::String("visible:true:2".to_owned())
        );
    }

    #[test]
    fn uncancelled_navigation_keys_scroll_the_root_document() {
        let mut handle = spawn_browser(test_config()).unwrap();
        let context_id = create(&mut handle);
        navigate(&mut handle, context_id, "https://same.test/scroll");
        let current = state(&mut handle, context_id);
        let viewport = (200, 100);
        handle
            .dispatch(BrowserCommand::UpdateHostViewState {
                context_id,
                state: HostViewState {
                    generation: 1,
                    viewport,
                    ..HostViewState::default()
                },
            })
            .unwrap();

        let initial_y = element_y(&mut handle, &current, "#marker", viewport);
        dispatch_test_key(&mut handle, &current, "PageDown", false);
        let paged_y = element_y(&mut handle, &current, "#marker", viewport);
        assert_eq!(initial_y - paged_y, 87.5);

        dispatch_test_key(&mut handle, &current, "PageUp", false);
        assert_eq!(
            element_y(&mut handle, &current, "#marker", viewport),
            paged_y
        );

        dispatch_test_key(&mut handle, &current, "Home", false);
        assert_eq!(
            element_y(&mut handle, &current, "#marker", viewport),
            initial_y
        );

        assert_eq!(
            eval(
                &mut handle,
                &current,
                "document.querySelector('#field').focus(); document.activeElement.id"
            ),
            ScriptValue::String("field".to_owned())
        );
        dispatch_test_key(&mut handle, &current, "End", false);
        assert_eq!(
            element_y(&mut handle, &current, "#marker", viewport),
            initial_y
        );
    }

    #[test]
    fn script_scroll_uses_the_browser_owned_root_offset_and_live_viewport() {
        let mut handle = spawn_browser(test_config()).unwrap();
        let context_id = create(&mut handle);
        navigate(&mut handle, context_id, "https://same.test/scroll");
        let current = state(&mut handle, context_id);
        let viewport = (200, 100);
        handle
            .dispatch(BrowserCommand::UpdateHostViewState {
                context_id,
                state: HostViewState {
                    generation: 1,
                    viewport,
                    ..HostViewState::default()
                },
            })
            .unwrap();

        let initial_y = element_y(&mut handle, &current, "#marker", viewport);
        assert_eq!(
            eval(
                &mut handle,
                &current,
                "scrollTo({ top: 150 }); [innerWidth, innerHeight, scrollY, pageYOffset, document.documentElement.scrollTop].join(':')"
            ),
            ScriptValue::String("200:100:150:150:150".to_owned())
        );
        assert_eq!(
            initial_y - element_y(&mut handle, &current, "#marker", viewport),
            150.0
        );

        assert_eq!(
            eval(
                &mut handle,
                &current,
                "scrollBy(0, 25); document.body.scrollTop = document.body.scrollTop + 10; scrollY"
            ),
            ScriptValue::Int32(185)
        );
        assert_eq!(
            initial_y - element_y(&mut handle, &current, "#marker", viewport),
            185.0
        );
        assert_eq!(
            eval(
                &mut handle,
                &current,
                "scrollTo(0, 1e9); scrollY === pageYOffset && scrollY === document.scrollingElement.scrollTop && scrollY < 1e9"
            ),
            ScriptValue::Bool(true)
        );
    }

    #[test]
    fn text_input_commits_bounded_ime_composition_to_the_focused_control() {
        let mut handle = spawn_browser(test_config()).unwrap();
        let context_id = create(&mut handle);
        navigate(&mut handle, context_id, "https://same.test/input");
        let current = state(&mut handle, context_id);
        assert_eq!(
            eval(
                &mut handle,
                &current,
                "const field = document.querySelector('#name'); field.focus(); globalThis.imeEvents = []; for (const type of ['compositionstart', 'beforeinput', 'input', 'compositionupdate', 'compositionend']) field.addEventListener(type, event => imeEvents.push(type + ':' + (event.data || '') + ':' + Boolean(event.isComposing))); field.value"
            ),
            ScriptValue::String(String::new())
        );

        let composing = vixen_api::AccessibilityTextSelection {
            base_offset: 0,
            extent_offset: 1,
        };
        let result = handle
            .dispatch(BrowserCommand::DispatchTextInput {
                context_id,
                document_id: current.document_id,
                runtime_context_id: current.runtime_context_id.unwrap(),
                state: TextInputState {
                    text: "に".to_owned(),
                    selection: vixen_api::AccessibilityTextSelection {
                        base_offset: 1,
                        extent_offset: 1,
                    },
                    composing: Some(composing),
                },
            })
            .unwrap();
        assert!(matches!(result, BrowserCommandResult::InputDispatched(_)));
        assert_eq!(
            eval(
                &mut handle,
                &current,
                "document.querySelector('#name').value + '|' + imeEvents.join('>')"
            ),
            ScriptValue::String(
                "に|compositionstart::false>beforeinput:に:true>input:に:true>compositionupdate:に:false"
                    .to_owned()
            )
        );

        handle
            .dispatch(BrowserCommand::DispatchTextInput {
                context_id,
                document_id: current.document_id,
                runtime_context_id: current.runtime_context_id.unwrap(),
                state: TextInputState {
                    text: "に".to_owned(),
                    selection: vixen_api::AccessibilityTextSelection {
                        base_offset: 1,
                        extent_offset: 1,
                    },
                    composing: None,
                },
            })
            .unwrap();
        assert_eq!(
            eval(&mut handle, &current, "imeEvents.at(-1)"),
            ScriptValue::String("compositionend:に:false".to_owned())
        );
        let BrowserCommandResult::AccessibilitySnapshot(snapshot) = handle
            .dispatch(BrowserCommand::AccessibilitySnapshot {
                context_id,
                document_id: current.document_id,
                viewport: (800, 600),
            })
            .unwrap()
        else {
            panic!("expected accessibility snapshot");
        };
        let field = snapshot
            .nodes
            .iter()
            .find(|node| node.label == "Name")
            .unwrap();
        assert_eq!(field.value.as_deref(), Some("に"));
        assert_eq!(field.text_selection.unwrap().base_offset, 1);

        let error = handle
            .dispatch(BrowserCommand::DispatchTextInput {
                context_id,
                document_id: current.document_id,
                runtime_context_id: current.runtime_context_id.unwrap(),
                state: TextInputState {
                    text: "x".to_owned(),
                    selection: vixen_api::AccessibilityTextSelection {
                        base_offset: 2,
                        extent_offset: 2,
                    },
                    composing: None,
                },
            })
            .unwrap_err();
        assert_eq!(error.code, browser_error_codes::INVALID_ARGUMENT);
    }

    #[test]
    fn text_input_commits_ime_state_to_a_focused_contenteditable_host() {
        let mut handle = spawn_browser(test_config()).unwrap();
        let context_id = create(&mut handle);
        navigate(&mut handle, context_id, "https://same.test/input");
        let current = state(&mut handle, context_id);
        assert_eq!(
            eval(
                &mut handle,
                &current,
                "const editor = document.querySelector('#editor'); editor.click(); globalThis.editorImeEvents = []; for (const type of ['compositionstart', 'beforeinput', 'input', 'compositionupdate', 'compositionend']) editor.addEventListener(type, event => editorImeEvents.push(type + ':' + (event.data || '') + ':' + Boolean(event.isComposing))); document.activeElement.id"
            ),
            ScriptValue::String("editor".to_owned())
        );
        let BrowserCommandResult::AccessibilitySnapshot(focused) = handle
            .dispatch(BrowserCommand::AccessibilitySnapshot {
                context_id,
                document_id: current.document_id,
                viewport: (800, 600),
            })
            .unwrap()
        else {
            panic!("expected accessibility snapshot");
        };
        let editor = focused
            .nodes
            .iter()
            .find(|node| node.label == "Editor")
            .unwrap();
        assert_eq!(editor.role, "textbox");
        assert_eq!(editor.value.as_deref(), Some("draft"));
        assert!(editor.actions.iter().any(|action| action == "set_value"));
        assert_eq!(
            editor.text_selection,
            Some(vixen_api::AccessibilityTextSelection {
                base_offset: 5,
                extent_offset: 5,
            })
        );

        handle
            .dispatch(BrowserCommand::DispatchTextInput {
                context_id,
                document_id: current.document_id,
                runtime_context_id: current.runtime_context_id.unwrap(),
                state: TextInputState {
                    text: "draft🦊".to_owned(),
                    selection: vixen_api::AccessibilityTextSelection {
                        base_offset: 7,
                        extent_offset: 7,
                    },
                    composing: Some(vixen_api::AccessibilityTextSelection {
                        base_offset: 5,
                        extent_offset: 7,
                    }),
                },
            })
            .unwrap();
        assert_eq!(
            eval(
                &mut handle,
                &current,
                "document.querySelector('#editor').textContent + '|' + editorImeEvents.join('>')"
            ),
            ScriptValue::String(
                "draft🦊|compositionstart::false>beforeinput:🦊:true>input:🦊:true>compositionupdate:🦊:false"
                    .to_owned()
            )
        );
        let BrowserCommandResult::AccessibilitySnapshot(updated) = handle
            .dispatch(BrowserCommand::AccessibilitySnapshot {
                context_id,
                document_id: current.document_id,
                viewport: (800, 600),
            })
            .unwrap()
        else {
            panic!("expected accessibility snapshot");
        };
        let editor = updated
            .nodes
            .iter()
            .find(|node| node.label == "Editor")
            .unwrap();
        assert_eq!(editor.value.as_deref(), Some("draft🦊"));
        assert_eq!(editor.text_selection.unwrap().base_offset, 7);

        handle
            .dispatch(BrowserCommand::DispatchTextInput {
                context_id,
                document_id: current.document_id,
                runtime_context_id: current.runtime_context_id.unwrap(),
                state: TextInputState {
                    text: "draft🦊".to_owned(),
                    selection: vixen_api::AccessibilityTextSelection {
                        base_offset: 7,
                        extent_offset: 7,
                    },
                    composing: None,
                },
            })
            .unwrap();
        assert_eq!(
            eval(&mut handle, &current, "editorImeEvents.at(-1)"),
            ScriptValue::String("compositionend:🦊:false".to_owned())
        );
    }

    #[test]
    fn accessibility_focus_action_is_capability_and_generation_checked() {
        let mut handle = spawn_browser(test_config()).unwrap();
        let context_id = create(&mut handle);
        navigate(&mut handle, context_id, "https://same.test/input");
        let current = state(&mut handle, context_id);
        let result = handle
            .dispatch(BrowserCommand::AccessibilitySnapshot {
                context_id,
                document_id: current.document_id,
                viewport: (800, 600),
            })
            .unwrap();
        let BrowserCommandResult::AccessibilitySnapshot(snapshot) = result else {
            panic!("unexpected accessibility result: {result:?}");
        };
        let link = snapshot
            .nodes
            .iter()
            .find(|node| node.label == "Go")
            .unwrap();
        assert!(link.actions.iter().any(|action| action == "focus"));

        let result = handle
            .dispatch(BrowserCommand::DispatchAccessibilityAction {
                context_id,
                document_id: current.document_id,
                runtime_context_id: current.runtime_context_id.unwrap(),
                viewport: snapshot.viewport,
                source_generation: snapshot.source_generation,
                node_id: link.id,
                action: AccessibilityAction::Focus,
            })
            .unwrap();
        assert!(matches!(result, BrowserCommandResult::InputDispatched(_)));

        let refreshed = handle
            .dispatch(BrowserCommand::AccessibilitySnapshot {
                context_id,
                document_id: current.document_id,
                viewport: snapshot.viewport,
            })
            .unwrap();
        let BrowserCommandResult::AccessibilitySnapshot(refreshed) = refreshed else {
            panic!("unexpected accessibility result: {refreshed:?}");
        };
        assert!(
            refreshed
                .nodes
                .iter()
                .any(|node| node.id == link.id && node.focused)
        );
        assert!(refreshed.source_generation > snapshot.source_generation);

        let stale = handle
            .dispatch(BrowserCommand::DispatchAccessibilityAction {
                context_id,
                document_id: current.document_id,
                runtime_context_id: current.runtime_context_id.unwrap(),
                viewport: snapshot.viewport,
                source_generation: snapshot.source_generation,
                node_id: link.id,
                action: AccessibilityAction::Focus,
            })
            .unwrap_err();
        assert_eq!(stale.code, browser_error_codes::STALE_ACCESSIBILITY);

        let input = refreshed
            .nodes
            .iter()
            .find(|node| node.label == "Name")
            .unwrap();
        let input_id = input.id;
        assert!(input.actions.iter().any(|action| action == "set_value"));
        handle
            .dispatch(BrowserCommand::DispatchAccessibilityAction {
                context_id,
                document_id: current.document_id,
                runtime_context_id: current.runtime_context_id.unwrap(),
                viewport: refreshed.viewport,
                source_generation: refreshed.source_generation,
                node_id: input_id,
                action: AccessibilityAction::Focus,
            })
            .unwrap();
        let input_focused = handle
            .dispatch(BrowserCommand::AccessibilitySnapshot {
                context_id,
                document_id: current.document_id,
                viewport: refreshed.viewport,
            })
            .unwrap();
        let BrowserCommandResult::AccessibilitySnapshot(input_focused) = input_focused else {
            panic!("unexpected accessibility result: {input_focused:?}");
        };
        let result = handle
            .dispatch(BrowserCommand::DispatchAccessibilityAction {
                context_id,
                document_id: current.document_id,
                runtime_context_id: current.runtime_context_id.unwrap(),
                viewport: input_focused.viewport,
                source_generation: input_focused.source_generation,
                node_id: input_id,
                action: AccessibilityAction::SetValue("Ada".to_owned()),
            })
            .unwrap();
        assert!(matches!(result, BrowserCommandResult::InputDispatched(_)));
        let updated = handle
            .dispatch(BrowserCommand::AccessibilitySnapshot {
                context_id,
                document_id: current.document_id,
                viewport: input_focused.viewport,
            })
            .unwrap();
        let BrowserCommandResult::AccessibilitySnapshot(updated) = updated else {
            panic!("unexpected accessibility result: {updated:?}");
        };
        assert!(
            updated
                .nodes
                .iter()
                .any(|node| node.id == input_id && node.value.as_deref() == Some("Ada"))
        );
        assert_eq!(
            updated
                .nodes
                .iter()
                .find(|node| node.id == input_id)
                .and_then(|node| node.text_selection)
                .map(|selection| (selection.base_offset, selection.extent_offset)),
            Some((3, 3))
        );

        let volume = updated
            .nodes
            .iter()
            .find(|node| node.label == "Volume")
            .unwrap();
        let volume_id = volume.id;
        assert_eq!(volume.range.map(|range| range.current), Some(4.0));
        assert!(volume.actions.iter().any(|action| action == "increase"));
        let result = handle
            .dispatch(BrowserCommand::DispatchAccessibilityAction {
                context_id,
                document_id: current.document_id,
                runtime_context_id: current.runtime_context_id.unwrap(),
                viewport: updated.viewport,
                source_generation: updated.source_generation,
                node_id: volume_id,
                action: AccessibilityAction::Increase,
            })
            .unwrap();
        assert!(matches!(result, BrowserCommandResult::InputDispatched(_)));
        let adjusted = handle
            .dispatch(BrowserCommand::AccessibilitySnapshot {
                context_id,
                document_id: current.document_id,
                viewport: updated.viewport,
            })
            .unwrap();
        let BrowserCommandResult::AccessibilitySnapshot(adjusted) = adjusted else {
            panic!("unexpected accessibility result: {adjusted:?}");
        };
        assert_eq!(
            adjusted
                .nodes
                .iter()
                .find(|node| node.id == volume_id)
                .and_then(|node| node.range)
                .map(|range| range.current),
            Some(6.0)
        );

        let brightness = adjusted
            .nodes
            .iter()
            .find(|node| node.label == "Brightness")
            .unwrap();
        assert_eq!(brightness.range.map(|range| range.current), Some(3.0));
        let result = handle
            .dispatch(BrowserCommand::DispatchAccessibilityAction {
                context_id,
                document_id: current.document_id,
                runtime_context_id: current.runtime_context_id.unwrap(),
                viewport: adjusted.viewport,
                source_generation: adjusted.source_generation,
                node_id: brightness.id,
                action: AccessibilityAction::Increase,
            })
            .unwrap();
        assert!(matches!(result, BrowserCommandResult::InputDispatched(_)));
        let authored_adjusted = handle
            .dispatch(BrowserCommand::AccessibilitySnapshot {
                context_id,
                document_id: current.document_id,
                viewport: adjusted.viewport,
            })
            .unwrap();
        let BrowserCommandResult::AccessibilitySnapshot(authored_adjusted) = authored_adjusted
        else {
            panic!("unexpected accessibility result: {authored_adjusted:?}");
        };
        assert_eq!(
            authored_adjusted
                .nodes
                .iter()
                .find(|node| node.label == "Brightness")
                .and_then(|node| node.range)
                .map(|range| range.current),
            Some(4.0)
        );
    }

    #[test]
    fn two_contexts_share_profile_storage_and_isolate_session_state() {
        let config = test_config();
        let profile_path = config.profile_path.clone();
        let mut handle = spawn_browser(config).unwrap();
        let context_a = create(&mut handle);
        let context_b = create(&mut handle);
        navigate(&mut handle, context_a, "https://same.test/a");
        navigate(&mut handle, context_b, "https://same.test/b");

        let state_a = state(&mut handle, context_a);
        let state_b = state(&mut handle, context_b);
        assert_ne!(state_a.document_id, state_b.document_id);
        assert_ne!(state_a.runtime_context_id, state_b.runtime_context_id);
        assert_eq!(
            eval(
                &mut handle,
                &state_a,
                "globalThis.onlyA = 7; sessionStorage.setItem('s', 'A'); localStorage.setItem('shared', 'from-a'); 'ready'",
            ),
            ScriptValue::String("ready".to_owned())
        );
        assert_eq!(
            eval(
                &mut handle,
                &state_b,
                "`${typeof globalThis.onlyA}:${sessionStorage.getItem('s')}:${localStorage.getItem('shared')}`",
            ),
            ScriptValue::String("undefined:null:from-a".to_owned())
        );

        navigate(&mut handle, context_a, "https://same.test/next");
        let next_a = state(&mut handle, context_a);
        let unchanged_b = state(&mut handle, context_b);
        assert_ne!(next_a.document_id, state_a.document_id);
        assert_ne!(next_a.runtime_context_id, state_a.runtime_context_id);
        assert!(next_a.can_go_back);
        assert_eq!(next_a.history_length, state_a.history_length + 1);
        assert_eq!(unchanged_b.document_id, state_b.document_id);
        assert_eq!(unchanged_b.history_length, state_b.history_length);

        let stale = handle
            .dispatch(BrowserCommand::Evaluate {
                context_id: context_a,
                document_id: state_a.document_id,
                runtime_context_id: state_a.runtime_context_id.unwrap(),
                source: "1".to_owned(),
            })
            .unwrap_err();
        assert_eq!(stale.code, browser_error_codes::STALE_DOCUMENT);

        handle
            .dispatch(BrowserCommand::CloseBrowsingContext {
                context_id: context_a,
            })
            .unwrap();
        let closed = handle
            .dispatch(BrowserCommand::GetBrowsingContextState {
                context_id: context_a,
            })
            .unwrap_err();
        assert_eq!(closed.code, browser_error_codes::STALE_CONTEXT);
        assert_eq!(
            state(&mut handle, context_b).document_id,
            state_b.document_id
        );

        drop(handle);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn context_runtime_uses_browser_network_policy() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
        });
        let mut config = test_config();
        config
            .network
            .dns_overrides
            .push(("browser-core-vixen.com".to_owned(), vec![address]));
        let profile_path = config.profile_path.clone();
        let mut handle = spawn_browser(config).unwrap();
        let context_id = create(&mut handle);
        let state = state(&mut handle, context_id);
        assert_eq!(
            eval(
                &mut handle,
                &state,
                &format!(
                    "fetch('http://browser-core-vixen.com:{}/').then(response => response.text())",
                    address.port()
                ),
            ),
            ScriptValue::String("ok".to_owned())
        );
        server.join().unwrap();
        drop(handle);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn bounded_event_queue_reports_lag_instead_of_blocking() {
        let mut config = test_config();
        let profile_path = config.profile_path.clone();
        config.event_capacity = 1;
        let mut handle = spawn_browser(config).unwrap();
        create(&mut handle);
        let error = handle.try_next_event().unwrap_err();
        assert_eq!(error.code, browser_error_codes::EVENT_LAGGED);
        assert!(handle.try_next_event().unwrap().is_some());
        drop(handle);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn navigation_emits_ordered_phase_sequence() {
        let config = test_config();
        let profile_path = config.profile_path.clone();
        let mut handle = spawn_browser(config).unwrap();
        let context_id = create(&mut handle);
        drain_events(&mut handle);

        let navigation_id = dispatch_navigation(&mut handle, context_id, "https://same.test/a");
        let events = wait_for_navigation(&mut handle, context_id, navigation_id).unwrap();

        assert_eq!(
            phases_for(&events, navigation_id),
            vec![
                NavigationPhase::Intent,
                NavigationPhase::Policy,
                NavigationPhase::Request,
                NavigationPhase::Response,
                NavigationPhase::Commit,
                NavigationPhase::Parse,
                NavigationPhase::ScriptsAndSubresources,
                NavigationPhase::DomContentLoaded,
                NavigationPhase::Load,
                NavigationPhase::Settled,
            ]
        );
        let parse_event = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    BrowserEvent::NavigationPhaseChanged {
                        navigation_id: event_navigation_id,
                        phase: NavigationPhase::Parse,
                        ..
                    } if *event_navigation_id == navigation_id
                )
            })
            .expect("parse phase event");
        let committed_event = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    BrowserEvent::NavigationCommitted {
                        navigation_id: event_navigation_id,
                        ..
                    } if *event_navigation_id == navigation_id
                )
            })
            .expect("navigation committed event");
        let scripts_event = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    BrowserEvent::NavigationPhaseChanged {
                        navigation_id: event_navigation_id,
                        phase: NavigationPhase::ScriptsAndSubresources,
                        ..
                    } if *event_navigation_id == navigation_id
                )
            })
            .expect("scripts phase event");
        assert!(parse_event < committed_event);
        assert!(committed_event < scripts_event);
        assert_exactly_one_terminal_phase(&events, navigation_id);

        drop(handle);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn oversized_file_navigation_fails_without_committing() {
        let serial = NEXT_TEST_PROFILE.fetch_add(1, Ordering::Relaxed);
        let file_path = std::env::temp_dir().join(format!(
            "vixen-oversized-navigation-{}-{serial}.html",
            std::process::id()
        ));
        std::fs::write(&file_path, "x".repeat(64)).unwrap();
        let file_url = url::Url::from_file_path(&file_path).unwrap().to_string();
        let mut config = test_config();
        config.network.max_body_bytes = 32;
        let profile_path = config.profile_path.clone();
        let mut handle = spawn_browser(config).unwrap();
        let context_id = create(&mut handle);
        drain_events(&mut handle);
        let initial = state(&mut handle, context_id);

        let navigation_id = dispatch_navigation(&mut handle, context_id, &file_url);
        let error = wait_for_navigation(&mut handle, context_id, navigation_id).unwrap_err();
        let after = state(&mut handle, context_id);

        assert_eq!(error.code, browser_error_codes::NAVIGATION_LOAD);
        assert!(error.message.contains("exceeds 32 bytes"));
        assert_eq!(after.document_id, initial.document_id);
        assert_eq!(after.runtime_context_id, initial.runtime_context_id);
        assert_eq!(after.url, initial.url);
        assert_eq!(after.active_navigation_id, None);

        drop(handle);
        let _ = std::fs::remove_file(profile_path);
        let _ = std::fs::remove_file(file_path);
    }

    #[test]
    fn stop_during_parse_cancels_without_commit() {
        let (mut core, events, command_rx, profile_path) = direct_core();
        let context_id = direct_create(&mut core);
        let initial = core.context_state(context_id).unwrap();
        let initial_history = core
            .context(context_id)
            .unwrap()
            .page
            .session_history()
            .clone();
        drain_direct_events(&events);

        let navigation_id =
            direct_begin_navigation(&mut core, context_id, "https://same.test/parser-stop");
        direct_complete_source(
            &mut core,
            context_id,
            navigation_id,
            "https://same.test/parser-stop",
            large_parser_document("Parser stop"),
        );
        assert!(core.advance_navigation_work());
        assert_parser_is_pending(&core, context_id, navigation_id);

        assert_eq!(
            core.stop(context_id).unwrap(),
            BrowserCommandResult::Accepted
        );
        assert!(core.advance_navigation_work());
        let events = drain_direct_events(&events);
        let stopped = core.context_state(context_id).unwrap();

        assert_eq!(
            phases_for(&events, navigation_id),
            vec![
                NavigationPhase::Intent,
                NavigationPhase::Policy,
                NavigationPhase::Request,
                NavigationPhase::Response,
                NavigationPhase::Commit,
                NavigationPhase::Parse,
                NavigationPhase::Cancelled,
            ]
        );
        assert_navigation_cancelled(
            &events,
            navigation_id,
            NavigationCancellationReason::Stopped,
        );
        assert_no_terminal_success(&events, navigation_id);
        assert_eq!(stopped.document_id, initial.document_id);
        assert_eq!(stopped.runtime_context_id, initial.runtime_context_id);
        assert_eq!(
            core.context(context_id).unwrap().page.session_history(),
            &initial_history
        );

        core.start_pending_loads();
        drop(core);
        drop(command_rx);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn reload_during_parse_supersedes_parser_and_preserves_history() {
        let (mut core, events, command_rx, profile_path) = direct_core();
        let context_id = direct_create(&mut core);
        direct_navigate(
            &mut core,
            context_id,
            "https://same.test/a",
            "<!doctype html><title>A</title><main>A</main>",
        );
        let stable = core.context_state(context_id).unwrap();
        let stable_history = core
            .context(context_id)
            .unwrap()
            .page
            .session_history()
            .clone();
        drain_direct_events(&events);

        let parser_navigation =
            direct_begin_navigation(&mut core, context_id, "https://same.test/parser-reload");
        direct_complete_source(
            &mut core,
            context_id,
            parser_navigation,
            "https://same.test/parser-reload",
            large_parser_document("Parser reload"),
        );
        assert!(core.advance_navigation_work());
        assert_parser_is_pending(&core, context_id, parser_navigation);

        let reload_navigation = match core
            .dispatch(BrowserCommand::Reload { context_id })
            .unwrap()
        {
            BrowserCommandResult::NavigationAccepted { navigation_id } => navigation_id,
            other => panic!("unexpected reload result: {other:?}"),
        };
        direct_complete_source(
            &mut core,
            context_id,
            reload_navigation,
            "https://same.test/a",
            "<!doctype html><title>A</title><main>A</main>".to_owned(),
        );
        direct_drive_navigation(&mut core, context_id, reload_navigation);
        let events = drain_direct_events(&events);
        let reloaded = core.context_state(context_id).unwrap();

        assert_eq!(
            phases_for(&events, parser_navigation),
            vec![
                NavigationPhase::Intent,
                NavigationPhase::Policy,
                NavigationPhase::Request,
                NavigationPhase::Response,
                NavigationPhase::Commit,
                NavigationPhase::Parse,
                NavigationPhase::Cancelled,
            ]
        );
        assert_navigation_cancelled(
            &events,
            parser_navigation,
            NavigationCancellationReason::Superseded,
        );
        assert_no_terminal_success(&events, parser_navigation);
        assert_eq!(
            phases_for(&events, reload_navigation),
            vec![
                NavigationPhase::Intent,
                NavigationPhase::Policy,
                NavigationPhase::Request,
                NavigationPhase::Response,
                NavigationPhase::Commit,
                NavigationPhase::Parse,
                NavigationPhase::ScriptsAndSubresources,
                NavigationPhase::DomContentLoaded,
                NavigationPhase::Load,
                NavigationPhase::Settled,
            ]
        );
        assert_exactly_one_terminal_phase(&events, reload_navigation);
        assert_ne!(reloaded.document_id, stable.document_id);
        assert_ne!(reloaded.runtime_context_id, stable.runtime_context_id);
        assert_eq!(reloaded.url, "https://same.test/a");
        assert_eq!(reloaded.title.as_deref(), Some("A"));
        assert_eq!(
            core.context(context_id).unwrap().page.session_history(),
            &stable_history
        );

        core.start_pending_loads();
        drop(core);
        drop(command_rx);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn history_traversal_during_parse_supersedes_parser() {
        let (mut core, events, command_rx, profile_path) = direct_core();
        let context_id = direct_create(&mut core);
        direct_navigate(
            &mut core,
            context_id,
            "https://same.test/a",
            "<!doctype html><title>A</title><main>A</main>",
        );
        direct_navigate(
            &mut core,
            context_id,
            "https://same.test/b",
            "<!doctype html><title>B</title><main>B</main>",
        );
        let before = core.context_state(context_id).unwrap();
        assert_eq!(before.history_length, 2);
        assert_eq!(before.history_index, 1);
        drain_direct_events(&events);

        let parser_navigation =
            direct_begin_navigation(&mut core, context_id, "https://same.test/parser-history");
        direct_complete_source(
            &mut core,
            context_id,
            parser_navigation,
            "https://same.test/parser-history",
            large_parser_document("Parser history"),
        );
        assert!(core.advance_navigation_work());
        assert_parser_is_pending(&core, context_id, parser_navigation);

        let history_navigation = match core
            .dispatch(BrowserCommand::TraverseHistory {
                context_id,
                delta: -1,
            })
            .unwrap()
        {
            BrowserCommandResult::NavigationAccepted { navigation_id } => navigation_id,
            other => panic!("unexpected history traversal result: {other:?}"),
        };
        direct_complete_source(
            &mut core,
            context_id,
            history_navigation,
            "https://same.test/a",
            "<!doctype html><title>A</title><main>A</main>".to_owned(),
        );
        direct_drive_navigation(&mut core, context_id, history_navigation);
        let events = drain_direct_events(&events);
        let traversed = core.context_state(context_id).unwrap();
        let traversed_history = core.context(context_id).unwrap().page.session_history();

        assert_eq!(
            phases_for(&events, parser_navigation),
            vec![
                NavigationPhase::Intent,
                NavigationPhase::Policy,
                NavigationPhase::Request,
                NavigationPhase::Response,
                NavigationPhase::Commit,
                NavigationPhase::Parse,
                NavigationPhase::Cancelled,
            ]
        );
        assert_navigation_cancelled(
            &events,
            parser_navigation,
            NavigationCancellationReason::Superseded,
        );
        assert_no_terminal_success(&events, parser_navigation);
        assert_eq!(
            phases_for(&events, history_navigation),
            vec![
                NavigationPhase::Intent,
                NavigationPhase::Policy,
                NavigationPhase::Request,
                NavigationPhase::Response,
                NavigationPhase::Commit,
                NavigationPhase::Parse,
                NavigationPhase::ScriptsAndSubresources,
                NavigationPhase::DomContentLoaded,
                NavigationPhase::Load,
                NavigationPhase::Settled,
            ]
        );
        assert_exactly_one_terminal_phase(&events, history_navigation);
        assert_eq!(traversed.url, "https://same.test/a");
        assert_eq!(traversed.title.as_deref(), Some("A"));
        assert_eq!(traversed.history_length, 2);
        assert_eq!(traversed.history_index, 0);
        assert!(traversed.can_go_forward);
        assert_eq!(traversed_history.entries()[0].url, "https://same.test/a");
        assert_eq!(traversed_history.entries()[1].url, "https://same.test/b");

        core.start_pending_loads();
        drop(core);
        drop(command_rx);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn redirect_navigation_emits_redirect_event_and_commits_final_url() {
        let server = GatedHttpServer::start(2);
        let mut config = test_config();
        server.configure(&mut config);
        let profile_path = config.profile_path.clone();
        let mut handle = spawn_browser(config).unwrap();
        let context_id = create(&mut handle);
        drain_events(&mut handle);

        let redirect_url = server.url("/redirect");
        let final_url = server.url("/final");
        let navigation_id = dispatch_navigation(&mut handle, context_id, &redirect_url);
        let request_redirect = server.request();
        assert_eq!(request_redirect.path, "/redirect");
        request_redirect
            .respond
            .send(redirect_response(&final_url))
            .unwrap();
        request_redirect
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("redirect response watchdog");

        let request_final = server.request();
        assert_eq!(request_final.path, "/final");
        let mut events = Vec::new();
        let redirect = loop {
            let event = handle
                .wait_next_event(Duration::from_secs(10))
                .unwrap()
                .expect("redirect event before final response");
            let redirected = matches!(
                &event,
                BrowserEvent::NavigationRedirected {
                    navigation_id: event_navigation_id,
                    from_url,
                    to_url,
                    status: 302,
                    ..
                } if *event_navigation_id == navigation_id
                    && from_url == &redirect_url
                    && to_url == &final_url
            );
            events.push(event.clone());
            if redirected {
                break event;
            }
        };
        assert!(matches!(
            redirect,
            BrowserEvent::NavigationRedirected { .. }
        ));
        assert!(!events.iter().any(|event| matches!(
            event,
            BrowserEvent::NavigationPhaseChanged {
                navigation_id: event_navigation_id,
                phase: NavigationPhase::Response,
                ..
            } if *event_navigation_id == navigation_id
        )));
        request_final
            .respond
            .send("<!doctype html><title>Redirected</title><main>final</main>".to_owned())
            .unwrap();
        request_final
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("final response watchdog");

        events.extend(wait_for_navigation(&mut handle, context_id, navigation_id).unwrap());
        let state = state(&mut handle, context_id);
        assert_eq!(state.url, final_url);
        assert_eq!(state.title.as_deref(), Some("Redirected"));
        assert_eq!(history(&mut handle, context_id).entries[0].url, final_url);

        let redirect_index = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    BrowserEvent::NavigationRedirected {
                        navigation_id: event_navigation_id,
                        from_url,
                        to_url,
                        status: 302,
                        ..
                    } if *event_navigation_id == navigation_id
                        && from_url == &redirect_url
                        && to_url == &final_url
                )
            })
            .expect("redirect event");
        let response_index = phases_for(&events, navigation_id)
            .iter()
            .position(|phase| *phase == NavigationPhase::Response)
            .expect("response phase");
        let phase_event_index = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    BrowserEvent::NavigationPhaseChanged {
                        navigation_id: event_navigation_id,
                        phase: NavigationPhase::Response,
                        ..
                    } if *event_navigation_id == navigation_id
                )
            })
            .expect("response phase event");
        assert!(redirect_index < phase_event_index);
        assert_eq!(response_index, 3);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    BrowserEvent::NavigationRedirected {
                        navigation_id: event_navigation_id,
                        ..
                    } if *event_navigation_id == navigation_id
                ))
                .count(),
            1
        );

        server.join();
        drop(handle);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn newer_navigation_wins_when_superseded_source_responds_late() {
        let server = GatedHttpServer::start(3);
        let mut config = test_config();
        server.configure(&mut config);
        let profile_path = config.profile_path.clone();
        let mut handle = spawn_browser(config).unwrap();
        let context_id = create(&mut handle);
        drain_events(&mut handle);
        let initial = state(&mut handle, context_id);

        let navigation_a = dispatch_navigation(&mut handle, context_id, &server.url("/a"));
        let request_a = server.request();
        assert_eq!(request_a.path, "/a");
        let loading_a = state(&mut handle, context_id);
        assert_eq!(loading_a.document_id, initial.document_id);
        assert_eq!(loading_a.active_navigation_id, Some(navigation_a));

        let navigation_b = dispatch_navigation(&mut handle, context_id, &server.url("/b"));
        let request_b = server.request();
        assert_eq!(request_b.path, "/b");
        request_b
            .respond
            .send("<!doctype html><title>B</title><main>B</main>".to_owned())
            .unwrap();
        request_b
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("B response watchdog");
        let mut events = wait_for_navigation(&mut handle, context_id, navigation_b).unwrap();
        events.extend(drain_events(&mut handle));
        let committed = state(&mut handle, context_id);
        assert_eq!(committed.title.as_deref(), Some("B"));
        assert_eq!(committed.active_navigation_id, None);

        inject_source_progress(
            &handle,
            context_id,
            navigation_a,
            NetworkEvent::Redirect {
                from: server.url("/a"),
                to: server.url("/stale-redirect"),
                status: 302,
            },
        );
        let after_stale_progress = state(&mut handle, context_id);
        events.extend(drain_events(&mut handle));
        assert_eq!(after_stale_progress.document_id, committed.document_id);
        assert!(!events.iter().any(|event| matches!(
            event,
            BrowserEvent::NavigationRedirected {
                navigation_id: event_navigation_id,
                ..
            } if *event_navigation_id == navigation_a
        )));

        request_a
            .respond
            .send("<!doctype html><title>A</title><main>A</main>".to_owned())
            .unwrap();
        request_a
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("late A response watchdog");
        inject_late_source_completion(
            &handle,
            context_id,
            navigation_a,
            server.url("/a"),
            "Injected stale A",
            Some("source=a"),
        );
        let after_late_a = state(&mut handle, context_id);
        events.extend(drain_events(&mut handle));
        assert_eq!(after_late_a.document_id, committed.document_id);
        assert_eq!(after_late_a.title.as_deref(), Some("B"));

        let navigation_probe = dispatch_navigation(&mut handle, context_id, &server.url("/probe"));
        let request_probe = server.request();
        assert_eq!(request_probe.path, "/probe");
        let probe_cookie = request_probe.cookie.as_deref().unwrap_or_default();
        assert!(probe_cookie.contains("source=b"));
        assert!(!probe_cookie.contains("source=a"));
        request_probe
            .respond
            .send("<!doctype html><title>Probe</title>".to_owned())
            .unwrap();
        request_probe
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("probe response watchdog");
        events.extend(wait_for_navigation(&mut handle, context_id, navigation_probe).unwrap());
        assert!(events.iter().any(|event| matches!(
            event,
            BrowserEvent::NavigationCancelled {
                navigation_id,
                reason: NavigationCancellationReason::Superseded,
                ..
            } if *navigation_id == navigation_a
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            BrowserEvent::NavigationCommitted { navigation_id, .. }
                | BrowserEvent::NavigationFailed { navigation_id, .. }
                | BrowserEvent::DomContentLoaded { navigation_id, .. }
                | BrowserEvent::DocumentLoadCompleted { navigation_id, .. }
                if *navigation_id == navigation_a
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            BrowserEvent::NavigationPhaseChanged {
                navigation_id,
                phase: NavigationPhase::Settled | NavigationPhase::Failed,
                ..
            } if *navigation_id == navigation_a
        )));

        server.join();
        drop(handle);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn reload_supersedes_active_navigation_and_rejects_late_completion() {
        let server = GatedHttpServer::start(2);
        let mut config = test_config();
        server.configure(&mut config);
        let profile_path = config.profile_path.clone();
        let mut handle = spawn_browser(config).unwrap();
        let context_id = create(&mut handle);
        drain_events(&mut handle);
        navigate(&mut handle, context_id, "https://same.test/a");
        let stable = state(&mut handle, context_id);
        let stable_history = history(&mut handle, context_id);
        drain_events(&mut handle);

        let slow_navigation = dispatch_navigation(&mut handle, context_id, &server.url("/slow"));
        let slow_request = server.request();
        assert_eq!(slow_request.path, "/slow");
        assert_eq!(
            state(&mut handle, context_id).active_navigation_id,
            Some(slow_navigation)
        );

        let reload_navigation = reload(&mut handle, context_id);
        let mut events = wait_for_navigation(&mut handle, context_id, reload_navigation).unwrap();
        events.extend(drain_events(&mut handle));
        let reloaded = state(&mut handle, context_id);
        assert_eq!(reloaded.url, "https://same.test/a");
        assert_eq!(reloaded.title.as_deref(), Some("A"));
        assert_eq!(reloaded.active_navigation_id, None);
        assert_eq!(history(&mut handle, context_id), stable_history);

        slow_request
            .respond
            .send("<!doctype html><title>Slow stale</title>".to_owned())
            .unwrap();
        slow_request
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("slow response watchdog");
        inject_late_source_completion(
            &handle,
            context_id,
            slow_navigation,
            server.url("/slow"),
            "Injected stale slow",
            Some("source=slow"),
        );
        events.extend(drain_events(&mut handle));
        let after_late = state(&mut handle, context_id);
        assert_eq!(after_late.document_id, reloaded.document_id);
        assert_eq!(after_late.title.as_deref(), Some("A"));
        assert_eq!(history(&mut handle, context_id), stable_history);

        let probe_navigation = dispatch_navigation(&mut handle, context_id, &server.url("/probe"));
        let probe_request = server.request();
        assert_eq!(probe_request.path, "/probe");
        assert!(
            !probe_request
                .cookie
                .as_deref()
                .unwrap_or_default()
                .contains("source=slow")
        );
        probe_request
            .respond
            .send("<!doctype html><title>Probe</title>".to_owned())
            .unwrap();
        probe_request
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("probe response watchdog");
        events.extend(wait_for_navigation(&mut handle, context_id, probe_navigation).unwrap());
        assert_navigation_cancelled(
            &events,
            slow_navigation,
            NavigationCancellationReason::Superseded,
        );
        assert_no_terminal_success(&events, slow_navigation);
        assert_ne!(stable.document_id, reloaded.document_id);

        server.join();
        drop(handle);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn history_traversal_supersedes_active_navigation_and_rejects_late_completion() {
        let server = GatedHttpServer::start(2);
        let mut config = test_config();
        server.configure(&mut config);
        let profile_path = config.profile_path.clone();
        let mut handle = spawn_browser(config).unwrap();
        let context_id = create(&mut handle);
        drain_events(&mut handle);
        navigate(&mut handle, context_id, "https://same.test/a");
        navigate(&mut handle, context_id, "https://same.test/b");
        let before = state(&mut handle, context_id);
        assert_eq!(before.history_length, 2);
        assert_eq!(before.history_index, 1);
        assert!(before.can_go_back);
        drain_events(&mut handle);

        let slow_navigation = dispatch_navigation(&mut handle, context_id, &server.url("/c"));
        let slow_request = server.request();
        assert_eq!(slow_request.path, "/c");
        assert_eq!(
            state(&mut handle, context_id).active_navigation_id,
            Some(slow_navigation)
        );

        let history_navigation = traverse_history(&mut handle, context_id, -1)
            .expect("history traversal should produce a document navigation");
        let mut events = wait_for_navigation(&mut handle, context_id, history_navigation).unwrap();
        events.extend(drain_events(&mut handle));
        let after_traverse = state(&mut handle, context_id);
        assert_eq!(after_traverse.url, "https://same.test/a");
        assert_eq!(after_traverse.title.as_deref(), Some("A"));
        assert_eq!(after_traverse.history_length, 2);
        assert_eq!(after_traverse.history_index, 0);
        assert!(after_traverse.can_go_forward);
        let traversed_history = history(&mut handle, context_id);
        assert_eq!(traversed_history.entries[0].url, "https://same.test/a");
        assert_eq!(traversed_history.entries[1].url, "https://same.test/b");

        slow_request
            .respond
            .send("<!doctype html><title>Stale C</title>".to_owned())
            .unwrap();
        slow_request
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("slow C response watchdog");
        inject_late_source_completion(
            &handle,
            context_id,
            slow_navigation,
            server.url("/c"),
            "Injected stale C",
            Some("source=c"),
        );
        events.extend(drain_events(&mut handle));
        let after_late = state(&mut handle, context_id);
        assert_eq!(after_late.document_id, after_traverse.document_id);
        assert_eq!(after_late.title.as_deref(), Some("A"));
        assert_eq!(history(&mut handle, context_id), traversed_history);

        let probe_navigation = dispatch_navigation(&mut handle, context_id, &server.url("/probe"));
        let probe_request = server.request();
        assert_eq!(probe_request.path, "/probe");
        assert!(
            !probe_request
                .cookie
                .as_deref()
                .unwrap_or_default()
                .contains("source=c")
        );
        probe_request
            .respond
            .send("<!doctype html><title>Probe</title>".to_owned())
            .unwrap();
        probe_request
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("probe response watchdog");
        events.extend(wait_for_navigation(&mut handle, context_id, probe_navigation).unwrap());
        assert_navigation_cancelled(
            &events,
            slow_navigation,
            NavigationCancellationReason::Superseded,
        );
        assert_no_terminal_success(&events, slow_navigation);

        server.join();
        drop(handle);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn configured_and_author_scripts_advance_in_order() {
        let (mut core, events, command_rx, profile_path) = direct_core();
        let context_id = direct_create(&mut core);
        let mut config = core.context(context_id).unwrap().config.clone();
        config.preload_scripts = vec!["document.title = 'preload'".to_owned()];
        config.new_document_scripts = vec!["document.title += ':new-document'".to_owned()];
        core.configure_context(context_id, config).unwrap();
        drain_direct_events(&events);

        let navigation_id =
            direct_begin_navigation(&mut core, context_id, "https://same.test/script-order");
        direct_complete_source(
            &mut core,
            context_id,
            navigation_id,
            "https://same.test/script-order",
            "<!doctype html><title>initial</title><script>document.title += ':author'</script>"
                .to_owned(),
        );
        direct_drive_to_scripts(&mut core, context_id, navigation_id);

        assert!(core.advance_navigation_work());
        assert_eq!(
            core.context_state(context_id).unwrap().title.as_deref(),
            Some("preload")
        );
        assert!(core.advance_navigation_work());
        assert_eq!(
            core.context_state(context_id).unwrap().title.as_deref(),
            Some("preload:new-document")
        );
        assert!(core.advance_navigation_work());
        assert_eq!(
            core.context_state(context_id).unwrap().title.as_deref(),
            Some("preload:new-document:author")
        );
        assert!(core.advance_navigation_work());
        assert!(!drain_direct_events(&events).iter().any(|event| matches!(
            event,
            BrowserEvent::DomContentLoaded { navigation_id: event_navigation_id, .. }
                if *event_navigation_id == navigation_id
        )));

        direct_drive_navigation(&mut core, context_id, navigation_id);
        let events = drain_direct_events(&events);
        assert_eq!(
            phases_for(&events, navigation_id),
            vec![
                NavigationPhase::DomContentLoaded,
                NavigationPhase::Load,
                NavigationPhase::Settled,
            ]
        );
        assert_exactly_one_terminal_phase(&events, navigation_id);

        drop(core);
        drop(command_rx);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn gated_external_script_executes_in_order_and_navigation_settles() {
        let server = GatedHttpServer::start(1);
        let mut config = test_config();
        server.configure(&mut config);
        let page_url = "http://same.test/gated-script-success";
        config.document_overrides.insert(
            page_url.to_owned(),
            format!(
                "<!doctype html><title>initial</title><script>globalThis.order = 'before'</script><script src='{}'></script><script>globalThis.order += ':after'</script>",
                server.url("/ordered.js")
            ),
        );
        let profile_path = config.profile_path.clone();
        let mut handle = spawn_browser(config).unwrap();
        let context_id = create(&mut handle);
        drain_events(&mut handle);

        let navigation_id = dispatch_navigation(&mut handle, context_id, page_url);
        let request = server.request();
        assert_eq!(request.path, "/ordered.js");
        let pending = state(&mut handle, context_id);
        assert_eq!(pending.active_navigation_id, Some(navigation_id));
        assert_eq!(pending.title.as_deref(), Some("initial"));

        request
            .respond
            .send(script_response(
                "globalThis.order += ':external'; document.title = 'external'",
                None,
            ))
            .unwrap();
        request
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("external script response watchdog");
        let events = wait_for_navigation(&mut handle, context_id, navigation_id).unwrap();
        let settled = state(&mut handle, context_id);

        assert_eq!(settled.title.as_deref(), Some("external"));
        assert_eq!(
            eval(&mut handle, &settled, "order"),
            ScriptValue::String("before:external:after".to_owned())
        );
        assert_exactly_one_terminal_phase(&events, navigation_id);

        server.join();
        drop(handle);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn active_mixed_content_script_is_blocked_before_io() {
        let server = GatedHttpServer::start(1);
        let mut config = test_config();
        server.configure(&mut config);
        let page_url = "https://same.test/mixed-script";
        config.document_overrides.insert(
            page_url.to_owned(),
            format!(
                "<!doctype html><title>secure</title><script src='{}'></script>",
                server.url("/mixed.js")
            ),
        );
        let profile_path = config.profile_path.clone();
        let mut handle = spawn_browser(config).unwrap();
        let context_id = create(&mut handle);
        drain_events(&mut handle);

        let navigation_id = dispatch_navigation(&mut handle, context_id, page_url);
        let events = wait_for_navigation(&mut handle, context_id, navigation_id).unwrap();
        let settled = state(&mut handle, context_id);
        assert_eq!(settled.title.as_deref(), Some("secure"));
        assert_eq!(
            eval(&mut handle, &settled, "typeof mixedScriptRan"),
            ScriptValue::String("undefined".to_owned())
        );
        assert_exactly_one_terminal_phase(&events, navigation_id);

        let probe_navigation = dispatch_navigation(&mut handle, context_id, &server.url("/probe"));
        let probe = server.request();
        assert_eq!(probe.path, "/probe");
        probe
            .respond
            .send("<!doctype html><title>Probe</title>".to_owned())
            .unwrap();
        probe
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("mixed-content probe watchdog");
        wait_for_navigation(&mut handle, context_id, probe_navigation).unwrap();

        server.join();
        drop(handle);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn external_script_shares_and_persists_profile_cookies() {
        let server = GatedHttpServer::start(1);
        let mut config = test_config();
        server.configure(&mut config);
        let seed_url = server.url("/cookie-seed");
        let script_url = server.url("/cookie-page");
        let read_url = server.url("/cookie-read");
        config.document_overrides.insert(
            seed_url.clone(),
            "<!doctype html><title>seed</title>".to_owned(),
        );
        config.document_overrides.insert(
            script_url.clone(),
            "<!doctype html><title>cookies</title><script src='/cookie.js'></script>".to_owned(),
        );
        config.document_overrides.insert(
            read_url.clone(),
            "<!doctype html><title>read</title>".to_owned(),
        );
        let profile_path = config.profile_path.clone();
        let mut handle = spawn_browser(config).unwrap();
        let seed_context = create(&mut handle);
        let script_context = create(&mut handle);
        drain_events(&mut handle);
        navigate(&mut handle, seed_context, &seed_url);
        let seed_state = state(&mut handle, seed_context);
        assert_eq!(
            eval(
                &mut handle,
                &seed_state,
                "document.cookie = 'profile_cookie=from-other-context; Path=/; SameSite=Strict'; true"
            ),
            ScriptValue::Bool(true)
        );

        let navigation_id = dispatch_navigation(&mut handle, script_context, &script_url);
        let request = server.request();
        assert_eq!(request.path, "/cookie.js");
        assert!(
            request
                .cookie
                .as_deref()
                .unwrap_or_default()
                .contains("profile_cookie=from-other-context")
        );
        request
            .respond
            .send(script_response(
                "globalThis.cookieSeenDuringExternal = document.cookie",
                Some("accepted_cookie=survives; Path=/"),
            ))
            .unwrap();
        request
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("cookie-sharing external script watchdog");
        let events = wait_for_navigation(&mut handle, script_context, navigation_id).unwrap();
        let settled = state(&mut handle, script_context);

        assert_eq!(
            eval(
                &mut handle,
                &settled,
                "cookieSeenDuringExternal.includes('accepted_cookie=survives')"
            ),
            ScriptValue::Bool(true)
        );
        assert_eq!(
            eval(
                &mut handle,
                &settled,
                "document.cookie.includes('accepted_cookie=survives')"
            ),
            ScriptValue::Bool(true)
        );
        assert_exactly_one_terminal_phase(&events, navigation_id);

        let read_context = create(&mut handle);
        navigate(&mut handle, read_context, &read_url);
        let read_state = state(&mut handle, read_context);
        assert_eq!(
            eval(
                &mut handle,
                &read_state,
                "document.cookie.includes('accepted_cookie=survives')"
            ),
            ScriptValue::Bool(true)
        );

        server.join();
        drop(handle);

        let mut reopened_config = BrowserConfig::new(profile_path.clone());
        reopened_config.document_overrides.insert(
            read_url.clone(),
            "<!doctype html><title>reopened</title>".to_owned(),
        );
        let mut reopened = spawn_browser(reopened_config).unwrap();
        let reopened_context = create(&mut reopened);
        navigate(&mut reopened, reopened_context, &read_url);
        let reopened_state = state(&mut reopened, reopened_context);
        assert_eq!(
            eval(
                &mut reopened,
                &reopened_state,
                "document.cookie.includes('accepted_cookie=survives')"
            ),
            ScriptValue::Bool(true)
        );
        drop(reopened);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn profile_cookie_persistence_merges_stale_worker_deltas() {
        let (core, _events, command_rx, profile_path) = direct_core();
        let url = url::Url::parse("http://profile-cookie-vixen.com/script.js").unwrap();
        let baseline = Vec::new();
        let mut first = CookieJar::from_snapshots(baseline.clone());
        first.set_cookie("first=one; Path=/", &url, true).unwrap();
        let mut second = CookieJar::from_snapshots(baseline.clone());
        second.set_cookie("second=two; Path=/", &url, true).unwrap();

        persist_profile_cookies(
            &core.store,
            std::slice::from_ref(&url),
            &first.delta_from_snapshots(&baseline),
        )
        .unwrap();
        persist_profile_cookies(
            &core.store,
            std::slice::from_ref(&url),
            &second.delta_from_snapshots(&baseline),
        )
        .unwrap();

        let mut merged = CookieJar::default();
        let mut profile_baseline = Vec::new();
        merge_profile_cookies(&core.store, &url, &mut merged, &mut profile_baseline).unwrap();
        let cookies = merged.cookies_for(&url, false, Method::Get);
        assert!(cookies.contains("first=one"));
        assert!(cookies.contains("second=two"));

        drop(core);
        drop(command_rx);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn profile_cookie_merge_prefers_store_then_worker_changes() {
        let (core, _events, command_rx, profile_path) = direct_core();
        let url = url::Url::parse("http://profile-cookie-vixen.com/script.js").unwrap();
        let mut memory = CookieJar::default();
        memory
            .set_cookie("version=stale-memory; Path=/", &url, true)
            .unwrap();
        let memory_baseline = memory.snapshots();
        let mut worker = CookieJar::from_snapshots(memory_baseline.clone());
        let mut profile_baseline = memory_baseline.clone();
        let empty = Vec::new();
        let mut store_update = CookieJar::default();
        store_update
            .set_cookie("version=fresh-store; Path=/", &url, true)
            .unwrap();
        persist_profile_cookies(
            &core.store,
            std::slice::from_ref(&url),
            &store_update.delta_from_snapshots(&empty),
        )
        .unwrap();

        merge_profile_cookies(&core.store, &url, &mut worker, &mut profile_baseline).unwrap();
        assert_eq!(
            worker.cookies_for(&url, false, Method::Get),
            "version=fresh-store"
        );

        worker
            .set_cookie("version=redirect-worker; Path=/", &url, true)
            .unwrap();
        let current_store = store_update.snapshots();
        store_update
            .set_cookie("version=newer-store; Path=/", &url, true)
            .unwrap();
        persist_profile_cookies(
            &core.store,
            std::slice::from_ref(&url),
            &store_update.delta_from_snapshots(&current_store),
        )
        .unwrap();
        merge_profile_cookies(&core.store, &url, &mut worker, &mut profile_baseline).unwrap();
        assert_eq!(
            worker.cookies_for(&url, false, Method::Get),
            "version=redirect-worker"
        );

        drop(core);
        drop(command_rx);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn cross_origin_redirect_persists_prior_origin_cookie_deletion() {
        let server = GatedHttpServer::start(2);
        let mut config = test_config();
        server.configure(&mut config);
        let redirect_host = "redirect-cookie-vixen.com";
        config
            .network
            .dns_overrides
            .push((redirect_host.to_owned(), vec![server.address]));
        let seed_url = server.url("/delete-seed");
        let page_url = server.url("/delete-page");
        let read_url = server.url("/delete-read");
        let final_url = format!("http://{redirect_host}:{}/final.js", server.address.port());
        config.document_overrides.insert(
            seed_url.clone(),
            "<!doctype html><title>seed</title>".to_owned(),
        );
        config.document_overrides.insert(
            page_url.clone(),
            "<!doctype html><title>delete</title><script src='/delete.js'></script>".to_owned(),
        );
        config.document_overrides.insert(
            read_url.clone(),
            "<!doctype html><title>read</title>".to_owned(),
        );
        let profile_path = config.profile_path.clone();
        let mut handle = spawn_browser(config).unwrap();
        let seed_context = create(&mut handle);
        let script_context = create(&mut handle);
        drain_events(&mut handle);
        navigate(&mut handle, seed_context, &seed_url);
        let seed_state = state(&mut handle, seed_context);
        assert_eq!(
            eval(
                &mut handle,
                &seed_state,
                "document.cookie = 'doomed=stored; Path=/'; true"
            ),
            ScriptValue::Bool(true)
        );

        let navigation_id = dispatch_navigation(&mut handle, script_context, &page_url);
        let first = server.request();
        assert_eq!(first.path, "/delete.js");
        assert!(
            first
                .cookie
                .as_deref()
                .unwrap_or_default()
                .contains("doomed=stored")
        );
        first
            .respond
            .send(redirect_response_with_cookie(
                &final_url,
                "doomed=; Max-Age=0; Path=/",
            ))
            .unwrap();
        first
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("cookie deletion redirect watchdog");
        let final_request = server.request();
        assert_eq!(final_request.path, "/final.js");
        final_request
            .respond
            .send(script_response("document.title = 'accepted'", None))
            .unwrap();
        final_request
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("cookie deletion final response watchdog");
        wait_for_navigation(&mut handle, script_context, navigation_id).unwrap();

        let read_context = create(&mut handle);
        navigate(&mut handle, read_context, &read_url);
        let read_state = state(&mut handle, read_context);
        assert_eq!(
            eval(
                &mut handle,
                &read_state,
                "document.cookie.includes('doomed=stored')"
            ),
            ScriptValue::Bool(false)
        );

        server.join();
        drop(handle);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn redirected_external_script_rechecks_final_url_against_csp() {
        let server = GatedHttpServer::start(2);
        let mut config = test_config();
        server.configure(&mut config);
        let page_url = "http://same.test/redirected-script-csp";
        let allowed_url = server.url("/allowed.js");
        let blocked_url = server.url("/blocked.js");
        config.document_overrides.insert(
            page_url.to_owned(),
            format!(
                "<!doctype html><title>initial</title><meta http-equiv='Content-Security-Policy' content='script-src {allowed_url}'><script src='{allowed_url}'></script>"
            ),
        );
        let profile_path = config.profile_path.clone();
        let mut handle = spawn_browser(config).unwrap();
        let context_id = create(&mut handle);
        drain_events(&mut handle);

        let navigation_id = dispatch_navigation(&mut handle, context_id, page_url);
        let initial = server.request();
        assert_eq!(initial.path, "/allowed.js");
        initial
            .respond
            .send(redirect_response_with_cookie(
                &blocked_url,
                "blocked_redirect=1",
            ))
            .unwrap();
        initial
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("external script redirect watchdog");
        let events = wait_for_navigation(&mut handle, context_id, navigation_id).unwrap();
        let settled = state(&mut handle, context_id);

        assert_eq!(settled.title.as_deref(), Some("initial"));
        assert_eq!(
            eval(&mut handle, &settled, "typeof redirectedScriptRan"),
            ScriptValue::String("undefined".to_owned())
        );
        assert_exactly_one_terminal_phase(&events, navigation_id);

        let probe_navigation = dispatch_navigation(&mut handle, context_id, &server.url("/probe"));
        let probe = server.request();
        assert_eq!(probe.path, "/probe");
        assert!(
            !probe
                .cookie
                .as_deref()
                .unwrap_or_default()
                .contains("blocked_redirect=1")
        );
        probe
            .respond
            .send("<!doctype html><title>Probe</title>".to_owned())
            .unwrap();
        probe
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("redirect CSP cookie probe watchdog");
        wait_for_navigation(&mut handle, context_id, probe_navigation).unwrap();

        server.join();
        drop(handle);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn stop_cancels_pending_external_script_and_rejects_late_response() {
        let server = GatedHttpServer::start(2);
        let mut config = test_config();
        server.configure(&mut config);
        let page_url = "http://same.test/gated-script-stop";
        config.document_overrides.insert(
            page_url.to_owned(),
            format!(
                "<!doctype html><title>committed</title><script src='{}'></script>",
                server.url("/stopped.js")
            ),
        );
        let profile_path = config.profile_path.clone();
        let mut handle = spawn_browser(config).unwrap();
        let context_id = create(&mut handle);
        drain_events(&mut handle);

        let navigation_id = dispatch_navigation(&mut handle, context_id, page_url);
        let request = server.request();
        assert_eq!(request.path, "/stopped.js");
        assert_eq!(
            state(&mut handle, context_id).active_navigation_id,
            Some(navigation_id)
        );
        assert_eq!(
            handle
                .dispatch(BrowserCommand::Stop { context_id })
                .unwrap(),
            BrowserCommandResult::Accepted
        );
        let mut events = drain_events(&mut handle);

        request
            .respond
            .send(script_response(
                "globalThis.stoppedExternal = true",
                Some("late_script=stop"),
            ))
            .unwrap();
        request
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("late external script response watchdog");
        let stopped = state(&mut handle, context_id);
        assert_eq!(stopped.active_navigation_id, None);
        assert_eq!(
            eval(&mut handle, &stopped, "typeof stoppedExternal"),
            ScriptValue::String("undefined".to_owned())
        );

        let probe_navigation = dispatch_navigation(&mut handle, context_id, &server.url("/probe"));
        let probe = server.request();
        assert_eq!(probe.path, "/probe");
        assert!(
            !probe
                .cookie
                .as_deref()
                .unwrap_or_default()
                .contains("late_script=stop")
        );
        probe
            .respond
            .send("<!doctype html><title>Probe</title>".to_owned())
            .unwrap();
        probe
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("probe response watchdog");
        events.extend(wait_for_navigation(&mut handle, context_id, probe_navigation).unwrap());

        assert_navigation_cancelled(
            &events,
            navigation_id,
            NavigationCancellationReason::Stopped,
        );
        assert!(events.iter().any(|event| matches!(
            event,
            BrowserEvent::RuntimeEffects { effects, .. }
                if matches!(effects.network.as_slice(), [
                    RuntimeNetworkEvent::Request {
                        request_id,
                        url,
                        method,
                    },
                    RuntimeNetworkEvent::Failure {
                        request_id: failure_request_id,
                        url: failure_url,
                        blocked_reason: Some(reason),
                        ..
                    }
                ] if request_id == failure_request_id
                    && url == failure_url
                    && url.ends_with("/stopped.js")
                    && method == "GET"
                    && reason == "canceled")
        )));
        assert_no_lifecycle_success(&events, navigation_id);

        server.join();
        drop(handle);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn supersede_cancels_pending_external_script_and_rejects_late_response() {
        let server = GatedHttpServer::start(2);
        let mut config = test_config();
        server.configure(&mut config);
        let page_url = "http://same.test/gated-script-supersede";
        config.document_overrides.insert(
            page_url.to_owned(),
            format!(
                "<!doctype html><title>old</title><script src='{}'></script>",
                server.url("/superseded.js")
            ),
        );
        let profile_path = config.profile_path.clone();
        let mut handle = spawn_browser(config).unwrap();
        let context_id = create(&mut handle);
        drain_events(&mut handle);

        let navigation_id = dispatch_navigation(&mut handle, context_id, page_url);
        let request = server.request();
        assert_eq!(request.path, "/superseded.js");
        let replacement = dispatch_navigation(&mut handle, context_id, "https://same.test/b");
        let mut events = wait_for_navigation(&mut handle, context_id, replacement).unwrap();

        request
            .respond
            .send(script_response(
                "localStorage.setItem('supersededExternal', 'ran')",
                Some("late_script=supersede"),
            ))
            .unwrap();
        request
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("superseded external script response watchdog");
        let replacement_state = state(&mut handle, context_id);
        assert_eq!(replacement_state.title.as_deref(), Some("B"));
        assert_eq!(
            eval(
                &mut handle,
                &replacement_state,
                "localStorage.getItem('supersededExternal')"
            ),
            ScriptValue::Null
        );

        let probe_navigation = dispatch_navigation(&mut handle, context_id, &server.url("/probe"));
        let probe = server.request();
        assert_eq!(probe.path, "/probe");
        assert!(
            !probe
                .cookie
                .as_deref()
                .unwrap_or_default()
                .contains("late_script=supersede")
        );
        probe
            .respond
            .send("<!doctype html><title>Probe</title>".to_owned())
            .unwrap();
        probe
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("probe response watchdog");
        events.extend(wait_for_navigation(&mut handle, context_id, probe_navigation).unwrap());

        assert_navigation_cancelled(
            &events,
            navigation_id,
            NavigationCancellationReason::Superseded,
        );
        assert_no_lifecycle_success(&events, navigation_id);
        assert_exactly_one_terminal_phase(&events, replacement);

        server.join();
        drop(handle);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn external_script_completion_rejects_stale_document_and_runtime() {
        let server = GatedHttpServer::start(1);
        let mut config = test_config();
        server.configure(&mut config);
        let profile_path = config.profile_path.clone();
        let events = Arc::new(EventChannel::new(config.event_capacity));
        let (command_tx, command_rx) = mpsc::sync_channel(config.command_capacity);
        let mut core = BrowserCore::new(config, Arc::clone(&events), command_tx).unwrap();
        let context_id = direct_create(&mut core);
        drain_direct_events(&events);
        let script_url = server.url("/stale.js");
        let page_url = "http://same.test/stale-script-completion";
        let navigation_id = direct_begin_navigation(&mut core, context_id, page_url);
        direct_complete_source(
            &mut core,
            context_id,
            navigation_id,
            page_url,
            format!("<!doctype html><title>pending</title><script src='{script_url}'></script>"),
        );
        direct_drive_to_scripts(&mut core, context_id, navigation_id);
        assert!(core.advance_navigation_work());
        let request = server.request();
        assert_eq!(request.path, "/stale.js");

        let key = core
            .context(context_id)
            .unwrap()
            .active_navigation
            .as_ref()
            .unwrap()
            .pending_script
            .as_ref()
            .unwrap()
            .key;
        let baseline = core.cookies.snapshots();
        let mut stale_jar = CookieJar::from_snapshots(baseline.clone());
        stale_jar
            .set_cookie(
                "stale_completion=1",
                &url::Url::parse(&script_url).unwrap(),
                true,
            )
            .unwrap();
        let stale_delta = stale_jar.delta_from_snapshots(&baseline);
        let stale_document_id = DocumentId::new(key.document_id.get() + 1).unwrap();
        core.complete_external_script(ExternalScriptLoadCompletion {
            key: ExternalScriptLoadKey {
                document_id: stale_document_id,
                ..key
            },
            result: Ok(loaded_script_response(
                &script_url,
                "document.title = 'stale-document'",
            )),
            cookie_delta: stale_delta.clone(),
        });
        let stale_runtime_id = RuntimeContextId::new(key.runtime_context_id.get() + 1).unwrap();
        core.complete_external_script(ExternalScriptLoadCompletion {
            key: ExternalScriptLoadKey {
                runtime_context_id: stale_runtime_id,
                ..key
            },
            result: Ok(loaded_script_response(
                &script_url,
                "document.title = 'stale-runtime'",
            )),
            cookie_delta: stale_delta,
        });

        assert_eq!(
            core.context_state(context_id).unwrap().title.as_deref(),
            Some("pending")
        );
        assert!(
            core.context(context_id)
                .unwrap()
                .active_navigation
                .as_ref()
                .unwrap()
                .pending_script
                .is_some()
        );
        assert!(
            !core
                .cookies
                .cookies_for(&url::Url::parse(&script_url).unwrap(), false, Method::Get)
                .contains("stale_completion=1")
        );

        core.complete_external_script(ExternalScriptLoadCompletion {
            key,
            result: Ok(loaded_script_response(
                &script_url,
                "document.title = 'valid'",
            )),
            cookie_delta: CookieJarDelta::default(),
        });
        direct_drive_navigation(&mut core, context_id, navigation_id);
        assert_eq!(
            core.context_state(context_id).unwrap().title.as_deref(),
            Some("valid")
        );
        assert_exactly_one_terminal_phase(&drain_direct_events(&events), navigation_id);

        request
            .respond
            .send(script_response("document.title = 'network-late'", None))
            .unwrap();
        request
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("stale completion network response watchdog");
        server.join();
        drop(core);
        drop(command_rx);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn stopped_navigation_rejects_injected_external_script_completion() {
        let server = GatedHttpServer::start(1);
        let mut config = test_config();
        server.configure(&mut config);
        let profile_path = config.profile_path.clone();
        let events = Arc::new(EventChannel::new(config.event_capacity));
        let (command_tx, command_rx) = mpsc::sync_channel(config.command_capacity);
        let mut core = BrowserCore::new(config, Arc::clone(&events), command_tx).unwrap();
        let context_id = direct_create(&mut core);
        drain_direct_events(&events);
        let script_url = server.url("/stopped-completion.js");
        let page_url = "http://same.test/stopped-script-completion";
        let navigation_id = direct_begin_navigation(&mut core, context_id, page_url);
        direct_complete_source(
            &mut core,
            context_id,
            navigation_id,
            page_url,
            format!("<!doctype html><title>pending</title><script src='{script_url}'></script>"),
        );
        direct_drive_to_scripts(&mut core, context_id, navigation_id);
        assert!(core.advance_navigation_work());
        let request = server.request();
        let key = core
            .context(context_id)
            .unwrap()
            .active_navigation
            .as_ref()
            .unwrap()
            .pending_script
            .as_ref()
            .unwrap()
            .key;
        let baseline = core.cookies.snapshots();
        let mut stale_jar = CookieJar::from_snapshots(baseline.clone());
        stale_jar
            .set_cookie(
                "stopped_completion=1",
                &url::Url::parse(&script_url).unwrap(),
                true,
            )
            .unwrap();

        core.stop(context_id).unwrap();
        drain_direct_events(&events);
        core.complete_external_script(ExternalScriptLoadCompletion {
            key,
            result: Ok(loaded_script_response(
                &script_url,
                "document.title = 'obsolete'",
            )),
            cookie_delta: stale_jar.delta_from_snapshots(&baseline),
        });

        assert_eq!(
            core.context_state(context_id).unwrap().title.as_deref(),
            Some("pending")
        );
        assert!(
            !core
                .cookies
                .cookies_for(&url::Url::parse(&script_url).unwrap(), false, Method::Get)
                .contains("stopped_completion=1")
        );
        assert!(
            !drain_direct_events(&events)
                .iter()
                .any(|event| matches!(event, BrowserEvent::RuntimeEffects { .. }))
        );

        request
            .respond
            .send(script_response("document.title = 'network-obsolete'", None))
            .unwrap();
        request
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("stopped completion network response watchdog");
        server.join();
        drop(core);
        drop(command_rx);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn superseded_navigation_rejects_injected_external_script_completion() {
        let server = GatedHttpServer::start(1);
        let mut config = test_config();
        server.configure(&mut config);
        let profile_path = config.profile_path.clone();
        let events = Arc::new(EventChannel::new(config.event_capacity));
        let (command_tx, command_rx) = mpsc::sync_channel(config.command_capacity);
        let mut core = BrowserCore::new(config, Arc::clone(&events), command_tx).unwrap();
        let context_id = direct_create(&mut core);
        drain_direct_events(&events);
        let script_url = server.url("/superseded-completion.js");
        let page_url = "http://same.test/superseded-script-completion";
        let navigation_id = direct_begin_navigation(&mut core, context_id, page_url);
        direct_complete_source(
            &mut core,
            context_id,
            navigation_id,
            page_url,
            format!("<!doctype html><title>old</title><script src='{script_url}'></script>"),
        );
        direct_drive_to_scripts(&mut core, context_id, navigation_id);
        assert!(core.advance_navigation_work());
        let request = server.request();
        let key = core
            .context(context_id)
            .unwrap()
            .active_navigation
            .as_ref()
            .unwrap()
            .pending_script
            .as_ref()
            .unwrap()
            .key;
        let baseline = core.cookies.snapshots();
        let mut stale_jar = CookieJar::from_snapshots(baseline.clone());
        stale_jar
            .set_cookie(
                "superseded_completion=1",
                &url::Url::parse(&script_url).unwrap(),
                true,
            )
            .unwrap();

        let replacement =
            direct_begin_navigation(&mut core, context_id, "https://same.test/replacement");
        assert_eq!(
            core.context(context_id).unwrap().document_id,
            key.document_id
        );
        assert_eq!(
            core.context(context_id).unwrap().runtime_context_id,
            key.runtime_context_id
        );
        drain_direct_events(&events);
        core.complete_external_script(ExternalScriptLoadCompletion {
            key,
            result: Ok(loaded_script_response(
                &script_url,
                "document.title = 'obsolete'",
            )),
            cookie_delta: stale_jar.delta_from_snapshots(&baseline),
        });

        assert_eq!(
            core.context_state(context_id).unwrap().title.as_deref(),
            Some("old")
        );
        assert!(
            !core
                .cookies
                .cookies_for(&url::Url::parse(&script_url).unwrap(), false, Method::Get)
                .contains("superseded_completion=1")
        );
        assert!(drain_direct_events(&events).is_empty());

        direct_complete_source(
            &mut core,
            context_id,
            replacement,
            "https://same.test/replacement",
            "<!doctype html><title>Replacement</title>".to_owned(),
        );
        direct_drive_navigation(&mut core, context_id, replacement);
        assert_eq!(
            core.context_state(context_id).unwrap().title.as_deref(),
            Some("Replacement")
        );

        request
            .respond
            .send(script_response("document.title = 'network-obsolete'", None))
            .unwrap();
        request
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("superseded completion network response watchdog");
        server.join();
        core.start_pending_loads();
        drop(core);
        drop(command_rx);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn new_document_mutation_changes_later_author_script_discovery() {
        let (mut core, events, command_rx, profile_path) = direct_core();
        let context_id = direct_create(&mut core);
        let mut config = core.context(context_id).unwrap().config.clone();
        config.new_document_scripts = vec![
            "document.getElementById('author').textContent = \"document.title = 'rewritten'\""
                .to_owned(),
        ];
        core.configure_context(context_id, config).unwrap();
        drain_direct_events(&events);

        let navigation_id = direct_navigate(
            &mut core,
            context_id,
            "https://same.test/rewrite-author",
            "<!doctype html><title>initial</title><script id='author'>document.title = 'original'</script>",
        );
        let events = drain_direct_events(&events);
        let state = core.context_state(context_id).unwrap();

        assert_eq!(state.title.as_deref(), Some("rewritten"));
        assert!(!events.iter().any(|event| matches!(
            event,
            BrowserEvent::NavigationFailed { navigation_id: event_navigation_id, .. }
                if *event_navigation_id == navigation_id
        )));
        assert_exactly_one_terminal_phase(&events, navigation_id);

        drop(core);
        drop(command_rx);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn stop_between_author_scripts_preserves_completed_mutation() {
        let (mut core, events, command_rx, profile_path) = direct_core();
        let context_id = direct_create(&mut core);
        drain_direct_events(&events);
        let navigation_id =
            direct_begin_navigation(&mut core, context_id, "https://same.test/script-stop");
        direct_complete_source(
            &mut core,
            context_id,
            navigation_id,
            "https://same.test/script-stop",
            "<!doctype html><title>initial</title><script>document.title = 'first'</script><script>document.title = 'second'</script>".to_owned(),
        );
        direct_drive_to_scripts(&mut core, context_id, navigation_id);
        assert!(core.advance_navigation_work());
        assert_eq!(
            core.context_state(context_id).unwrap().title.as_deref(),
            Some("first")
        );

        core.stop(context_id).unwrap();
        assert!(core.advance_navigation_work());
        let events = drain_direct_events(&events);
        let stopped = core.context_state(context_id).unwrap();

        assert_eq!(stopped.title.as_deref(), Some("first"));
        assert_eq!(
            phases_for(&events, navigation_id),
            vec![
                NavigationPhase::Intent,
                NavigationPhase::Policy,
                NavigationPhase::Request,
                NavigationPhase::Response,
                NavigationPhase::Commit,
                NavigationPhase::Parse,
                NavigationPhase::ScriptsAndSubresources,
                NavigationPhase::Cancelled,
            ]
        );
        assert_navigation_cancelled(
            &events,
            navigation_id,
            NavigationCancellationReason::Stopped,
        );
        assert_no_lifecycle_success(&events, navigation_id);

        core.start_pending_loads();
        drop(core);
        drop(command_rx);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn supersede_between_author_scripts_suppresses_unstarted_item() {
        let (mut core, events, command_rx, profile_path) = direct_core();
        let context_id = direct_create(&mut core);
        drain_direct_events(&events);
        let navigation_id =
            direct_begin_navigation(&mut core, context_id, "https://same.test/script-supersede");
        direct_complete_source(
            &mut core,
            context_id,
            navigation_id,
            "https://same.test/script-supersede",
            "<!doctype html><script>localStorage.setItem('completed', 'yes')</script><script>localStorage.setItem('unstarted', 'no')</script>".to_owned(),
        );
        direct_drive_to_scripts(&mut core, context_id, navigation_id);
        assert!(core.advance_navigation_work());

        let replacement =
            direct_begin_navigation(&mut core, context_id, "https://same.test/replacement");
        direct_complete_source(
            &mut core,
            context_id,
            replacement,
            "https://same.test/replacement",
            "<!doctype html><title>Replacement</title>".to_owned(),
        );
        direct_drive_navigation(&mut core, context_id, replacement);
        let events = drain_direct_events(&events);

        assert_eq!(
            direct_eval(
                &mut core,
                context_id,
                "`${localStorage.getItem('completed')}:${localStorage.getItem('unstarted')}`",
            ),
            ScriptValue::String("yes:null".to_owned())
        );
        assert_navigation_cancelled(
            &events,
            navigation_id,
            NavigationCancellationReason::Superseded,
        );
        assert_no_lifecycle_success(&events, navigation_id);
        assert_exactly_one_terminal_phase(&events, replacement);

        core.start_pending_loads();
        drop(core);
        drop(command_rx);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn author_exception_is_runtime_effect_and_later_script_runs() {
        let (mut core, events, command_rx, profile_path) = direct_core();
        let context_id = direct_create(&mut core);
        drain_direct_events(&events);
        let navigation_id = direct_navigate(
            &mut core,
            context_id,
            "https://same.test/script-error",
            "<!doctype html><title>initial</title><script>throw new Error('author boom')</script><script>document.title = 'after-error'</script>",
        );
        let events = drain_direct_events(&events);
        let state = core.context_state(context_id).unwrap();
        let exceptions = events
            .iter()
            .filter_map(|event| match event {
                BrowserEvent::RuntimeEffects {
                    context_id: event_context_id,
                    document_id,
                    runtime_context_id,
                    effects,
                    ..
                } if *event_context_id == context_id
                    && *document_id == state.document_id
                    && Some(*runtime_context_id) == state.runtime_context_id =>
                {
                    Some(effects.exceptions.as_slice())
                }
                _ => None,
            })
            .flatten()
            .collect::<Vec<_>>();

        assert_eq!(state.title.as_deref(), Some("after-error"));
        assert_eq!(exceptions.len(), 1);
        assert_eq!(
            exceptions[0].error.code,
            crate::engine_error::codes::SCRIPT_EVAL
        );
        assert!(exceptions[0].error.message.contains("author boom"));
        assert!(!events.iter().any(|event| matches!(
            event,
            BrowserEvent::NavigationFailed { navigation_id: event_navigation_id, .. }
                if *event_navigation_id == navigation_id
        )));
        assert_eq!(
            phases_for(&events, navigation_id),
            vec![
                NavigationPhase::Intent,
                NavigationPhase::Policy,
                NavigationPhase::Request,
                NavigationPhase::Response,
                NavigationPhase::Commit,
                NavigationPhase::Parse,
                NavigationPhase::ScriptsAndSubresources,
                NavigationPhase::DomContentLoaded,
                NavigationPhase::Load,
                NavigationPhase::Settled,
            ]
        );
        assert_exactly_one_terminal_phase(&events, navigation_id);

        drop(core);
        drop(command_rx);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn author_timeout_is_runtime_effect_and_later_script_runs() {
        let (mut core, events, command_rx, profile_path) = direct_core();
        let context_id = direct_create(&mut core);
        drain_direct_events(&events);
        let navigation_id = direct_navigate(
            &mut core,
            context_id,
            "https://same.test/script-timeout",
            "<!doctype html><title>initial</title><script>for (;;) {}</script><script>document.title = 'after-timeout'</script>",
        );
        let events = drain_direct_events(&events);
        let state = core.context_state(context_id).unwrap();

        assert_eq!(state.title.as_deref(), Some("after-timeout"));
        assert!(events.iter().any(|event| matches!(
            event,
            BrowserEvent::RuntimeEffects { effects, .. }
                if effects.exceptions.iter().any(|exception| {
                    exception.error.code == crate::engine_error::codes::SCRIPT_TIMEOUT
                })
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            BrowserEvent::NavigationFailed { navigation_id: event_navigation_id, .. }
                if *event_navigation_id == navigation_id
        )));
        assert_exactly_one_terminal_phase(&events, navigation_id);

        drop(core);
        drop(command_rx);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn stop_after_dom_content_loaded_suppresses_load_and_settle() {
        let (mut core, events, command_rx, profile_path) = direct_core();
        let context_id = direct_create(&mut core);
        drain_direct_events(&events);
        let navigation_id =
            direct_begin_navigation(&mut core, context_id, "https://same.test/lifecycle-stop");
        direct_complete_source(
            &mut core,
            context_id,
            navigation_id,
            "https://same.test/lifecycle-stop",
            "<!doctype html><title>Lifecycle</title>".to_owned(),
        );
        direct_drive_to_scripts(&mut core, context_id, navigation_id);
        assert!(core.advance_navigation_work());
        assert!(core.advance_navigation_work());

        core.stop(context_id).unwrap();
        assert!(core.advance_navigation_work());
        let events = drain_direct_events(&events);

        assert_eq!(
            phases_for(&events, navigation_id),
            vec![
                NavigationPhase::Intent,
                NavigationPhase::Policy,
                NavigationPhase::Request,
                NavigationPhase::Response,
                NavigationPhase::Commit,
                NavigationPhase::Parse,
                NavigationPhase::ScriptsAndSubresources,
                NavigationPhase::DomContentLoaded,
                NavigationPhase::Cancelled,
            ]
        );
        assert!(events.iter().any(|event| matches!(
            event,
            BrowserEvent::DomContentLoaded { navigation_id: event_navigation_id, .. }
                if *event_navigation_id == navigation_id
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            BrowserEvent::DocumentLoadCompleted { navigation_id: event_navigation_id, .. }
                if *event_navigation_id == navigation_id
        )));
        assert_navigation_cancelled(
            &events,
            navigation_id,
            NavigationCancellationReason::Stopped,
        );

        core.start_pending_loads();
        drop(core);
        drop(command_rx);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn redirect_stop_rejects_late_redirect_completion() {
        let server = GatedHttpServer::start(3);
        let mut config = test_config();
        server.configure(&mut config);
        let profile_path = config.profile_path.clone();
        let mut handle = spawn_browser(config).unwrap();
        let context_id = create(&mut handle);
        drain_events(&mut handle);
        let initial = state(&mut handle, context_id);
        let initial_history = history(&mut handle, context_id);

        let redirect_url = server.url("/redirect-stop");
        let final_url = server.url("/redirect-final");
        let navigation_id = dispatch_navigation(&mut handle, context_id, &redirect_url);
        let redirect_request = server.request();
        assert_eq!(redirect_request.path, "/redirect-stop");
        redirect_request
            .respond
            .send(redirect_response(&final_url))
            .unwrap();
        redirect_request
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("redirect-stop response watchdog");

        let final_request = server.request();
        assert_eq!(final_request.path, "/redirect-final");
        let mut events = Vec::new();
        let latest_request_id = loop {
            let event = handle
                .wait_next_event(Duration::from_secs(10))
                .unwrap()
                .expect("redirect event before stop");
            let next_request_id = match &event {
                BrowserEvent::NavigationRedirected {
                    navigation_id: event_navigation_id,
                    next_request_id,
                    ..
                } if *event_navigation_id == navigation_id => Some(*next_request_id),
                _ => None,
            };
            events.push(event);
            if let Some(next_request_id) = next_request_id {
                break next_request_id;
            }
        };
        assert_eq!(
            state(&mut handle, context_id).active_navigation_id,
            Some(navigation_id)
        );
        assert_eq!(
            handle
                .dispatch(BrowserCommand::Stop { context_id })
                .unwrap(),
            BrowserCommandResult::Accepted
        );
        events.extend(drain_events(&mut handle));

        final_request
            .respond
            .send("<!doctype html><title>Too late final</title>".to_owned())
            .unwrap();
        final_request
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("late redirected final response watchdog");
        inject_late_source_completion(
            &handle,
            context_id,
            navigation_id,
            final_url.clone(),
            "Injected stale redirect",
            Some("source=redirect-final"),
        );
        events.extend(drain_events(&mut handle));
        let after_late = state(&mut handle, context_id);
        assert_eq!(after_late.document_id, initial.document_id);
        assert_eq!(after_late.url, initial.url);
        assert_eq!(history(&mut handle, context_id), initial_history);

        let probe_navigation = dispatch_navigation(&mut handle, context_id, &server.url("/probe"));
        let probe_request = server.request();
        assert_eq!(probe_request.path, "/probe");
        assert!(
            !probe_request
                .cookie
                .as_deref()
                .unwrap_or_default()
                .contains("source=redirect-final")
        );
        probe_request
            .respond
            .send("<!doctype html><title>Probe</title>".to_owned())
            .unwrap();
        probe_request
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("probe response watchdog");
        events.extend(wait_for_navigation(&mut handle, context_id, probe_navigation).unwrap());
        assert_navigation_cancelled(
            &events,
            navigation_id,
            NavigationCancellationReason::Stopped,
        );
        assert_no_terminal_success(&events, navigation_id);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    BrowserEvent::NavigationRedirected {
                        navigation_id: event_navigation_id,
                        ..
                    } if *event_navigation_id == navigation_id
                ))
                .count(),
            1
        );
        assert!(events.iter().any(|event| matches!(
            event,
            BrowserEvent::NavigationCancelled {
                navigation_id: event_navigation_id,
                request_id: Some(request_id),
                reason: NavigationCancellationReason::Stopped,
                ..
            } if *event_navigation_id == navigation_id && *request_id == latest_request_id
        )));

        server.join();
        drop(handle);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn stop_keeps_late_source_completion_from_committing_or_emitting_terminal_load_events() {
        let server = GatedHttpServer::start(2);
        let mut config = test_config();
        server.configure(&mut config);
        let profile_path = config.profile_path.clone();
        let mut handle = spawn_browser(config).unwrap();
        let context_id = create(&mut handle);
        drain_events(&mut handle);
        let initial = state(&mut handle, context_id);
        let initial_history = history(&mut handle, context_id);

        let navigation_id = dispatch_navigation(&mut handle, context_id, &server.url("/stopped"));
        let request = server.request();
        assert_eq!(request.path, "/stopped");
        assert_eq!(
            state(&mut handle, context_id).active_navigation_id,
            Some(navigation_id)
        );
        assert_eq!(
            handle
                .dispatch(BrowserCommand::Stop { context_id })
                .unwrap(),
            BrowserCommandResult::Accepted
        );
        let mut events = drain_events(&mut handle);
        let stopped = state(&mut handle, context_id);
        assert_eq!(stopped.active_navigation_id, None);
        assert_eq!(stopped.document_id, initial.document_id);

        request
            .respond
            .send("<!doctype html><title>Too late</title>".to_owned())
            .unwrap();
        request
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("late stopped response watchdog");
        inject_late_source_completion(
            &handle,
            context_id,
            navigation_id,
            server.url("/stopped"),
            "Injected stale stop",
            Some("source=stopped"),
        );
        let after_late_response = state(&mut handle, context_id);
        events.extend(drain_events(&mut handle));
        assert_eq!(after_late_response.document_id, initial.document_id);
        assert_eq!(after_late_response.url, initial.url);
        assert_eq!(history(&mut handle, context_id), initial_history);

        let probe_navigation = dispatch_navigation(&mut handle, context_id, &server.url("/probe"));
        let probe_request = server.request();
        assert_eq!(probe_request.path, "/probe");
        assert!(
            !probe_request
                .cookie
                .as_deref()
                .unwrap_or_default()
                .contains("source=stopped")
        );
        probe_request
            .respond
            .send("<!doctype html><title>Probe</title>".to_owned())
            .unwrap();
        probe_request
            .completed
            .recv_timeout(Duration::from_secs(10))
            .expect("probe response watchdog");
        events.extend(wait_for_navigation(&mut handle, context_id, probe_navigation).unwrap());
        assert_navigation_cancelled(
            &events,
            navigation_id,
            NavigationCancellationReason::Stopped,
        );
        assert_no_terminal_success(&events, navigation_id);

        server.join();
        drop(handle);
        let _ = std::fs::remove_file(profile_path);
    }
}
