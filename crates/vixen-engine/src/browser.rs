//! Engine-owned browser/profile/context lifecycle.
//!
//! This is the first production owner selected by ADR-017. All `Page`, V8,
//! history, and context-registry mutation runs on one dedicated thread. The
//! main-document source loader runs off-thread with generation-tagged results;
//! parsing, runtime creation, and commit remain on the dedicated owner thread.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::marker::PhantomData;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Duration;

use vixen_api::{
    AutomationEvaluation, BrowserCommand, BrowserCommandResult, BrowserError, BrowserEvent,
    BrowserHandle, BrowserId, BrowsingContextConfig, BrowsingContextId, BrowsingContextState,
    DocumentId, DocumentTextKind, FocusEventInfo, FocusProjection, FormEntryInfo,
    FormEntryValueInfo, FormSubmissionInfo, FrameId, KeyEventData, MouseEventData,
    NavigationCancellationReason, NavigationHistoryEntry, NavigationHistorySnapshot, NavigationId,
    NavigationPhase, ProfileDataSelection, ProfileId, ProfileSessionState, RequestId,
    RuntimeBindingEvent, RuntimeConsoleArg, RuntimeConsoleEvent, RuntimeConsoleValue,
    RuntimeContextId, RuntimeDialogEvent, RuntimeEffects, RuntimeNetworkEvent, ScriptValue,
    browser_error_codes,
};
use vixen_net::{CookieJar, CookieJarDelta, Method, Network, NetworkConfig, NetworkEvent};
use vixen_store::{ClearDataSelection, SessionRecord, Store};

use crate::data_url::parse_data_url;
use crate::display_list::PaintCommand;
use crate::history::{HistoryEntry, SessionHistory};
use crate::page::Page;
use crate::script::{JsConsoleValue, JsNavigationAction, JsNetworkEvent, JsRuntime, JsValue};

const DEFAULT_MAX_CONTEXTS: usize = 128;
const DEFAULT_COMMAND_CAPACITY: usize = 256;
const DEFAULT_EVENT_CAPACITY: usize = 2048;
const MAX_SCRIPT_BYTES: usize = 1024 * 1024;
const MAX_SELECTOR_BYTES: usize = 64 * 1024;
const MAX_URL_BYTES: usize = 16 * 1024;
const MAX_VIEWPORT_DIMENSION: u32 = 16_384;
const MAX_RUNTIME_SLOTS: usize = 512;

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
            while let Ok(message) = command_rx.recv() {
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
                        let _ = reply.send(core.capture_paint_snapshot(
                            context_id,
                            document_id,
                            viewport,
                        ));
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
                    CoreMessage::Shutdown => {
                        core.shutdown();
                        break;
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
    SourceLoaded(SourceLoadCompletion),
    Shutdown,
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
}

struct RuntimeSlot {
    runtime: JsRuntime,
    active: bool,
}

struct ActiveNavigation {
    navigation_id: NavigationId,
    request_id: RequestId,
    history_update: HistoryUpdate,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    load_task: Option<tokio::task::AbortHandle>,
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
    events: Vec<NetworkEvent>,
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
            document_overrides: config.document_overrides,
            _local_only: PhantomData,
        })
    }

    fn dispatch(&mut self, command: BrowserCommand) -> Result<BrowserCommandResult, BrowserError> {
        match command {
            BrowserCommand::LoadProfileSession => self.load_profile_session(),
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
                Ok(BrowserCommandResult::HitTest(
                    context.page.element_at(viewport, x, y),
                ))
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
        Ok(PaintSnapshot {
            context_id,
            document_id,
            viewport,
            commands: context.page.display_list(viewport),
        })
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
        self.begin_navigation(context_id, url, Some(html), history_update)
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
        let mut context = self.contexts.remove(&context_id).expect("context checked");
        self.runtime_slots[context.runtime_slot].active = false;
        if let Some(mut navigation) = context.active_navigation.take() {
            if let Some(cancel) = navigation.cancel.take() {
                let _ = cancel.send(());
            }
            if let Some(load_task) = navigation.load_task.take() {
                load_task.abort();
            }
            self.emit_phase(
                context_id,
                context.frame_id,
                navigation.navigation_id,
                NavigationPhase::Cancelled,
            );
            self.emit(BrowserEvent::NavigationCancelled {
                context_id,
                frame_id: context.frame_id,
                navigation_id: navigation.navigation_id,
                request_id: Some(navigation.request_id),
                reason: NavigationCancellationReason::ContextClosed,
            });
        }
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
        self.begin_navigation(context_id, url, None, history_update)
    }

    fn begin_navigation(
        &mut self,
        context_id: BrowsingContextId,
        url: String,
        injected_html: Option<String>,
        history_update: HistoryUpdate,
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
            cancel: Some(cancel),
            load_task: None,
        });
        self.emit(BrowserEvent::NavigationRequested {
            context_id,
            frame_id,
            navigation_id,
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
                    result = load_source(&mut network, &mut worker_jar, input) => result,
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
        let initial_request_id = active.request_id;
        let history_update = active.history_update.clone();
        self.cookies.apply_delta(cookie_delta);
        let loaded = match result {
            Ok(loaded) => loaded,
            Err(error) => {
                self.fail_navigation(
                    context_id,
                    frame_id,
                    navigation_id,
                    Some(initial_request_id),
                    error,
                );
                return;
            }
        };
        let mut request_id = initial_request_id;
        for event in &loaded.events {
            if let NetworkEvent::Redirect { from, to, status } = event {
                let next_request_id = match self.ids.request() {
                    Ok(request_id) => request_id,
                    Err(error) => {
                        self.fail_navigation(
                            context_id,
                            frame_id,
                            navigation_id,
                            Some(request_id),
                            error,
                        );
                        return;
                    }
                };
                self.emit(BrowserEvent::NavigationRedirected {
                    context_id,
                    frame_id,
                    navigation_id,
                    request_id,
                    next_request_id,
                    from_url: from.clone(),
                    to_url: to.clone(),
                    status: *status,
                });
                request_id = next_request_id;
            }
        }
        self.emit_phase(
            context_id,
            frame_id,
            navigation_id,
            NavigationPhase::Response,
        );

        let mut page = match Page::from_html_with_headers(
            loaded.final_url.clone(),
            &loaded.html,
            loaded
                .headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
        ) {
            Ok(page) => page,
            Err(error) => {
                self.fail_navigation(
                    context_id,
                    frame_id,
                    navigation_id,
                    Some(request_id),
                    BrowserError::new(browser_error_codes::NAVIGATION_LOAD, error.to_string()),
                );
                return;
            }
        };

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
                let disposition = if history.url() == Some(loaded.final_url.as_str()) {
                    HistoryDisposition::Keep
                } else {
                    HistoryDisposition::Replace
                };
                (history, disposition)
            }
        };
        match history_disposition {
            HistoryDisposition::Push => {
                history.push(HistoryEntry::navigation(loaded.final_url.clone()));
            }
            HistoryDisposition::Replace => {
                history.replace(HistoryEntry::navigation(loaded.final_url.clone()));
            }
            HistoryDisposition::Keep => {}
        }
        page.set_session_history(history);

        let document_id = match self.ids.document() {
            Ok(document_id) => document_id,
            Err(error) => {
                self.fail_navigation(context_id, frame_id, navigation_id, Some(request_id), error);
                return;
            }
        };
        let runtime_context_id = match self.ids.runtime() {
            Ok(runtime_context_id) => runtime_context_id,
            Err(error) => {
                self.fail_navigation(context_id, frame_id, navigation_id, Some(request_id), error);
                return;
            }
        };
        let (old_document_id, old_runtime_context_id, old_runtime_slot, context_config) = {
            let context = self
                .context(context_id)
                .expect("active navigation context exists");
            (
                context.document_id,
                context.runtime_context_id,
                context.runtime_slot,
                context.config.clone(),
            )
        };
        if self.runtime_slots.len() >= MAX_RUNTIME_SLOTS {
            self.fail_navigation(
                context_id,
                frame_id,
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
                    frame_id,
                    navigation_id,
                    Some(request_id),
                    engine_error(error),
                );
                return;
            }
        };
        apply_runtime_config(&mut runtime, &context_config);
        if let Err(error) = self.record_visit_url(&loaded.final_url) {
            self.fail_navigation(context_id, frame_id, navigation_id, Some(request_id), error);
            return;
        }
        let runtime_slot = self.runtime_slots.len();
        self.runtime_slots.push(RuntimeSlot {
            runtime,
            active: true,
        });
        self.runtime_slots[old_runtime_slot].active = false;
        self.emit_phase(context_id, frame_id, navigation_id, NavigationPhase::Commit);
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
        }
        self.emit(BrowserEvent::NavigationCommitted {
            context_id,
            frame_id,
            navigation_id,
            request_id: Some(request_id),
            document_id,
            runtime_context_id: Some(runtime_context_id),
            url: loaded.final_url,
        });
        self.emit(BrowserEvent::RuntimeContextCreated {
            context_id,
            frame_id,
            document_id,
            runtime_context_id,
        });
        self.emit_phase(context_id, frame_id, navigation_id, NavigationPhase::Parse);
        self.emit_phase(
            context_id,
            frame_id,
            navigation_id,
            NavigationPhase::ScriptsAndSubresources,
        );
        let script_result = {
            let (contexts, runtime_slots) = (&mut self.contexts, &mut self.runtime_slots);
            let context = contexts.get_mut(&context_id).expect("context checked");
            runtime_slots[runtime_slot]
                .runtime
                .with_entered_isolate(|runtime| {
                    for source in &context.config.preload_scripts {
                        runtime.evaluate_with_page_mut(source, &mut context.page)?;
                    }
                    for source in &context.config.new_document_scripts {
                        runtime.evaluate_with_page_mut(source, &mut context.page)?;
                    }
                    runtime.execute_page_scripts_with_csp_bypass(
                        &mut context.page,
                        context.config.bypass_csp,
                    )?;
                    let effects = drain_runtime_effects(runtime)?;
                    let actions = runtime.drain_navigation_actions()?;
                    Ok::<_, crate::engine_error::EngineError>((effects, actions))
                })
        };
        let (effects, actions) = match script_result {
            Ok(result) => result,
            Err(error) => {
                self.fail_navigation(
                    context_id,
                    frame_id,
                    navigation_id,
                    Some(request_id),
                    engine_error(error),
                );
                return;
            }
        };
        if !effects.is_empty() {
            self.emit(BrowserEvent::RuntimeEffects {
                context_id,
                document_id,
                runtime_context_id,
                effects,
            });
        }
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
        self.emit_phase(context_id, frame_id, navigation_id, NavigationPhase::Load);
        self.emit(BrowserEvent::DocumentLoadCompleted {
            context_id,
            frame_id,
            navigation_id,
            document_id,
        });
        self.context_mut(context_id)
            .expect("active navigation context exists")
            .active_navigation = None;
        self.emit_phase(
            context_id,
            frame_id,
            navigation_id,
            NavigationPhase::Settled,
        );
        self.emit(BrowserEvent::BrowsingContextStateChanged {
            state: self
                .context_state(context_id)
                .expect("active navigation context exists"),
        });
        let _ = self.apply_navigation_actions(context_id, actions);
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
        let context = self.context_mut(context_id)?;
        let Some(mut navigation) = context.active_navigation.take() else {
            return Ok(false);
        };
        let frame_id = context.frame_id;
        if let Some(cancel) = navigation.cancel.take() {
            let _ = cancel.send(());
        }
        if let Some(load_task) = navigation.load_task.take() {
            load_task.abort();
        }
        self.emit_phase(
            context_id,
            frame_id,
            navigation.navigation_id,
            NavigationPhase::Cancelled,
        );
        self.emit(BrowserEvent::NavigationCancelled {
            context_id,
            frame_id,
            navigation_id: navigation.navigation_id,
            request_id: Some(navigation.request_id),
            reason,
        });
        if emit_state {
            self.emit(BrowserEvent::BrowsingContextStateChanged {
                state: self.context_state(context_id)?,
            });
        }
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

    fn evaluate(
        &mut self,
        context_id: BrowsingContextId,
        document_id: DocumentId,
        runtime_context_id: RuntimeContextId,
        source: String,
    ) -> Result<BrowserCommandResult, BrowserError> {
        let evaluation =
            self.automation_evaluation(context_id, document_id, runtime_context_id, source)?;
        Ok(BrowserCommandResult::Evaluation(evaluation.value))
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
        let (value, effects, actions) = runtime_slots[runtime_slot]
            .runtime
            .with_entered_isolate(|runtime| {
                let value = runtime.evaluate_with_page_mut(&source, &mut context.page)?;
                let effects = drain_runtime_effects(runtime)?;
                let actions = runtime.drain_navigation_actions()?;
                Ok::<_, crate::engine_error::EngineError>((value, effects, actions))
            })
            .map_err(script_error)?;
        let state = context_state(context_id, context);
        self.emit(BrowserEvent::BrowsingContextStateChanged { state });
        self.apply_navigation_actions(context_id, actions)?;
        Ok(AutomationEvaluation {
            value: script_value(value),
            effects,
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
        Ok(BrowserCommandResult::InputDispatched(evaluation.effects))
    }

    fn dispatch_key_event(
        &mut self,
        context_id: BrowsingContextId,
        document_id: DocumentId,
        runtime_context_id: RuntimeContextId,
        event_type: String,
        event: KeyEventData,
    ) -> Result<BrowserCommandResult, BrowserError> {
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
        Ok(BrowserCommandResult::InputDispatched(evaluation.effects))
    }

    fn apply_navigation_actions(
        &mut self,
        context_id: BrowsingContextId,
        actions: Vec<JsNavigationAction>,
    ) -> Result<(), BrowserError> {
        for action in actions {
            match action {
                JsNavigationAction::Navigate { url, replace } => {
                    self.navigate(
                        context_id,
                        url,
                        if replace {
                            HistoryUpdate::Replace
                        } else {
                            HistoryUpdate::Push
                        },
                    )?;
                }
                JsNavigationAction::SetContent { html } => {
                    let (url, history) = {
                        let context = self.context(context_id)?;
                        (
                            context.page.url().to_owned(),
                            context.page.session_history().clone(),
                        )
                    };
                    self.navigate_injected(
                        context_id,
                        url,
                        html,
                        HistoryUpdate::Preserve(history),
                    )?;
                }
                JsNavigationAction::FormSubmit {
                    form_id,
                    form_node_id,
                    submitter_node_id,
                    action,
                    method,
                    ..
                } => {
                    let context = self.context(context_id)?;
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
                        self.navigate(context_id, target, HistoryUpdate::Push)?;
                    }
                }
                JsNavigationAction::HistoryPush {
                    url,
                    state_json,
                    title,
                } => self.apply_history_state(context_id, url, state_json, title, false)?,
                JsNavigationAction::HistoryReplace {
                    url,
                    state_json,
                    title,
                } => self.apply_history_state(context_id, url, state_json, title, true)?,
                JsNavigationAction::HistoryTraverse { delta } => {
                    let context = self.context(context_id)?;
                    let mut history = context.page.session_history().clone();
                    let Some(entry) = history.go(delta).cloned() else {
                        continue;
                    };
                    if entry.state.is_some() {
                        let runtime_slot = context.runtime_slot;
                        self.context_mut(context_id)?
                            .page
                            .set_session_history(history);
                        let (contexts, slots) = (&self.contexts, &mut self.runtime_slots);
                        let page = &contexts.get(&context_id).expect("context checked").page;
                        slots[runtime_slot].runtime.sync_page_realm_key(page);
                    } else {
                        self.navigate(context_id, entry.url, HistoryUpdate::Preserve(history))?;
                    }
                }
            }
        }
        Ok(())
    }

    fn apply_history_state(
        &mut self,
        context_id: BrowsingContextId,
        url: String,
        state_json: String,
        title: String,
        replace: bool,
    ) -> Result<(), BrowserError> {
        let context = self.context(context_id)?;
        ensure_same_origin_history_url(context.page.url(), &url)?;
        let runtime_slot = context.runtime_slot;
        let mut history = context.page.session_history().clone();
        let mut entry = HistoryEntry::push_state(url, state_json.into_bytes());
        if !title.is_empty() {
            entry.title = Some(title);
        }
        if replace {
            history.replace(entry);
        } else {
            history.push(entry);
        }
        self.context_mut(context_id)?
            .page
            .set_session_history(history);
        let (contexts, slots) = (&self.contexts, &mut self.runtime_slots);
        let page = &contexts.get(&context_id).expect("context checked").page;
        slots[runtime_slot].runtime.sync_page_realm_key(page);
        self.emit(BrowserEvent::BrowsingContextStateChanged {
            state: self.context_state(context_id)?,
        });
        Ok(())
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
        frame_id: FrameId,
        navigation_id: NavigationId,
        request_id: Option<RequestId>,
        error: BrowserError,
    ) {
        let is_current = self
            .context(context_id)
            .ok()
            .and_then(|context| context.active_navigation.as_ref())
            .is_some_and(|active| active.navigation_id == navigation_id);
        if !is_current {
            return;
        }
        if let Ok(context) = self.context_mut(context_id) {
            context.active_navigation = None;
        }
        self.emit_phase(context_id, frame_id, navigation_id, NavigationPhase::Failed);
        self.emit(BrowserEvent::NavigationFailed {
            context_id,
            frame_id,
            navigation_id,
            request_id,
            error,
        });
        if let Ok(state) = self.context_state(context_id) {
            self.emit(BrowserEvent::BrowsingContextStateChanged { state });
        }
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
    }
}

async fn load_source(
    network: &mut Network,
    cookies: &mut CookieJar,
    input: SourceLoadInput,
) -> Result<LoadedSource, BrowserError> {
    let SourceLoadInput { url, injected_html } = input;
    if let Some(html) = injected_html {
        return Ok(LoadedSource {
            final_url: url,
            html,
            headers: Vec::new(),
            events: Vec::new(),
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
            events: Vec::new(),
        }),
        "about" if parsed.path() == "vixen" => Ok(LoadedSource {
            final_url: parsed.to_string(),
            html: "<!doctype html><title>Vixen</title><h1>Vixen</h1>".to_owned(),
            headers: Vec::new(),
            events: Vec::new(),
        }),
        "data" => {
            let data = parse_data_url(&url).map_err(|error| {
                BrowserError::new(browser_error_codes::NAVIGATION_LOAD, error.to_string())
            })?;
            Ok(LoadedSource {
                final_url: parsed.to_string(),
                html: String::from_utf8_lossy(&data.data).into_owned(),
                headers: vec![("content-type".to_owned(), data.mime_type.essence())],
                events: Vec::new(),
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
            let html = tokio::fs::read_to_string(&path).await.map_err(|error| {
                BrowserError::new(
                    browser_error_codes::NAVIGATION_LOAD,
                    format!("failed to read {}: {error}", path.display()),
                )
            })?;
            Ok(LoadedSource {
                final_url: parsed.to_string(),
                html,
                headers: Vec::new(),
                events: Vec::new(),
            })
        }
        "http" | "https" => {
            let response = network
                .get_text_with_cookies(cookies, &parsed, false, Method::Get)
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
                events: response.events,
            })
        }
        scheme => Err(BrowserError::new(
            browser_error_codes::INVALID_ARGUMENT,
            format!("unsupported navigation URL scheme: {scheme}"),
        )),
    }
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
    })
}

fn json_string(value: &str) -> Result<String, BrowserError> {
    deno_core::serde_json::to_string(value).map_err(|error| {
        BrowserError::new(
            browser_error_codes::INVALID_ARGUMENT,
            format!("failed to encode script input: {error}"),
        )
    })
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
                    events: Vec::new(),
                }),
                cookie_delta: worker_jar.delta_from_snapshots(&baseline),
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
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nSet-Cookie: source={}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            cookie_value,
                            body.len(),
                            body
                        );
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
            BrowserCommandResult::Evaluation(value) => value,
            other => panic!("unexpected eval result: {other:?}"),
        }
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
    fn stop_keeps_late_source_completion_from_committing_or_emitting_terminal_load_events() {
        let server = GatedHttpServer::start(1);
        let mut config = test_config();
        server.configure(&mut config);
        let profile_path = config.profile_path.clone();
        let mut handle = spawn_browser(config).unwrap();
        let context_id = create(&mut handle);
        drain_events(&mut handle);
        let initial = state(&mut handle, context_id);

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
        assert!(events.iter().any(|event| matches!(
            event,
            BrowserEvent::NavigationCancelled {
                navigation_id: event_navigation_id,
                reason: NavigationCancellationReason::Stopped,
                ..
            } if *event_navigation_id == navigation_id
        )));
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

        server.join();
        drop(handle);
        let _ = std::fs::remove_file(profile_path);
    }
}
