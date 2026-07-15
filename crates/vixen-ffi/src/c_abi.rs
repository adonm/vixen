//! Handwritten C-compatible ABI over [`crate::FlutterBrowserController`].
//!
//! Unsafe code is isolated here because C callers provide raw input and output
//! pointers. Handles and allocations are registry tokens; caller values are
//! never dereferenced as Rust objects.

#![allow(unsafe_code)]

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{Map, Value, json};
use vixen_api::{
    ACCESSIBILITY_MAX_VALUE_BYTES, AccessibilityAction, AccessibilityNode, AccessibilitySnapshot,
    AccessibilityTextSelection, BrowserError, BrowserEvent, BrowsingContextId,
    BrowsingContextState, CrossDocumentNavigationKind, DiagnosticScope, DownloadEvent,
    EngineDiagnostic, EngineDiagnosticCategory, HostLifecycle, HostViewState, InputDispatchResult,
    KeyEventData, MouseEventData, NavigationActionOutcome, NavigationCancellationReason,
    NavigationPhase, RenderBridgeSubmission, RenderBridgeUpdate, RenderCommit, RenderCommitState,
    RenderHandleRelease, RenderReplica, RuntimeConsoleArg, RuntimeConsoleValue, RuntimeEffects,
    RuntimeNetworkEvent, TextInputState,
};

use crate::{
    ABI_VERSION, ControllerCommand, ControllerResponse, FlutterBrowserController,
    bounded_accessibility_snapshot,
};

pub const VIXEN_STATUS_OK: u32 = 0;
pub const VIXEN_STATUS_NO_EVENT: u32 = 1;
pub const VIXEN_STATUS_INVALID_ARGUMENT: u32 = 2;
pub const VIXEN_STATUS_INVALID_UTF8: u32 = 3;
pub const VIXEN_STATUS_INPUT_TOO_LARGE: u32 = 4;
pub const VIXEN_STATUS_INVALID_COMMAND: u32 = 5;
pub const VIXEN_STATUS_UNKNOWN_HANDLE: u32 = 6;
pub const VIXEN_STATUS_BROWSER_ERROR: u32 = 7;
pub const VIXEN_STATUS_UNKNOWN_BUFFER: u32 = 8;
pub const VIXEN_STATUS_PANIC: u32 = 9;
pub const VIXEN_STATUS_INTERNAL_ERROR: u32 = 10;
pub const VIXEN_STATUS_OUTPUT_TOO_LARGE: u32 = 11;
pub const VIXEN_STATUS_BUFFER_LIMIT: u32 = 12;

pub const VIXEN_MAX_PROFILE_PATH_BYTES: usize = 4096;
pub const VIXEN_MAX_MESSAGE_BYTES: usize = 65_536;
pub const VIXEN_MAX_OUTPUT_BYTES: usize = 1_048_576;
pub const VIXEN_MAX_OUTSTANDING_BUFFERS: usize = 64;
pub const VIXEN_MAX_WAIT_MILLISECONDS: u64 = 60_000;

const MAX_INPUT_VIEWPORT_DIMENSION: u32 = 4096;
const MAX_INPUT_VIEWPORT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_KEY_BYTES: usize = 256;
const MAX_CODE_BYTES: usize = 256;
const MAX_TEXT_BYTES: usize = 4096;
const MAX_TEXT_INPUT_BYTES: usize = 16 * 1024;
const FFI_INVALID_ARGUMENT: &str = "ffi.invalid-argument";
const FFI_INVALID_UTF8: &str = "ffi.invalid-utf8";
const FFI_INPUT_TOO_LARGE: &str = "ffi.input-too-large";
const FFI_OUTPUT_TOO_LARGE: &str = "ffi.output-too-large";
const FFI_BUFFER_LIMIT: &str = "ffi.buffer-limit";
const FFI_INVALID_COMMAND: &str = "ffi.invalid-command";
const FFI_UNKNOWN_HANDLE: &str = "ffi.unknown-handle";
const FFI_INTERNAL: &str = "ffi.internal";

/// Rust-owned output bytes. A zeroed descriptor means no output.
///
/// A nonzero token identifies the allocation until exactly one successful call
/// to [`vixen_buffer_release`]. Callers must not mutate or free `ptr`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VixenBuffer {
    pub token: u64,
    pub ptr: *const u8,
    pub len: usize,
}

impl VixenBuffer {
    pub(crate) const EMPTY: Self = Self {
        token: 0,
        ptr: ptr::null(),
        len: 0,
    };
}

pub(crate) struct ControllerState {
    pub(crate) controller: FlutterBrowserController,
    next_event_sequence: u64,
}

#[derive(Default)]
pub(crate) struct RendererState {
    pub(crate) replica: RenderReplica,
    pub(crate) commits: RenderCommitState,
    pub(crate) source: Option<vixen_api::FullRenderSnapshot>,
    pub(crate) needs_resync: bool,
}

pub(crate) struct ControllerEntry {
    pub(crate) state: Mutex<ControllerState>,
    pub(crate) renderer: crate::RenderBroker,
    pub(crate) renderer_state: Arc<Mutex<RendererState>>,
    pub(crate) cdp: Mutex<Option<crate::cdp_host::CdpHost>>,
}

pub(crate) type SharedControllerEntry = Arc<ControllerEntry>;

static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);
static NEXT_BUFFER: AtomicU64 = AtomicU64::new(1);
static CONTROLLERS: OnceLock<Mutex<HashMap<u64, SharedControllerEntry>>> = OnceLock::new();
static BUFFERS: OnceLock<Mutex<HashMap<u64, Box<[u8]>>>> = OnceLock::new();

fn controllers() -> &'static Mutex<HashMap<u64, SharedControllerEntry>> {
    CONTROLLERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn buffers() -> &'static Mutex<HashMap<u64, Box<[u8]>>> {
    BUFFERS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Debug)]
pub(crate) struct AbiError {
    pub(crate) status: u32,
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl AbiError {
    pub(crate) fn new(status: u32, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub(crate) fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(VIXEN_STATUS_INVALID_ARGUMENT, FFI_INVALID_ARGUMENT, message)
    }

    pub(crate) fn invalid_command(message: impl Into<String>) -> Self {
        Self::new(VIXEN_STATUS_INVALID_COMMAND, FFI_INVALID_COMMAND, message)
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::new(VIXEN_STATUS_INTERNAL_ERROR, FFI_INTERNAL, message)
    }
}

/// Return ABI version 1. Zero is reserved for panic containment failure.
#[unsafe(no_mangle)]
pub extern "C" fn vixen_abi_version() -> u32 {
    catch_unwind(|| ABI_VERSION).unwrap_or(0)
}

/// Open one profile and return a registry-backed opaque browser handle.
///
/// # Safety
///
/// `profile_path` must address `profile_path_len` readable bytes. `out_handle`
/// and `out_json` must each address writable values for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vixen_open(
    profile_path: *const u8,
    profile_path_len: usize,
    out_handle: *mut u64,
    out_json: *mut VixenBuffer,
) -> u32 {
    ffi_boundary(|| {
        if !initialize_output(out_json) {
            return VIXEN_STATUS_INVALID_ARGUMENT;
        }
        if out_handle.is_null() {
            return finish(
                Err(AbiError::invalid_argument(
                    "output handle pointer must not be null",
                )),
                out_json,
            );
        }
        unsafe { out_handle.write(0) };

        let result = (|| {
            let profile_path = copy_utf8_input(
                profile_path,
                profile_path_len,
                VIXEN_MAX_PROFILE_PATH_BYTES,
                "profile path",
            )?;
            if profile_path.is_empty() {
                return Err(AbiError::invalid_argument("profile path must not be empty"));
            }
            let renderer = crate::RenderBroker::new();
            let renderer_state = Arc::new(Mutex::new(RendererState::default()));
            let mut config = crate::BrowserConfig::new(profile_path);
            config.synchronous_renderer = Some(Arc::new(
                crate::sync_renderer::FlutterSynchronousRenderer::new(
                    renderer.clone(),
                    Arc::clone(&renderer_state),
                ),
            ));
            let controller =
                FlutterBrowserController::from_config(config).map_err(browser_error)?;
            let handle = next_token(&NEXT_HANDLE, "browser handle")?;
            let entry = Arc::new(ControllerEntry {
                state: Mutex::new(ControllerState {
                    controller,
                    next_event_sequence: 1,
                }),
                renderer,
                renderer_state,
                cdp: Mutex::new(None),
            });
            controllers()
                .lock()
                .map_err(|_| AbiError::internal("browser registry is unavailable"))?
                .insert(handle, entry);
            if let Err(error) = write_json(out_json, &json!({"v": ABI_VERSION, "type": "opened"})) {
                controllers()
                    .lock()
                    .map_err(|_| AbiError::internal("browser registry is unavailable"))?
                    .remove(&handle);
                return Err(error);
            }
            unsafe { out_handle.write(handle) };
            Ok(())
        })();
        finish(result, out_json)
    })
}

/// Destroy a browser handle. Zero, unknown, and repeated handles fail safely.
#[unsafe(no_mangle)]
pub extern "C" fn vixen_destroy(handle: u64) -> u32 {
    ffi_boundary(|| {
        if handle == 0 {
            return VIXEN_STATUS_UNKNOWN_HANDLE;
        }
        let removed = match controllers().lock() {
            Ok(mut registry) => registry.remove(&handle),
            Err(_) => return VIXEN_STATUS_INTERNAL_ERROR,
        };
        if let Some(entry) = removed {
            if let Ok(mut cdp) = entry.cdp.lock()
                && let Some(mut cdp) = cdp.take()
            {
                cdp.shutdown();
            }
            let _ = entry.renderer.shutdown();
            VIXEN_STATUS_OK
        } else {
            VIXEN_STATUS_UNKNOWN_HANDLE
        }
    })
}

/// Parse and dispatch one bounded JSON v1 command.
///
/// # Safety
///
/// `message` must address `message_len` readable bytes and `out_json` must
/// address a writable [`VixenBuffer`] for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vixen_command(
    handle: u64,
    message: *const u8,
    message_len: usize,
    out_json: *mut VixenBuffer,
) -> u32 {
    ffi_boundary(|| {
        if !initialize_output(out_json) {
            return VIXEN_STATUS_INVALID_ARGUMENT;
        }
        let result = (|| {
            let message = copy_utf8_input(
                message,
                message_len,
                VIXEN_MAX_MESSAGE_BYTES,
                "command message",
            )?;
            let command = parse_command(&message)?;
            let entry = controller_entry(handle)?;
            if let ControllerCommand::StartCdp { port } = &command {
                let mut cdp = entry
                    .cdp
                    .lock()
                    .map_err(|_| AbiError::internal("CDP host is unavailable"))?;
                if cdp.is_some() {
                    return Err(AbiError::invalid_command("CDP host is already running"));
                }
                *cdp = Some(
                    crate::cdp_host::CdpHost::start(&entry, *port)
                        .map_err(AbiError::invalid_command)?,
                );
                return write_json(
                    out_json,
                    &json!({
                        "v": ABI_VERSION,
                        "type": "response",
                        "response": {"type": "accepted"},
                    }),
                );
            }
            let mut state = entry
                .state
                .lock()
                .map_err(|_| AbiError::internal("browser handle is unavailable"))?;
            {
                let mut renderer_state = entry
                    .renderer_state
                    .lock()
                    .map_err(|_| AbiError::internal("renderer acceptance state is unavailable"))?;
                let commits = drain_renderer_submissions(&entry.renderer, &mut renderer_state)?;
                drop(renderer_state);
                for commit in commits {
                    state
                        .controller
                        .apply_renderer_commit(commit)
                        .map_err(browser_error)?;
                }
            }
            let response = match command {
                ControllerCommand::DispatchRendererMouseEvent(dispatch) => {
                    let crate::RendererMouseEventDispatch {
                        mouse,
                        query,
                        target,
                    } = *dispatch;
                    let target_node_id = {
                        let renderer_state = entry.renderer_state.lock().map_err(|_| {
                            AbiError::internal("renderer acceptance state is unavailable")
                        })?;
                        renderer_state
                            .commits
                            .validate_hit_test_query(&renderer_state.replica, query)
                            .map_err(render_protocol_error)?;
                        let presented =
                            renderer_state.commits.presented_commit().ok_or_else(|| {
                                AbiError::invalid_command(
                                    "renderer mouse input requires a presented commit",
                                )
                            })?;
                        if query.context_id != mouse.context_id
                            || query.document_id != mouse.document_id
                            || query.point.x != mouse.event.x
                            || query.point.y != mouse.event.y
                            || presented.viewport.width != mouse.viewport.0
                            || presented.viewport.height != mouse.viewport.1
                        {
                            return Err(AbiError::invalid_command(
                                "renderer mouse input does not match its query and viewport",
                            ));
                        }
                        if let Some(target) = target {
                            renderer_state
                                .commits
                                .validate_input_target(&renderer_state.replica, &query, target)
                                .map_err(render_protocol_error)?;
                        }
                        target
                            .and_then(|target| {
                                renderer_state
                                    .replica
                                    .nearest_semantic_node_id(target.node_id)
                            })
                            .map(|node_id| {
                                usize::try_from(node_id.get()).map_err(|_| {
                                    AbiError::invalid_command(
                                        "renderer input target node_id must fit usize",
                                    )
                                })
                            })
                            .transpose()?
                    };
                    state
                        .controller
                        .dispatch_renderer_mouse_event(mouse, target_node_id)
                        .map_err(browser_error)?
                }
                command => state.controller.dispatch(command).map_err(browser_error)?,
            };
            let response = match response {
                ControllerResponse::RendererUpdate(snapshot) => {
                    let mut renderer_state = entry.renderer_state.lock().map_err(|_| {
                        AbiError::internal("renderer acceptance state is unavailable")
                    })?;
                    crate::sync_renderer::publish_renderer_source(
                        &entry.renderer,
                        &mut renderer_state,
                        snapshot,
                        false,
                    )
                    .map_err(synchronous_renderer_error)?;
                    ControllerResponse::Accepted
                }
                response => response,
            };
            write_json(
                out_json,
                &json!({
                    "v": ABI_VERSION,
                    "type": "response",
                    "response": response_json(response),
                }),
            )
        })();
        finish(result, out_json)
    })
}

pub(crate) fn drain_renderer_submissions(
    renderer: &crate::RenderBroker,
    state: &mut RendererState,
) -> Result<Vec<RenderCommit>, AbiError> {
    let mut accepted_commits = Vec::new();
    while let Some(submission) = renderer.peek_submission().map_err(renderer_broker_error)? {
        let (next_commits, releases, accepted_commit) = match &submission {
            RenderBridgeSubmission::Commit(commit) => {
                let mut commits = state.commits.clone();
                let releases = match commits.accept_commit(&state.replica, commit.clone()) {
                    Ok(releases) => releases,
                    Err(error) => {
                        renderer
                            .consume_submission_with_updates(
                                &submission,
                                [RenderBridgeUpdate::ReleaseHandles(RenderHandleRelease {
                                    version: vixen_api::RENDER_PROTOCOL_VERSION,
                                    commit_id: commit.commit_id,
                                    hit_test_handle: commit.hit_test_handle,
                                    text_query_handle: commit.text_query_handle,
                                })],
                            )
                            .map_err(renderer_broker_error)?;
                        return Err(render_protocol_error(error));
                    }
                };
                (Some(commits), releases, Some(commit.clone()))
            }
            RenderBridgeSubmission::Presented(presented) => {
                let mut commits = state.commits.clone();
                let releases = match commits.accept_presented(&state.replica, *presented) {
                    Ok(releases) => releases,
                    Err(error) => {
                        renderer
                            .consume_submission_with_updates(&submission, [])
                            .map_err(renderer_broker_error)?;
                        return Err(render_protocol_error(error));
                    }
                };
                (Some(commits), releases, None)
            }
            RenderBridgeSubmission::Resync(_) => {
                state.needs_resync = true;
                state.commits = RenderCommitState::default();
                (None, Vec::new(), None)
            }
        };
        renderer
            .consume_submission_with_updates(
                &submission,
                releases.into_iter().map(RenderBridgeUpdate::ReleaseHandles),
            )
            .map_err(renderer_broker_error)?;
        if let Some(commits) = next_commits {
            state.commits = commits;
        }
        if let Some(commit) = accepted_commit {
            accepted_commits.push(commit);
        }
    }
    Ok(accepted_commits)
}

fn render_protocol_error(error: vixen_api::RenderProtocolError) -> AbiError {
    AbiError::invalid_command(format!("{}: {}", error.code, error.message))
}

fn renderer_broker_error(error: crate::RenderBrokerError) -> AbiError {
    AbiError::invalid_command(format!("{}: {}", error.code, error.message))
}

fn synchronous_renderer_error(error: vixen_engine::browser::SynchronousRendererError) -> AbiError {
    AbiError::invalid_command(format!("{}: {}", error.code, error.message))
}

/// Consume the next ordered event without blocking.
///
/// # Safety
///
/// `out_json` must address a writable [`VixenBuffer`] for the duration of the
/// call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vixen_poll_event(handle: u64, out_json: *mut VixenBuffer) -> u32 {
    ffi_boundary(|| event_impl(handle, None, out_json))
}

/// Wait up to `timeout_milliseconds` for the next ordered event.
///
/// # Safety
///
/// `out_json` must address a writable [`VixenBuffer`] for the duration of the
/// call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vixen_wait_event(
    handle: u64,
    timeout_milliseconds: u64,
    out_json: *mut VixenBuffer,
) -> u32 {
    ffi_boundary(|| {
        if timeout_milliseconds > VIXEN_MAX_WAIT_MILLISECONDS {
            if !initialize_output(out_json) {
                return VIXEN_STATUS_INVALID_ARGUMENT;
            }
            return finish(
                Err(AbiError::invalid_argument(format!(
                    "wait timeout exceeds {VIXEN_MAX_WAIT_MILLISECONDS} milliseconds"
                ))),
                out_json,
            );
        }
        event_impl(
            handle,
            Some(Duration::from_millis(timeout_milliseconds)),
            out_json,
        )
    })
}

/// Release one Rust-owned output allocation by token.
#[unsafe(no_mangle)]
pub extern "C" fn vixen_buffer_release(token: u64) -> u32 {
    ffi_boundary(|| {
        if token == 0 {
            return VIXEN_STATUS_UNKNOWN_BUFFER;
        }
        let removed = match buffers().lock() {
            Ok(mut registry) => registry.remove(&token),
            Err(_) => return VIXEN_STATUS_INTERNAL_ERROR,
        };
        if removed.is_some() {
            VIXEN_STATUS_OK
        } else {
            VIXEN_STATUS_UNKNOWN_BUFFER
        }
    })
}

fn ffi_boundary(operation: impl FnOnce() -> u32) -> u32 {
    catch_unwind(AssertUnwindSafe(operation)).unwrap_or(VIXEN_STATUS_PANIC)
}

fn event_impl(handle: u64, timeout: Option<Duration>, out_json: *mut VixenBuffer) -> u32 {
    if !initialize_output(out_json) {
        return VIXEN_STATUS_INVALID_ARGUMENT;
    }
    let result = (|| {
        let entry = controller_entry(handle)?;
        let mut state = entry
            .state
            .lock()
            .map_err(|_| AbiError::internal("browser handle is unavailable"))?;
        let event = match timeout {
            Some(timeout) => state.controller.wait_next_event(timeout),
            None => state.controller.try_next_event(),
        }
        .map_err(browser_error)?;
        let Some(event) = event else {
            return Ok(false);
        };
        let sequence = state.next_event_sequence;
        let next_sequence = sequence
            .checked_add(1)
            .ok_or_else(|| AbiError::internal("event sequence exhausted"))?;
        write_json(
            out_json,
            &json!({
                "v": ABI_VERSION,
                "type": "event",
                "sequence": sequence,
                "event": event_json(event),
            }),
        )?;
        state.next_event_sequence = next_sequence;
        Ok(true)
    })();
    match result {
        Ok(true) => VIXEN_STATUS_OK,
        Ok(false) => VIXEN_STATUS_NO_EVENT,
        Err(error) => finish(Err(error), out_json),
    }
}

pub(crate) fn initialize_output(out_json: *mut VixenBuffer) -> bool {
    if out_json.is_null() {
        return false;
    }
    unsafe { out_json.write(VixenBuffer::EMPTY) };
    true
}

pub(crate) fn finish(result: Result<(), AbiError>, out_json: *mut VixenBuffer) -> u32 {
    match result {
        Ok(()) => VIXEN_STATUS_OK,
        Err(error) => {
            let status = error.status;
            if let Err(write_error) = write_json(
                out_json,
                &json!({
                    "v": ABI_VERSION,
                    "type": "error",
                    "error": {"code": error.code, "message": error.message},
                }),
            ) {
                return write_error.status;
            }
            status
        }
    }
}

pub(crate) fn write_json(out_json: *mut VixenBuffer, value: &Value) -> Result<(), AbiError> {
    let reservation = reserve_buffer()?;
    write_reserved_json(out_json, reservation, value)
}

pub(crate) struct BufferReservation {
    token: u64,
    committed: bool,
}

impl Drop for BufferReservation {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if let Ok(mut registry) = buffers().lock() {
            registry.remove(&self.token);
        }
    }
}

pub(crate) fn reserve_buffer() -> Result<BufferReservation, AbiError> {
    let mut registry = buffers()
        .lock()
        .map_err(|_| AbiError::internal("buffer registry is unavailable"))?;
    if registry.len() >= VIXEN_MAX_OUTSTANDING_BUFFERS {
        return Err(AbiError::new(
            VIXEN_STATUS_BUFFER_LIMIT,
            FFI_BUFFER_LIMIT,
            format!("outstanding output buffer limit of {VIXEN_MAX_OUTSTANDING_BUFFERS} reached"),
        ));
    }
    let token = next_token(&NEXT_BUFFER, "buffer token")?;
    registry.insert(token, Vec::new().into_boxed_slice());
    Ok(BufferReservation {
        token,
        committed: false,
    })
}

pub(crate) fn write_reserved_json(
    out_json: *mut VixenBuffer,
    mut reservation: BufferReservation,
    value: &Value,
) -> Result<(), AbiError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| AbiError::internal(format!("could not encode JSON output: {error}")))?;
    if bytes.len() > VIXEN_MAX_OUTPUT_BYTES {
        return Err(AbiError::new(
            VIXEN_STATUS_OUTPUT_TOO_LARGE,
            FFI_OUTPUT_TOO_LARGE,
            format!("JSON output exceeds {VIXEN_MAX_OUTPUT_BYTES} bytes"),
        ));
    }
    let allocation = bytes.into_boxed_slice();
    let mut registry = buffers()
        .lock()
        .map_err(|_| AbiError::internal("buffer registry is unavailable"))?;
    let token = reservation.token;
    let Some(retained) = registry.get_mut(&token) else {
        return Err(AbiError::internal("reserved output buffer is unavailable"));
    };
    let descriptor = VixenBuffer {
        token,
        ptr: allocation.as_ptr(),
        len: allocation.len(),
    };
    *retained = allocation;
    unsafe { out_json.write(descriptor) };
    reservation.committed = true;
    Ok(())
}

pub(crate) fn next_token(counter: &AtomicU64, kind: &str) -> Result<u64, AbiError> {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| AbiError::internal(format!("{kind} exhausted")))
}

pub(crate) fn controller_entry(handle: u64) -> Result<SharedControllerEntry, AbiError> {
    if handle == 0 {
        return Err(AbiError::new(
            VIXEN_STATUS_UNKNOWN_HANDLE,
            FFI_UNKNOWN_HANDLE,
            "browser handle is zero",
        ));
    }
    controllers()
        .lock()
        .map_err(|_| AbiError::internal("browser registry is unavailable"))?
        .get(&handle)
        .cloned()
        .ok_or_else(|| {
            AbiError::new(
                VIXEN_STATUS_UNKNOWN_HANDLE,
                FFI_UNKNOWN_HANDLE,
                "browser handle is unknown or destroyed",
            )
        })
}

pub(crate) fn browser_error(error: BrowserError) -> AbiError {
    AbiError::new(VIXEN_STATUS_BROWSER_ERROR, error.code, error.message)
}

pub(crate) fn copy_utf8_input(
    input: *const u8,
    len: usize,
    maximum: usize,
    name: &str,
) -> Result<String, AbiError> {
    if input.is_null() {
        return Err(AbiError::invalid_argument(format!(
            "{name} pointer must not be null"
        )));
    }
    if len > maximum {
        return Err(AbiError::new(
            VIXEN_STATUS_INPUT_TOO_LARGE,
            FFI_INPUT_TOO_LARGE,
            format!("{name} exceeds {maximum} bytes"),
        ));
    }
    let copied = unsafe { std::slice::from_raw_parts(input, len) }.to_vec();
    String::from_utf8(copied).map_err(|_| {
        AbiError::new(
            VIXEN_STATUS_INVALID_UTF8,
            FFI_INVALID_UTF8,
            format!("{name} is not valid UTF-8"),
        )
    })
}

fn parse_command(message: &str) -> Result<ControllerCommand, AbiError> {
    let value: Value = serde_json::from_str(message)
        .map_err(|error| AbiError::invalid_command(format!("invalid JSON: {error}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| AbiError::invalid_command("command must be a JSON object"))?;
    if required_u64(object, "v")? != u64::from(ABI_VERSION) {
        return Err(AbiError::invalid_command("command version must be 1"));
    }
    let command_type = required_string(object, "type")?;
    let command = match command_type {
        "load_profile_session" => {
            exact_keys(object, &["type", "v"])?;
            ControllerCommand::LoadProfileSession
        }
        "start_cdp" => {
            exact_keys(object, &["port", "type", "v"])?;
            let port = u16::try_from(required_u64(object, "port")?)
                .map_err(|_| AbiError::invalid_command("CDP port must fit unsigned 16 bits"))?;
            if port == 0 {
                return Err(AbiError::invalid_command("CDP port must be nonzero"));
            }
            ControllerCommand::StartCdp { port }
        }
        "save_current_profile_session" => {
            exact_keys(object, &["type", "v"])?;
            ControllerCommand::SaveCurrentProfileSession
        }
        "browser_snapshot" => {
            exact_keys(object, &["type", "v"])?;
            ControllerCommand::BrowserSnapshot
        }
        "create_context" => {
            exact_keys(object, &["type", "v"])?;
            ControllerCommand::CreateContext
        }
        "close_context" => {
            exact_keys(object, &["context_id", "type", "v"])?;
            ControllerCommand::CloseContext(required_context_id(object)?)
        }
        "activate_context" => {
            exact_keys(object, &["context_id", "type", "v"])?;
            ControllerCommand::ActivateContext(required_context_id(object)?)
        }
        "navigate" => {
            exact_keys(object, &["context_id", "type", "url", "v"])?;
            ControllerCommand::Navigate {
                context_id: required_context_id(object)?,
                url: required_string(object, "url")?.to_owned(),
            }
        }
        "reload" => {
            exact_keys(object, &["context_id", "type", "v"])?;
            ControllerCommand::Reload(required_context_id(object)?)
        }
        "stop" => {
            exact_keys(object, &["context_id", "type", "v"])?;
            ControllerCommand::Stop(required_context_id(object)?)
        }
        "traverse_history" => {
            exact_keys(object, &["context_id", "delta", "type", "v"])?;
            let delta = required_i64(object, "delta")?;
            ControllerCommand::TraverseHistory {
                context_id: required_context_id(object)?,
                delta: i32::try_from(delta)
                    .map_err(|_| AbiError::invalid_command("delta must fit signed 32 bits"))?,
            }
        }
        "context_state" => {
            exact_keys(object, &["context_id", "type", "v"])?;
            ControllerCommand::ContextState(required_context_id(object)?)
        }
        "find_text" => {
            exact_keys(
                object,
                &[
                    "case_sensitive",
                    "context_id",
                    "document_id",
                    "forward",
                    "query",
                    "type",
                    "v",
                ],
            )?;
            ControllerCommand::FindText {
                context_id: required_context_id(object)?,
                document_id: required_document_id(object)?,
                query: bounded_string(object, "query", MAX_TEXT_BYTES)?,
                case_sensitive: required_bool(object, "case_sensitive")?,
                forward: required_bool(object, "forward")?,
            }
        }
        "set_page_zoom" => {
            exact_keys(object, &["context_id", "type", "v", "zoom"])?;
            ControllerCommand::SetPageZoom {
                context_id: required_context_id(object)?,
                zoom: required_f64(object, "zoom")?,
            }
        }
        "update_host_view_state" => {
            exact_keys(
                object,
                &[
                    "context_id",
                    "focused",
                    "generation",
                    "lifecycle",
                    "scale_factor",
                    "type",
                    "v",
                    "viewport",
                    "visible",
                ],
            )?;
            let generation = required_u64(object, "generation")?;
            if generation == 0 {
                return Err(AbiError::invalid_command(
                    "host view generation must be nonzero",
                ));
            }
            let lifecycle = match required_string(object, "lifecycle")? {
                "resumed" => HostLifecycle::Resumed,
                "inactive" => HostLifecycle::Inactive,
                "hidden" => HostLifecycle::Hidden,
                "paused" => HostLifecycle::Paused,
                "detached" => HostLifecycle::Detached,
                _ => return Err(AbiError::invalid_command("unsupported host lifecycle")),
            };
            let scale_factor = required_f64(object, "scale_factor")?;
            if !(0.1..=16.0).contains(&scale_factor) {
                return Err(AbiError::invalid_command(
                    "host view scale_factor must be between 0.1 and 16",
                ));
            }
            ControllerCommand::UpdateHostViewState {
                context_id: required_context_id(object)?,
                state: HostViewState {
                    generation,
                    viewport: required_viewport(object)?,
                    scale_factor,
                    focused: required_bool(object, "focused")?,
                    visible: required_bool(object, "visible")?,
                    lifecycle,
                },
            }
        }
        "accessibility_snapshot" => {
            exact_keys(
                object,
                &["context_id", "document_id", "type", "v", "viewport"],
            )?;
            ControllerCommand::AccessibilitySnapshot {
                context_id: required_context_id(object)?,
                document_id: required_document_id(object)?,
                viewport: required_viewport(object)?,
            }
        }
        "publish_renderer_snapshot" => {
            exact_keys(
                object,
                &[
                    "context_id",
                    "document_id",
                    "page_zoom",
                    "type",
                    "v",
                    "viewport",
                    "viewport_generation",
                ],
            )?;
            let viewport_generation = required_u64(object, "viewport_generation")?;
            if viewport_generation == 0 {
                return Err(AbiError::invalid_command(
                    "renderer viewport_generation must be nonzero",
                ));
            }
            ControllerCommand::PublishRendererSnapshot {
                context_id: required_context_id(object)?,
                document_id: required_document_id(object)?,
                viewport: required_viewport(object)?,
                viewport_generation,
                page_zoom: required_f64(object, "page_zoom")?,
            }
        }
        "flush_renderer_submissions" => {
            exact_keys(object, &["type", "v"])?;
            ControllerCommand::FlushRendererSubmissions
        }
        "dispatch_accessibility_action" => {
            let action = match required_string(object, "action")? {
                "focus" => {
                    exact_keys(
                        object,
                        &[
                            "action",
                            "context_id",
                            "document_id",
                            "generation",
                            "node_id",
                            "runtime_context_id",
                            "source_generation",
                            "type",
                            "v",
                            "viewport",
                        ],
                    )?;
                    AccessibilityAction::Focus
                }
                "set_value" => {
                    exact_keys(
                        object,
                        &[
                            "action",
                            "context_id",
                            "document_id",
                            "generation",
                            "node_id",
                            "runtime_context_id",
                            "source_generation",
                            "type",
                            "v",
                            "value",
                            "viewport",
                        ],
                    )?;
                    AccessibilityAction::SetValue(bounded_string(
                        object,
                        "value",
                        ACCESSIBILITY_MAX_VALUE_BYTES,
                    )?)
                }
                "increase" | "decrease" => {
                    exact_keys(
                        object,
                        &[
                            "action",
                            "context_id",
                            "document_id",
                            "generation",
                            "node_id",
                            "runtime_context_id",
                            "source_generation",
                            "type",
                            "v",
                            "viewport",
                        ],
                    )?;
                    if required_string(object, "action")? == "increase" {
                        AccessibilityAction::Increase
                    } else {
                        AccessibilityAction::Decrease
                    }
                }
                _ => {
                    return Err(AbiError::invalid_command(
                        "unsupported accessibility action",
                    ));
                }
            };
            let source_generation = required_u64(object, "source_generation")?;
            let generation = required_u64(object, "generation")?;
            let node_id = usize::try_from(required_u64(object, "node_id")?)
                .map_err(|_| AbiError::invalid_command("node_id must fit usize"))?;
            if source_generation == 0 || generation == 0 || node_id == 0 {
                return Err(AbiError::invalid_command(
                    "accessibility generations and node_id must be nonzero",
                ));
            }
            ControllerCommand::DispatchAccessibilityAction {
                context_id: required_context_id(object)?,
                document_id: required_document_id(object)?,
                runtime_context_id: required_runtime_context_id(object)?,
                viewport: required_viewport(object)?,
                source_generation,
                generation,
                node_id,
                action,
            }
        }
        "dispatch_renderer_mouse_event" => {
            exact_keys(
                object,
                &[
                    "context_id",
                    "document_id",
                    "event",
                    "event_type",
                    "query",
                    "runtime_context_id",
                    "target",
                    "type",
                    "v",
                    "viewport",
                ],
            )?;
            let event_type = required_string(object, "event_type")?;
            if !matches!(
                event_type,
                "mousemove" | "mousedown" | "mouseup" | "wheel" | "cancel"
            ) {
                return Err(AbiError::invalid_command(
                    "event_type must be mousemove, mousedown, mouseup, wheel, or cancel",
                ));
            }
            let query =
                crate::render_wire::parse_hit_test_query(required_object(object, "query")?)?;
            let target = match object.get("target") {
                Some(Value::Null) => None,
                Some(Value::Object(target)) => {
                    Some(crate::render_wire::parse_input_target(target)?)
                }
                _ => {
                    return Err(AbiError::invalid_command(
                        "renderer mouse target must be an object or null",
                    ));
                }
            };
            ControllerCommand::DispatchRendererMouseEvent(Box::new(
                crate::RendererMouseEventDispatch {
                    mouse: crate::MouseEventDispatch {
                        context_id: required_context_id(object)?,
                        document_id: required_document_id(object)?,
                        runtime_context_id: required_runtime_context_id(object)?,
                        viewport: required_viewport(object)?,
                        event_type: event_type.to_owned(),
                        event: required_mouse_event(object)?,
                    },
                    query,
                    target,
                },
            ))
        }
        "dispatch_key_event" => {
            exact_keys(
                object,
                &[
                    "context_id",
                    "document_id",
                    "event",
                    "event_type",
                    "runtime_context_id",
                    "type",
                    "v",
                    "viewport",
                ],
            )?;
            let event_type = required_string(object, "event_type")?;
            if !matches!(event_type, "keydown" | "keyup") {
                return Err(AbiError::invalid_command(
                    "event_type must be keydown or keyup",
                ));
            }
            ControllerCommand::DispatchKeyEvent {
                context_id: required_context_id(object)?,
                document_id: required_document_id(object)?,
                runtime_context_id: required_runtime_context_id(object)?,
                viewport: required_viewport(object)?,
                event_type: event_type.to_owned(),
                event: required_key_event(object)?,
            }
        }
        "dispatch_text_input" => {
            exact_keys(
                object,
                &[
                    "context_id",
                    "document_id",
                    "runtime_context_id",
                    "state",
                    "type",
                    "v",
                    "viewport",
                ],
            )?;
            ControllerCommand::DispatchTextInput {
                context_id: required_context_id(object)?,
                document_id: required_document_id(object)?,
                runtime_context_id: required_runtime_context_id(object)?,
                viewport: required_viewport(object)?,
                state: required_text_input_state(object)?,
            }
        }
        _ => return Err(AbiError::invalid_command("unknown command type")),
    };
    Ok(command)
}

fn exact_keys(object: &Map<String, Value>, expected: &[&str]) -> Result<(), AbiError> {
    if object.len() != expected.len() || !expected.iter().all(|key| object.contains_key(*key)) {
        return Err(AbiError::invalid_command(
            "command has missing or unknown fields",
        ));
    }
    Ok(())
}

fn required_string<'a>(object: &'a Map<String, Value>, field: &str) -> Result<&'a str, AbiError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| AbiError::invalid_command(format!("{field} must be a string")))
}

fn required_u64(object: &Map<String, Value>, field: &str) -> Result<u64, AbiError> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| AbiError::invalid_command(format!("{field} must be an unsigned integer")))
}

fn required_i64(object: &Map<String, Value>, field: &str) -> Result<i64, AbiError> {
    object
        .get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| AbiError::invalid_command(format!("{field} must be an integer")))
}

fn required_f64(object: &Map<String, Value>, field: &str) -> Result<f64, AbiError> {
    let value = object
        .get(field)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .ok_or_else(|| AbiError::invalid_command(format!("{field} must be a finite number")))?;
    Ok(value)
}

fn required_bool(object: &Map<String, Value>, field: &str) -> Result<bool, AbiError> {
    object
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| AbiError::invalid_command(format!("{field} must be a boolean")))
}

fn required_object<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a Map<String, Value>, AbiError> {
    object
        .get(field)
        .and_then(Value::as_object)
        .ok_or_else(|| AbiError::invalid_command(format!("{field} must be an object")))
}

fn required_context_id(object: &Map<String, Value>) -> Result<BrowsingContextId, AbiError> {
    BrowsingContextId::new(required_u64(object, "context_id")?)
        .ok_or_else(|| AbiError::invalid_command("context_id must be nonzero"))
}

fn required_document_id(object: &Map<String, Value>) -> Result<vixen_api::DocumentId, AbiError> {
    vixen_api::DocumentId::new(required_u64(object, "document_id")?)
        .ok_or_else(|| AbiError::invalid_command("document_id must be nonzero"))
}

fn required_runtime_context_id(
    object: &Map<String, Value>,
) -> Result<vixen_api::RuntimeContextId, AbiError> {
    vixen_api::RuntimeContextId::new(required_u64(object, "runtime_context_id")?)
        .ok_or_else(|| AbiError::invalid_command("runtime_context_id must be nonzero"))
}

fn required_viewport(object: &Map<String, Value>) -> Result<(u32, u32), AbiError> {
    let viewport = required_object(object, "viewport")?;
    exact_keys(viewport, &["height", "width"])?;
    let width = u32::try_from(required_u64(viewport, "width")?)
        .map_err(|_| AbiError::invalid_command("viewport width must fit unsigned 32 bits"))?;
    let height = u32::try_from(required_u64(viewport, "height")?)
        .map_err(|_| AbiError::invalid_command("viewport height must fit unsigned 32 bits"))?;
    let rgba_bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| AbiError::invalid_command("viewport area overflows"))?;
    if width == 0
        || height == 0
        || width > MAX_INPUT_VIEWPORT_DIMENSION
        || height > MAX_INPUT_VIEWPORT_DIMENSION
        || rgba_bytes > MAX_INPUT_VIEWPORT_BYTES
    {
        return Err(AbiError::invalid_command(
            "viewport must have positive dimensions no larger than 4096 and bounded area",
        ));
    }
    Ok((width, height))
}

fn required_mouse_event(object: &Map<String, Value>) -> Result<MouseEventData, AbiError> {
    let event = required_object(object, "event")?;
    exact_keys(
        event,
        &[
            "alt_key",
            "bubbles",
            "button",
            "buttons",
            "ctrl_key",
            "delta_x",
            "delta_y",
            "detail",
            "meta_key",
            "shift_key",
            "x",
            "y",
        ],
    )?;
    let button = i32::try_from(required_i64(event, "button")?)
        .map_err(|_| AbiError::invalid_command("button must fit signed 32 bits"))?;
    Ok(MouseEventData {
        x: required_f64(event, "x")?,
        y: required_f64(event, "y")?,
        button,
        buttons: required_i64(event, "buttons")?,
        detail: required_i64(event, "detail")?,
        related_node_id: None,
        bubbles: required_bool(event, "bubbles")?,
        ctrl_key: required_bool(event, "ctrl_key")?,
        shift_key: required_bool(event, "shift_key")?,
        alt_key: required_bool(event, "alt_key")?,
        meta_key: required_bool(event, "meta_key")?,
        delta_x: required_f64(event, "delta_x")?,
        delta_y: required_f64(event, "delta_y")?,
    })
}

fn required_key_event(object: &Map<String, Value>) -> Result<KeyEventData, AbiError> {
    let event = required_object(object, "event")?;
    exact_keys(
        event,
        &[
            "alt_key",
            "apply_text",
            "code",
            "ctrl_key",
            "key",
            "location",
            "meta_key",
            "repeat",
            "shift_key",
            "text",
        ],
    )?;
    let key = bounded_string(event, "key", MAX_KEY_BYTES)?;
    let code = bounded_string(event, "code", MAX_CODE_BYTES)?;
    let text = bounded_string(event, "text", MAX_TEXT_BYTES)?;
    Ok(KeyEventData {
        key,
        code,
        text,
        apply_text: required_bool(event, "apply_text")?,
        ctrl_key: required_bool(event, "ctrl_key")?,
        shift_key: required_bool(event, "shift_key")?,
        alt_key: required_bool(event, "alt_key")?,
        meta_key: required_bool(event, "meta_key")?,
        repeat: required_bool(event, "repeat")?,
        location: required_i64(event, "location")?,
    })
}

fn required_text_input_state(object: &Map<String, Value>) -> Result<TextInputState, AbiError> {
    let state = required_object(object, "state")?;
    exact_keys(state, &["composing", "selection", "text"])?;
    let text = bounded_string(state, "text", MAX_TEXT_INPUT_BYTES)?;
    let selection = required_text_range(state, "selection")?
        .ok_or_else(|| AbiError::invalid_command("text input selection must be an object"))?;
    let composing = required_text_range(state, "composing")?;
    let utf16_len = u32::try_from(text.encode_utf16().count())
        .map_err(|_| AbiError::invalid_command("text input exceeds the UTF-16 range limit"))?;
    if selection.base_offset > utf16_len || selection.extent_offset > utf16_len {
        return Err(AbiError::invalid_command(
            "text input selection exceeds the value",
        ));
    }
    if composing.is_some_and(|range| {
        range.base_offset > range.extent_offset || range.extent_offset > utf16_len
    }) {
        return Err(AbiError::invalid_command(
            "text input composing range is invalid",
        ));
    }
    Ok(TextInputState {
        text,
        selection,
        composing,
    })
}

fn required_text_range(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<AccessibilityTextSelection>, AbiError> {
    let Some(value) = object.get(field) else {
        return Err(AbiError::invalid_command(format!("{field} is required")));
    };
    if value.is_null() {
        return Ok(None);
    }
    let range = value
        .as_object()
        .ok_or_else(|| AbiError::invalid_command(format!("{field} must be an object or null")))?;
    exact_keys(range, &["base_offset", "extent_offset"])?;
    let base_offset = u32::try_from(required_u64(range, "base_offset")?)
        .map_err(|_| AbiError::invalid_command("text range offset must fit unsigned 32 bits"))?;
    let extent_offset = u32::try_from(required_u64(range, "extent_offset")?)
        .map_err(|_| AbiError::invalid_command("text range offset must fit unsigned 32 bits"))?;
    Ok(Some(AccessibilityTextSelection {
        base_offset,
        extent_offset,
    }))
}

fn bounded_string(
    object: &Map<String, Value>,
    field: &str,
    maximum: usize,
) -> Result<String, AbiError> {
    let value = required_string(object, field)?;
    if value.len() > maximum {
        return Err(AbiError::invalid_command(format!(
            "{field} exceeds {maximum} UTF-8 bytes"
        )));
    }
    Ok(value.to_owned())
}

fn response_json(response: ControllerResponse) -> Value {
    match response {
        ControllerResponse::Accepted => json!({"type": "accepted"}),
        ControllerResponse::ProfileSession(session) => json!({
            "type": "profile_session",
            "tabs": session.tabs,
            "active_index": session.active_index,
        }),
        ControllerResponse::BrowserSnapshot(snapshot) => json!({
            "type": "browser_snapshot",
            "active_context_id": optional_id(snapshot.active_context_id),
            "contexts": snapshot.contexts.into_iter().map(context_state_json).collect::<Vec<_>>(),
        }),
        ControllerResponse::ContextCreated(context_id) => {
            json!({"type": "context_created", "context_id": context_id.get()})
        }
        ControllerResponse::NavigationAccepted(navigation_id) => json!({
            "type": "navigation_accepted",
            "navigation_id": navigation_id.get(),
        }),
        ControllerResponse::ContextState(state) => json!({
            "type": "context_state",
            "state": context_state_json(state),
        }),
        ControllerResponse::AccessibilitySnapshot(snapshot) => {
            accessibility_snapshot_json(snapshot)
        }
        ControllerResponse::InputDispatched(result) => input_dispatch_result_json(result),
        ControllerResponse::FindText(result) => json!({
            "type": "find_text",
            "matches": result.matches,
            "active_match": result.active_match,
        }),
        ControllerResponse::RendererUpdate(_) => json!({"type": "accepted"}),
    }
}

fn accessibility_snapshot_json(snapshot: AccessibilitySnapshot) -> Value {
    let snapshot = bounded_accessibility_snapshot(snapshot);
    json!({
        "type": "accessibility_snapshot",
        "context_id": snapshot.context_id.get(),
        "document_id": snapshot.document_id.get(),
        "source_generation": snapshot.source_generation,
        "generation": snapshot.generation,
        "viewport": {
            "width": snapshot.viewport.0,
            "height": snapshot.viewport.1,
        },
        "nodes": snapshot.nodes.into_iter().map(accessibility_node_json).collect::<Vec<_>>(),
        "truncated": snapshot.truncated,
    })
}

fn accessibility_node_json(node: AccessibilityNode) -> Value {
    json!({
        "id": node.id,
        "parent_id": node.parent_id,
        "controls_ids": node.controls_ids,
        "described_by_ids": node.described_by_ids,
        "details_ids": node.details_ids,
        "owns_ids": node.owns_ids,
        "role": node.role,
        "label": node.label,
        "description": node.description,
        "value": node.value,
        "text_selection": node.text_selection.map(|selection| json!({
            "base_offset": selection.base_offset,
            "extent_offset": selection.extent_offset,
        })),
        "multiline": node.multiline,
        "text_input_type": node.text_input_type.map(|value| value.as_str()),
        "text_input_action": node.text_input_action.map(|value| value.as_str()),
        "range": node.range.map(|range| json!({
            "current": range.current,
            "minimum": range.minimum,
            "maximum": range.maximum,
            "step": range.step,
        })),
        "bbox": node.bbox.map(|bbox| json!({
            "x": bbox.x,
            "y": bbox.y,
            "width": bbox.width,
            "height": bbox.height,
        })),
        "focused": node.focused,
        "disabled": node.disabled,
        "checked": node.checked,
        "mixed": node.mixed,
        "selected": node.selected,
        "expanded": node.expanded,
        "heading_level": node.heading_level,
        "hidden": node.hidden,
        "live_region": node.live_region,
        "focusable": node.focusable,
        "actions": node.actions,
    })
}

fn input_dispatch_result_json(result: InputDispatchResult) -> Value {
    json!({
        "type": "input_dispatched",
        "effects": runtime_effects_json(result.effects),
        "navigation_actions": result
            .navigation_actions
            .into_iter()
            .map(navigation_action_json)
            .collect::<Vec<_>>(),
    })
}

fn navigation_action_json(action: NavigationActionOutcome) -> Value {
    match action {
        NavigationActionOutcome::SameDocument { url } => json!({
            "type": "same_document",
            "url": url,
        }),
        NavigationActionOutcome::CrossDocument {
            navigation_id,
            kind,
        } => json!({
            "type": "cross_document",
            "navigation_id": navigation_id.get(),
            "kind": navigation_kind_json(kind),
        }),
    }
}

fn context_state_json(state: BrowsingContextState) -> Value {
    json!({
        "context_id": state.context_id.get(),
        "main_frame_id": state.main_frame_id.get(),
        "document_id": state.document_id.get(),
        "runtime_context_id": optional_id(state.runtime_context_id),
        "active_navigation_id": optional_id(state.active_navigation_id),
        "url": state.url,
        "title": state.title,
        "history_length": state.history_length,
        "history_index": state.history_index,
        "can_go_back": state.can_go_back,
        "can_go_forward": state.can_go_forward,
        "is_loading": state.is_loading,
        "load_progress": state.load_progress,
        "page_zoom": state.page_zoom,
    })
}

fn optional_id<T: Into<u64>>(id: Option<T>) -> Option<u64> {
    id.map(Into::into)
}

fn event_json(event: BrowserEvent) -> Value {
    match event {
        BrowserEvent::BrowsingContextCreated { state } => json!({
            "type": "browsing_context_created",
            "state": context_state_json(state),
        }),
        BrowserEvent::BrowsingContextClosed { context_id } => json!({
            "type": "browsing_context_closed",
            "context_id": context_id.get(),
        }),
        BrowserEvent::ActiveBrowsingContextChanged { context_id } => json!({
            "type": "active_browsing_context_changed",
            "context_id": optional_id(context_id),
        }),
        BrowserEvent::NavigationRequested {
            context_id,
            frame_id,
            navigation_id,
            predecessor_navigation_id,
            kind,
            url,
        } => json!({
            "type": "navigation_requested",
            "context_id": context_id.get(),
            "frame_id": frame_id.get(),
            "navigation_id": navigation_id.get(),
            "predecessor_navigation_id": optional_id(predecessor_navigation_id),
            "kind": navigation_kind_json(kind),
            "url": url,
        }),
        BrowserEvent::NavigationStarted {
            context_id,
            frame_id,
            navigation_id,
            request_id,
            url,
        } => json!({
            "type": "navigation_started",
            "context_id": context_id.get(),
            "frame_id": frame_id.get(),
            "navigation_id": navigation_id.get(),
            "request_id": request_id.get(),
            "url": url,
        }),
        BrowserEvent::NavigationRedirected {
            context_id,
            frame_id,
            navigation_id,
            request_id,
            next_request_id,
            from_url,
            to_url,
            status,
        } => json!({
            "type": "navigation_redirected",
            "context_id": context_id.get(),
            "frame_id": frame_id.get(),
            "navigation_id": navigation_id.get(),
            "request_id": request_id.get(),
            "next_request_id": next_request_id.get(),
            "from_url": from_url,
            "to_url": to_url,
            "status": status,
        }),
        BrowserEvent::NavigationPhaseChanged {
            context_id,
            frame_id,
            navigation_id,
            phase,
        } => json!({
            "type": "navigation_phase_changed",
            "context_id": context_id.get(),
            "frame_id": frame_id.get(),
            "navigation_id": navigation_id.get(),
            "phase": navigation_phase_name(phase),
        }),
        BrowserEvent::RuntimeContextDestroyed {
            context_id,
            frame_id,
            document_id,
            runtime_context_id,
        } => json!({
            "type": "runtime_context_destroyed",
            "context_id": context_id.get(),
            "frame_id": frame_id.get(),
            "document_id": document_id.get(),
            "runtime_context_id": runtime_context_id.get(),
        }),
        BrowserEvent::DocumentDiscarded {
            context_id,
            frame_id,
            document_id,
            replaced_by,
        } => json!({
            "type": "document_discarded",
            "context_id": context_id.get(),
            "frame_id": frame_id.get(),
            "document_id": document_id.get(),
            "replaced_by": optional_id(replaced_by),
        }),
        BrowserEvent::NavigationCommitted {
            context_id,
            frame_id,
            navigation_id,
            request_id,
            document_id,
            runtime_context_id,
            url,
        } => json!({
            "type": "navigation_committed",
            "context_id": context_id.get(),
            "frame_id": frame_id.get(),
            "navigation_id": navigation_id.get(),
            "request_id": optional_id(request_id),
            "document_id": document_id.get(),
            "runtime_context_id": optional_id(runtime_context_id),
            "url": url,
        }),
        BrowserEvent::RuntimeContextCreated {
            context_id,
            frame_id,
            document_id,
            runtime_context_id,
        } => json!({
            "type": "runtime_context_created",
            "context_id": context_id.get(),
            "frame_id": frame_id.get(),
            "document_id": document_id.get(),
            "runtime_context_id": runtime_context_id.get(),
        }),
        BrowserEvent::RuntimeEffects {
            context_id,
            frame_id,
            document_id,
            runtime_context_id,
            url,
            effects,
        } => json!({
            "type": "runtime_effects",
            "context_id": context_id.get(),
            "frame_id": frame_id.get(),
            "document_id": document_id.get(),
            "runtime_context_id": runtime_context_id.get(),
            "url": url,
            "effects": runtime_effects_json(effects),
        }),
        BrowserEvent::DomContentLoaded {
            context_id,
            frame_id,
            navigation_id,
            document_id,
        } => json!({
            "type": "dom_content_loaded",
            "context_id": context_id.get(),
            "frame_id": frame_id.get(),
            "navigation_id": navigation_id.get(),
            "document_id": document_id.get(),
        }),
        BrowserEvent::DocumentLoadCompleted {
            context_id,
            frame_id,
            navigation_id,
            document_id,
        } => json!({
            "type": "document_load_completed",
            "context_id": context_id.get(),
            "frame_id": frame_id.get(),
            "navigation_id": navigation_id.get(),
            "document_id": document_id.get(),
        }),
        BrowserEvent::NavigationCancelled {
            context_id,
            frame_id,
            navigation_id,
            request_id,
            reason,
        } => json!({
            "type": "navigation_cancelled",
            "context_id": context_id.get(),
            "frame_id": frame_id.get(),
            "navigation_id": navigation_id.get(),
            "request_id": optional_id(request_id),
            "reason": cancellation_reason_name(reason),
        }),
        BrowserEvent::NavigationFailed {
            context_id,
            frame_id,
            navigation_id,
            request_id,
            error,
        } => json!({
            "type": "navigation_failed",
            "context_id": context_id.get(),
            "frame_id": frame_id.get(),
            "navigation_id": navigation_id.get(),
            "request_id": optional_id(request_id),
            "error": browser_error_json(error),
        }),
        BrowserEvent::BrowsingContextStateChanged { state } => json!({
            "type": "browsing_context_state_changed",
            "state": context_state_json(state),
        }),
        BrowserEvent::Download {
            source_context_id,
            source_document_id,
            event,
        } => json!({
            "type": "download",
            "source_context_id": optional_id(source_context_id),
            "source_document_id": optional_id(source_document_id),
            "download": download_json(event),
        }),
        BrowserEvent::Diagnostic { scope, diagnostic } => json!({
            "type": "diagnostic",
            "scope": diagnostic_scope_json(scope),
            "diagnostic": diagnostic_json(diagnostic),
        }),
    }
}

fn navigation_kind_json(kind: CrossDocumentNavigationKind) -> Value {
    match kind {
        CrossDocumentNavigationKind::Regular => json!({"type": "regular"}),
        CrossDocumentNavigationKind::ContentReplacement {
            replaced_document_id,
        } => json!({
            "type": "content_replacement",
            "replaced_document_id": replaced_document_id.get(),
        }),
    }
}

fn navigation_phase_name(phase: NavigationPhase) -> &'static str {
    match phase {
        NavigationPhase::Intent => "intent",
        NavigationPhase::Policy => "policy",
        NavigationPhase::Request => "request",
        NavigationPhase::Response => "response",
        NavigationPhase::Commit => "commit",
        NavigationPhase::Parse => "parse",
        NavigationPhase::ScriptsAndSubresources => "scripts_and_subresources",
        NavigationPhase::DomContentLoaded => "dom_content_loaded",
        NavigationPhase::Load => "load",
        NavigationPhase::Settled => "settled",
        NavigationPhase::Failed => "failed",
        NavigationPhase::Cancelled => "cancelled",
    }
}

fn cancellation_reason_name(reason: NavigationCancellationReason) -> &'static str {
    match reason {
        NavigationCancellationReason::Stopped => "stopped",
        NavigationCancellationReason::Superseded => "superseded",
        NavigationCancellationReason::ContextClosed => "context_closed",
        NavigationCancellationReason::BrowserShutdown => "browser_shutdown",
    }
}

fn browser_error_json(error: BrowserError) -> Value {
    json!({"code": error.code, "message": error.message})
}

fn runtime_effects_json(effects: RuntimeEffects) -> Value {
    json!({
        "console": effects.console.into_iter().map(|event| json!({
            "kind": event.kind,
            "args": event.args.into_iter().map(console_arg_json).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "dialogs": effects.dialogs.into_iter().map(|event| json!({
            "kind": event.kind,
            "message": event.message,
            "default_prompt": event.default_prompt,
        })).collect::<Vec<_>>(),
        "bindings": effects.bindings.into_iter().map(|event| json!({
            "name": event.name,
            "payload": event.payload,
        })).collect::<Vec<_>>(),
        "network": effects.network.into_iter().map(network_event_json).collect::<Vec<_>>(),
        "exceptions": effects.exceptions.into_iter().map(|event| browser_error_json(event.error)).collect::<Vec<_>>(),
    })
}

fn console_arg_json(argument: RuntimeConsoleArg) -> Value {
    json!({
        "type_name": argument.type_name,
        "subtype": argument.subtype,
        "value": argument.value.map(console_value_json),
        "unserializable_value": argument.unserializable_value,
        "description": argument.description,
    })
}

fn console_value_json(value: RuntimeConsoleValue) -> Value {
    match value {
        RuntimeConsoleValue::String(value) => json!({"type": "string", "value": value}),
        RuntimeConsoleValue::Number(value) => json!({"type": "number", "value": value}),
        RuntimeConsoleValue::Bool(value) => json!({"type": "bool", "value": value}),
        RuntimeConsoleValue::Null => json!({"type": "null"}),
    }
}

fn network_event_json(event: RuntimeNetworkEvent) -> Value {
    match event {
        RuntimeNetworkEvent::Request {
            request_id,
            url,
            method,
        } => json!({
            "type": "request",
            "request_id": request_id,
            "url": url,
            "method": method,
        }),
        RuntimeNetworkEvent::Redirect {
            request_id,
            from,
            to,
            status,
        } => json!({
            "type": "redirect",
            "request_id": request_id,
            "from": from,
            "to": to,
            "status": status,
        }),
        RuntimeNetworkEvent::Response {
            request_id,
            url,
            status,
        } => json!({
            "type": "response",
            "request_id": request_id,
            "url": url,
            "status": status,
        }),
        RuntimeNetworkEvent::Failure {
            request_id,
            url,
            error_text,
            blocked_reason,
        } => json!({
            "type": "failure",
            "request_id": request_id,
            "url": url,
            "error_text": error_text,
            "blocked_reason": blocked_reason,
        }),
    }
}

fn download_json(event: DownloadEvent) -> Value {
    match event {
        DownloadEvent::Started {
            id,
            filename,
            total_bytes,
            mime,
        } => json!({
            "type": "started",
            "download_id": id.get(),
            "filename": filename,
            "total_bytes": total_bytes,
            "mime": mime,
        }),
        DownloadEvent::Progress {
            id,
            received_bytes,
            total_bytes,
        } => json!({
            "type": "progress",
            "download_id": id.get(),
            "received_bytes": received_bytes,
            "total_bytes": total_bytes,
        }),
        DownloadEvent::Completed { id } => {
            json!({"type": "completed", "download_id": id.get()})
        }
        DownloadEvent::Cancelled { id } => {
            json!({"type": "cancelled", "download_id": id.get()})
        }
        DownloadEvent::Failed { id, message } => json!({
            "type": "failed",
            "download_id": id.get(),
            "message": message,
        }),
    }
}

fn diagnostic_scope_json(scope: DiagnosticScope) -> Value {
    json!({
        "profile_id": optional_id(scope.profile_id),
        "browser_id": optional_id(scope.browser_id),
        "context_id": optional_id(scope.context_id),
        "frame_id": optional_id(scope.frame_id),
        "navigation_id": optional_id(scope.navigation_id),
        "document_id": optional_id(scope.document_id),
        "request_id": optional_id(scope.request_id),
        "runtime_context_id": optional_id(scope.runtime_context_id),
        "download_id": optional_id(scope.download_id),
    })
}

fn diagnostic_json(diagnostic: EngineDiagnostic) -> Value {
    json!({
        "category": diagnostic_category_name(diagnostic.category),
        "code": diagnostic.code,
        "message": diagnostic.message,
    })
}

fn diagnostic_category_name(category: EngineDiagnosticCategory) -> &'static str {
    match category {
        EngineDiagnosticCategory::Network => "network",
        EngineDiagnosticCategory::ParseDom => "parse_dom",
        EngineDiagnosticCategory::ScriptRuntime => "script_runtime",
        EngineDiagnosticCategory::LayoutRender => "layout_render",
        EngineDiagnosticCategory::StorageCache => "storage_cache",
    }
}

#[cfg(test)]
mod tests {
    use std::mem::{align_of, offset_of, size_of};
    use std::path::PathBuf;
    use std::sync::MutexGuard;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_PROFILE: AtomicU64 = AtomicU64::new(1);
    static ABI_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    struct TestScope {
        _guard: MutexGuard<'static, ()>,
    }

    impl Drop for TestScope {
        fn drop(&mut self) {
            assert!(controllers().lock().unwrap().is_empty());
            assert!(buffers().lock().unwrap().is_empty());
        }
    }

    struct TestProfile(PathBuf);

    impl TestProfile {
        fn new() -> Self {
            let serial = NEXT_PROFILE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("vixen-c-abi-{}-{serial}.redb", std::process::id()));
            let _ = std::fs::remove_file(&path);
            Self(path)
        }

        fn utf8(&self) -> &str {
            self.0.to_str().unwrap()
        }
    }

    impl Drop for TestProfile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    struct Handle(u64);

    impl Drop for Handle {
        fn drop(&mut self) {
            if self.0 != 0 {
                assert_eq!(vixen_destroy(self.0), VIXEN_STATUS_OK);
            }
        }
    }

    #[test]
    fn version_and_c_layout_are_stable() {
        let _scope = test_scope();
        assert_eq!(vixen_abi_version(), 1);
        assert_eq!(size_of::<u64>(), 8);
        assert_eq!(offset_of!(VixenBuffer, token), 0);
        assert_eq!(offset_of!(VixenBuffer, ptr), 8);
        assert_eq!(offset_of!(VixenBuffer, len), 8 + size_of::<*const u8>());
        assert!(size_of::<VixenBuffer>() >= 8 + size_of::<*const u8>() + size_of::<usize>());
        assert!(align_of::<VixenBuffer>() >= align_of::<u64>());
    }

    #[test]
    fn checked_in_header_matches_exported_v1_surface() {
        let _scope = test_scope();
        let header = include_str!("../include/vixen.h");
        for declaration in [
            "#define VIXEN_ABI_VERSION 1u",
            "#define VIXEN_MAX_PROFILE_PATH_BYTES 4096u",
            "#define VIXEN_MAX_MESSAGE_BYTES 65536u",
            "#define VIXEN_MAX_OUTPUT_BYTES 1048576u",
            "#define VIXEN_MAX_OUTSTANDING_BUFFERS 64u",
            "#define VIXEN_MAX_WAIT_MILLISECONDS 60000u",
            "#define VIXEN_MAX_RENDER_UPDATE_SOURCE_BYTES 524288u",
            "uint32_t vixen_abi_version(void);",
            "uint32_t vixen_destroy(VixenHandle handle);",
            "uint32_t vixen_poll_event(VixenHandle handle, VixenBuffer *out_json);",
            "uint32_t vixen_buffer_release(uint64_t token);",
        ] {
            assert!(header.contains(declaration), "missing {declaration}");
        }
        assert!(header.contains("uint32_t vixen_open(const uint8_t *profile_path,"));
        assert!(header.contains("uint32_t vixen_command(VixenHandle handle,"));
        assert!(header.contains("uint32_t vixen_wait_event(VixenHandle handle,"));
        assert!(header.contains("uint32_t vixen_renderer_poll(VixenHandle handle,"));
        assert!(header.contains("uint32_t vixen_renderer_respond(VixenHandle handle,"));
        assert!(header.contains("uint32_t vixen_renderer_submit(VixenHandle handle,"));
        assert!(header.contains("uint32_t vixen_renderer_shutdown(VixenHandle handle,"));
        assert_eq!(VIXEN_MAX_PROFILE_PATH_BYTES, 4096);
        assert_eq!(VIXEN_MAX_MESSAGE_BYTES, 65_536);
        assert_eq!(VIXEN_MAX_OUTPUT_BYTES, 1_048_576);
        assert_eq!(VIXEN_MAX_OUTSTANDING_BUFFERS, 64);
        assert_eq!(VIXEN_MAX_WAIT_MILLISECONDS, 60_000);
        assert_eq!(crate::RENDER_BROKER_MAX_UPDATE_SOURCE_BYTES, 512 * 1024);
    }

    #[test]
    fn null_invalid_utf8_and_oversized_inputs_fail_closed() {
        let _scope = test_scope();
        let mut handle = 77;
        let mut output = VixenBuffer::EMPTY;
        let profile = b"unused.redb";
        assert_eq!(
            unsafe {
                vixen_open(
                    profile.as_ptr(),
                    profile.len(),
                    ptr::null_mut(),
                    &mut output,
                )
            },
            VIXEN_STATUS_INVALID_ARGUMENT
        );
        assert_error_and_release(output, FFI_INVALID_ARGUMENT);
        assert_eq!(
            unsafe { vixen_open(ptr::null(), 0, &mut handle, &mut output) },
            VIXEN_STATUS_INVALID_ARGUMENT
        );
        assert_eq!(handle, 0);
        assert_error_and_release(output, FFI_INVALID_ARGUMENT);

        let invalid = [0xff];
        assert_eq!(
            unsafe { vixen_open(invalid.as_ptr(), invalid.len(), &mut handle, &mut output) },
            VIXEN_STATUS_INVALID_UTF8
        );
        assert_error_and_release(output, FFI_INVALID_UTF8);

        let byte = [b'x'];
        assert_eq!(
            unsafe {
                vixen_open(
                    byte.as_ptr(),
                    VIXEN_MAX_PROFILE_PATH_BYTES + 1,
                    &mut handle,
                    &mut output,
                )
            },
            VIXEN_STATUS_INPUT_TOO_LARGE
        );
        assert_error_and_release(output, FFI_INPUT_TOO_LARGE);

        assert_eq!(
            unsafe { vixen_command(1, ptr::null(), 0, &mut output) },
            VIXEN_STATUS_INVALID_ARGUMENT
        );
        assert_error_and_release(output, FFI_INVALID_ARGUMENT);
        assert_eq!(
            unsafe { vixen_command(1, profile.as_ptr(), profile.len(), ptr::null_mut()) },
            VIXEN_STATUS_INVALID_ARGUMENT
        );
        assert_eq!(
            unsafe { vixen_command(1, invalid.as_ptr(), invalid.len(), &mut output) },
            VIXEN_STATUS_INVALID_UTF8
        );
        assert_error_and_release(output, FFI_INVALID_UTF8);
        assert_eq!(
            unsafe { vixen_command(1, byte.as_ptr(), VIXEN_MAX_MESSAGE_BYTES + 1, &mut output,) },
            VIXEN_STATUS_INPUT_TOO_LARGE
        );
        assert_error_and_release(output, FFI_INPUT_TOO_LARGE);
    }

    #[test]
    fn unknown_and_double_handle_destruction_are_safe() {
        let _scope = test_scope();
        assert_eq!(vixen_destroy(0), VIXEN_STATUS_UNKNOWN_HANDLE);
        assert_eq!(vixen_destroy(u64::MAX), VIXEN_STATUS_UNKNOWN_HANDLE);

        let profile = TestProfile::new();
        let mut handle = open(&profile);
        assert_eq!(vixen_destroy(handle.0), VIXEN_STATUS_OK);
        assert_eq!(vixen_destroy(handle.0), VIXEN_STATUS_UNKNOWN_HANDLE);
        handle.0 = 0;
    }

    #[test]
    fn buffer_release_is_tokenized_and_double_release_is_safe() {
        let _scope = test_scope();
        let profile = TestProfile::new();
        let handle = open(&profile);
        let output = command(handle.0, json!({"v": 1, "type": "create_context"}));
        assert!(!output.ptr.is_null());
        assert_ne!(output.token, 0);
        assert_ne!(output.len, 0);
        assert_eq!(vixen_buffer_release(output.token), VIXEN_STATUS_OK);
        assert_eq!(
            vixen_buffer_release(output.token),
            VIXEN_STATUS_UNKNOWN_BUFFER
        );
        assert_eq!(vixen_buffer_release(0), VIXEN_STATUS_UNKNOWN_BUFFER);
    }

    #[test]
    fn oversized_output_is_rejected_before_registration() {
        let _scope = test_scope();
        let mut output = VixenBuffer::EMPTY;
        let value = json!({"value": "x".repeat(VIXEN_MAX_OUTPUT_BYTES)});
        let error = write_json(&mut output, &value).unwrap_err();
        assert_eq!(error.status, VIXEN_STATUS_OUTPUT_TOO_LARGE);
        assert_eq!(error.code, FFI_OUTPUT_TOO_LARGE);
        assert_eq!(output.token, 0);
        assert!(buffers().lock().unwrap().is_empty());
    }

    #[test]
    fn outstanding_output_allocations_are_globally_bounded() {
        let _scope = test_scope();
        let mut outputs = Vec::new();
        for index in 0..VIXEN_MAX_OUTSTANDING_BUFFERS {
            let mut output = VixenBuffer::EMPTY;
            write_json(&mut output, &json!({"index": index})).unwrap();
            outputs.push(output);
        }

        let mut rejected = VixenBuffer::EMPTY;
        let error = write_json(&mut rejected, &json!({"overflow": true})).unwrap_err();
        assert_eq!(error.status, VIXEN_STATUS_BUFFER_LIMIT);
        assert_eq!(error.code, FFI_BUFFER_LIMIT);
        assert_eq!(rejected.token, 0);

        for output in outputs {
            assert_eq!(vixen_buffer_release(output.token), VIXEN_STATUS_OK);
        }
    }

    #[test]
    fn create_navigate_and_event_flow_has_typed_ids_and_sequences() {
        let _scope = test_scope();
        let profile = TestProfile::new();
        let handle = open(&profile);
        let created = take_json(command(handle.0, json!({"v": 1, "type": "create_context"})));
        let context_id = created["response"]["context_id"].as_u64().unwrap();
        assert_ne!(context_id, 0);

        let mut sequences = Vec::new();
        drain_events(handle.0, &mut sequences);
        let navigated = take_json(command(
            handle.0,
            json!({
                "v": 1,
                "type": "navigate",
                "context_id": context_id,
                "url": "about:blank",
            }),
        ));
        let navigation_id = navigated["response"]["navigation_id"].as_u64().unwrap();
        assert_ne!(navigation_id, 0);

        let mut saw_typed_navigation = false;
        let mut saw_settled = false;
        for _ in 0..64 {
            let mut output = VixenBuffer::EMPTY;
            let status = unsafe { vixen_wait_event(handle.0, 1_000, &mut output) };
            assert_eq!(status, VIXEN_STATUS_OK);
            let event = take_json(output);
            sequences.push(event["sequence"].as_u64().unwrap());
            if event["event"]["navigation_id"].as_u64() == Some(navigation_id) {
                saw_typed_navigation = true;
            }
            if event["event"]["type"] == "navigation_phase_changed"
                && event["event"]["navigation_id"].as_u64() == Some(navigation_id)
                && event["event"]["phase"] == "settled"
            {
                saw_settled = true;
                break;
            }
        }
        assert!(saw_typed_navigation);
        assert!(saw_settled);
        assert!(sequences.windows(2).all(|pair| pair[1] == pair[0] + 1));
    }

    #[test]
    fn no_event_poll_returns_zero_output() {
        let _scope = test_scope();
        let profile = TestProfile::new();
        let handle = open(&profile);
        let mut sequences = Vec::new();
        drain_events(handle.0, &mut sequences);
        let mut output = VixenBuffer {
            token: 9,
            ptr: ptr::dangling(),
            len: 9,
        };
        assert_eq!(
            unsafe { vixen_poll_event(handle.0, &mut output) },
            VIXEN_STATUS_NO_EVENT
        );
        assert_eq!(output.token, 0);
        assert!(output.ptr.is_null());
        assert_eq!(output.len, 0);
    }

    #[test]
    fn invalid_context_ids_and_browser_errors_are_stable_json() {
        let _scope = test_scope();
        let profile = TestProfile::new();
        let handle = open(&profile);
        let mut output = VixenBuffer::EMPTY;
        let zero = serde_json::to_vec(&json!({
            "v": 1,
            "type": "context_state",
            "context_id": 0,
        }))
        .unwrap();
        assert_eq!(
            unsafe { vixen_command(handle.0, zero.as_ptr(), zero.len(), &mut output) },
            VIXEN_STATUS_INVALID_COMMAND
        );
        assert_error_and_release(output, FFI_INVALID_COMMAND);

        let unknown = serde_json::to_vec(&json!({
            "v": 1,
            "type": "context_state",
            "context_id": u64::MAX,
        }))
        .unwrap();
        assert_eq!(
            unsafe { vixen_command(handle.0, unknown.as_ptr(), unknown.len(), &mut output) },
            VIXEN_STATUS_BROWSER_ERROR
        );
        let error = take_json(output);
        assert_eq!(error["type"], "error");
        assert_eq!(error["error"]["code"], "browser.unknown-context");
        assert!(error["error"]["message"].is_string());
    }

    #[test]
    fn accessibility_command_is_strict_and_c_abi_json_is_exact() {
        let _scope = test_scope();
        let parsed = parse_command(
            &json!({
                "v": 1,
                "type": "accessibility_snapshot",
                "context_id": 2,
                "document_id": 3,
                "viewport": {"width": 320, "height": 240},
            })
            .to_string(),
        )
        .unwrap();
        assert!(matches!(
            parsed,
            ControllerCommand::AccessibilitySnapshot {
                context_id,
                document_id,
                viewport: (320, 240),
            } if context_id.get() == 2 && document_id.get() == 3
        ));
        for invalid in [
            json!({
                "v": 1,
                "type": "accessibility_snapshot",
                "context_id": 0,
                "document_id": 3,
                "viewport": {"width": 320, "height": 240},
            }),
            json!({
                "v": 1,
                "type": "accessibility_snapshot",
                "context_id": 2,
                "document_id": 0,
                "viewport": {"width": 320, "height": 240},
            }),
            json!({
                "v": 1,
                "type": "accessibility_snapshot",
                "context_id": 2,
                "document_id": 3,
                "viewport": {"width": 4097, "height": 240},
            }),
            json!({
                "v": 1,
                "type": "accessibility_snapshot",
                "context_id": 2,
                "document_id": 3,
                "viewport": {"width": 320, "height": 240},
                "extra": true,
            }),
        ] {
            assert!(
                parse_command(&invalid.to_string()).is_err(),
                "accepted {invalid}"
            );
        }

        let profile = TestProfile::new();
        let handle = open(&profile);
        let created = take_json(command(handle.0, json!({"v": 1, "type": "create_context"})));
        let context_id = created["response"]["context_id"].as_u64().unwrap();
        let state = take_json(command(
            handle.0,
            json!({"v": 1, "type": "context_state", "context_id": context_id}),
        ));
        let document_id = state["response"]["state"]["document_id"].as_u64().unwrap();
        let actual = take_json(command(
            handle.0,
            json!({
                "v": 1,
                "type": "accessibility_snapshot",
                "context_id": context_id,
                "document_id": document_id,
                "viewport": {"width": 320, "height": 240},
            }),
        ));
        let generation = actual["response"]["generation"].as_u64().unwrap();
        let source_generation = actual["response"]["source_generation"].as_u64().unwrap();
        assert_ne!(generation, 0);
        assert_ne!(source_generation, 0);
        assert_eq!(
            actual,
            json!({
                "v": 1,
                "type": "response",
                "response": {
                    "type": "accessibility_snapshot",
                    "context_id": context_id,
                    "document_id": document_id,
                    "source_generation": source_generation,
                    "generation": generation,
                    "viewport": {"width": 320, "height": 240},
                    "nodes": [],
                    "truncated": false,
                },
            })
        );

        let mut output = VixenBuffer::EMPTY;
        let stale = serde_json::to_vec(&json!({
            "v": 1,
            "type": "accessibility_snapshot",
            "context_id": context_id,
            "document_id": document_id + 1,
            "viewport": {"width": 320, "height": 240},
        }))
        .unwrap();
        assert_eq!(
            unsafe { vixen_command(handle.0, stale.as_ptr(), stale.len(), &mut output) },
            VIXEN_STATUS_BROWSER_ERROR
        );
        let error = take_json(output);
        assert_eq!(error["error"]["code"], "browser.stale-document");
    }

    #[test]
    fn accessibility_action_command_is_strict_and_bounded() {
        let _scope = test_scope();
        let value = json!({
            "v": 1,
            "type": "dispatch_accessibility_action",
            "context_id": 2,
            "document_id": 3,
            "runtime_context_id": 4,
            "viewport": {"width": 320, "height": 240},
            "source_generation": 5,
            "generation": 6,
            "node_id": 7,
            "action": "focus",
        });
        let parsed = parse_command(&value.to_string()).unwrap();
        assert!(matches!(
            parsed,
            ControllerCommand::DispatchAccessibilityAction {
                context_id,
                document_id,
                runtime_context_id,
                viewport: (320, 240),
                source_generation: 5,
                generation: 6,
                node_id: 7,
                action: AccessibilityAction::Focus,
            } if context_id.get() == 2
                && document_id.get() == 3
                && runtime_context_id.get() == 4
        ));

        let mut set_value = value.clone();
        set_value["action"] = json!("set_value");
        set_value["value"] = json!("Ada");
        assert!(matches!(
            parse_command(&set_value.to_string()).unwrap(),
            ControllerCommand::DispatchAccessibilityAction {
                action: AccessibilityAction::SetValue(value),
                ..
            } if value == "Ada"
        ));
        for (name, expected) in [
            ("increase", AccessibilityAction::Increase),
            ("decrease", AccessibilityAction::Decrease),
        ] {
            let mut adjustment = value.clone();
            adjustment["action"] = json!(name);
            assert!(matches!(
                parse_command(&adjustment.to_string()).unwrap(),
                ControllerCommand::DispatchAccessibilityAction { action, .. }
                    if action == expected
            ));
        }
        set_value["value"] = json!("x".repeat(ACCESSIBILITY_MAX_VALUE_BYTES + 1));
        assert!(parse_command(&set_value.to_string()).is_err());

        for invalid in [
            ("source_generation", json!(0)),
            ("generation", json!(0)),
            ("node_id", json!(0)),
            ("action", json!("set_value")),
            ("viewport", json!({"width": 0, "height": 240})),
        ] {
            let mut invalid_value = value.clone();
            invalid_value[invalid.0] = invalid.1;
            assert!(
                parse_command(&invalid_value.to_string()).is_err(),
                "accepted {invalid_value}"
            );
        }
        let mut extra = value;
        extra["extra"] = json!(true);
        assert!(parse_command(&extra.to_string()).is_err());
    }

    #[test]
    fn host_view_command_is_strict_and_generation_tagged() {
        let value = json!({
            "v": 1,
            "type": "update_host_view_state",
            "context_id": 2,
            "generation": 3,
            "viewport": {"width": 640, "height": 360},
            "scale_factor": 2.0,
            "focused": false,
            "visible": false,
            "lifecycle": "hidden",
        });
        assert!(matches!(
            parse_command(&value.to_string()).unwrap(),
            ControllerCommand::UpdateHostViewState {
                context_id,
                state: HostViewState {
                    generation: 3,
                    viewport: (640, 360),
                    scale_factor: 2.0,
                    focused: false,
                    visible: false,
                    lifecycle: HostLifecycle::Hidden,
                },
            } if context_id.get() == 2
        ));
        for (field, invalid) in [
            ("generation", json!(0)),
            ("scale_factor", json!(0)),
            ("lifecycle", json!("background")),
            ("focused", json!("false")),
        ] {
            let mut command = value.clone();
            command[field] = invalid;
            assert!(parse_command(&command.to_string()).is_err());
        }
        let mut extra = value;
        extra["extra"] = json!(true);
        assert!(parse_command(&extra.to_string()).is_err());
    }

    #[test]
    fn accessibility_response_projects_all_fields_and_stays_under_output_cap() {
        let node = AccessibilityNode {
            id: 4,
            parent_id: Some(2),
            controls_ids: vec![],
            described_by_ids: vec![],
            details_ids: vec![],
            owns_ids: vec![],
            role: "checkbox".to_owned(),
            label: "Remember me".to_owned(),
            description: "Account preference".to_owned(),
            value: Some("yes".to_owned()),
            text_selection: Some(vixen_api::AccessibilityTextSelection {
                base_offset: 1,
                extent_offset: 3,
            }),
            multiline: false,
            text_input_type: Some(vixen_api::AccessibilityTextInputType::Email),
            text_input_action: Some(vixen_api::AccessibilityTextInputAction::Send),
            range: None,
            bbox: Some(vixen_api::AccessibilityRect {
                x: 1.5,
                y: 2.5,
                width: 30.0,
                height: 40.0,
            }),
            focused: true,
            disabled: false,
            checked: Some(true),
            mixed: None,
            selected: true,
            expanded: Some(false),
            heading_level: None,
            hidden: false,
            live_region: true,
            focusable: true,
            actions: vec!["tap".to_owned()],
        };
        let projected = response_json(ControllerResponse::AccessibilitySnapshot(
            AccessibilitySnapshot {
                context_id: BrowsingContextId::new(2).unwrap(),
                document_id: vixen_api::DocumentId::new(3).unwrap(),
                source_generation: 1,
                generation: 1,
                viewport: (320, 240),
                nodes: vec![node.clone()],
                truncated: false,
            },
        ));
        let generation = projected["generation"].as_u64().unwrap();
        assert_ne!(generation, 0);
        assert_eq!(
            projected,
            json!({
                "type": "accessibility_snapshot",
                "context_id": 2,
                "document_id": 3,
                "source_generation": 1,
                "generation": generation,
                "viewport": {"width": 320, "height": 240},
                "nodes": [{
                    "id": 4,
                    "parent_id": 2,
                    "controls_ids": [],
                    "described_by_ids": [],
                    "details_ids": [],
                    "owns_ids": [],
                    "role": "checkbox",
                    "label": "Remember me",
                    "description": "Account preference",
                    "value": "yes",
                    "text_selection": {"base_offset": 1, "extent_offset": 3},
                    "multiline": false,
                    "text_input_type": "email",
                    "text_input_action": "send",
                    "range": null,
                    "bbox": {"x": 1.5, "y": 2.5, "width": 30.0, "height": 40.0},
                    "focused": true,
                    "disabled": false,
                    "checked": true,
                    "mixed": null,
                    "selected": true,
                    "expanded": false,
                    "heading_level": null,
                    "hidden": false,
                    "live_region": true,
                    "focusable": true,
                    "actions": ["tap"],
                }],
                "truncated": false,
            })
        );

        let worst = AccessibilityNode {
            role: "\\".repeat(512),
            label: "\\".repeat(512),
            description: "\\".repeat(512),
            value: Some("\\".repeat(512)),
            actions: vec!["tap".to_owned()],
            ..node
        };
        let bounded = response_json(ControllerResponse::AccessibilitySnapshot(
            AccessibilitySnapshot {
                context_id: BrowsingContextId::new(2).unwrap(),
                document_id: vixen_api::DocumentId::new(3).unwrap(),
                source_generation: 7,
                generation: 1,
                viewport: (4096, 4096),
                nodes: vec![worst; crate::ACCESSIBILITY_ABI_MAX_NODES + 1],
                truncated: false,
            },
        ));
        assert_eq!(
            bounded["nodes"].as_array().unwrap().len(),
            crate::ACCESSIBILITY_ABI_MAX_NODES
        );
        assert_eq!(bounded["truncated"], true);
        assert_eq!(bounded["source_generation"], 7);
        assert!(serde_json::to_vec(&bounded).unwrap().len() < VIXEN_MAX_OUTPUT_BYTES);
    }

    #[test]
    fn renderer_mouse_input_json_is_strict_and_commit_bound() {
        let _scope = test_scope();
        let parsed = parse_command(&renderer_mouse_command().to_string()).unwrap();
        let ControllerCommand::DispatchRendererMouseEvent(dispatch) = parsed else {
            panic!("renderer mouse command parsed as the wrong variant");
        };
        let target = dispatch.target.expect("renderer target");
        assert_eq!(dispatch.query.query_id.get(), 7);
        assert_eq!(target.query_id, dispatch.query.query_id);
        assert_eq!(target.node_id.get(), 9);

        let mut cases = Vec::new();
        let mut value = renderer_mouse_command();
        value["query"]["extra"] = json!(true);
        cases.push(value);
        let mut value = renderer_mouse_command();
        value["query"]["point"]["x"] = json!("NaN");
        cases.push(value);
        let mut value = renderer_mouse_command();
        value["target"]["fragment_id"] = json!(0);
        cases.push(value);
        let mut value = renderer_mouse_command();
        value["target"] = json!(false);
        cases.push(value);

        for value in cases {
            assert!(
                parse_command(&value.to_string()).is_err(),
                "accepted invalid renderer mouse command: {value}"
            );
        }
    }

    #[test]
    fn key_input_json_is_strict_and_bounded() {
        let _scope = test_scope();
        let parsed = parse_command(&key_command().to_string()).unwrap();
        assert!(matches!(
            parsed,
            ControllerCommand::DispatchKeyEvent {
                viewport: (800, 600),
                ref event_type,
                ref event,
                ..
            } if event_type == "keydown"
                && event.key == "a"
                && event.code == "KeyA"
                && event.text == "a"
                && event.apply_text
        ));

        let mut cases = Vec::new();
        let mut value = key_command();
        value["event_type"] = json!("keypress");
        cases.push(value);
        let mut value = key_command();
        value["event"]["key"] = json!("x".repeat(MAX_KEY_BYTES + 1));
        cases.push(value);
        let mut value = key_command();
        value["event"]["code"] = json!("x".repeat(MAX_CODE_BYTES + 1));
        cases.push(value);
        let mut value = key_command();
        value["event"]["text"] = json!("x".repeat(MAX_TEXT_BYTES + 1));
        cases.push(value);
        let mut value = key_command();
        value["event"]["repeat"] = json!(0);
        cases.push(value);
        let mut value = key_command();
        value["event"]["unknown"] = json!(false);
        cases.push(value);

        for value in cases {
            assert!(
                parse_command(&value.to_string()).is_err(),
                "accepted invalid key command: {value}"
            );
        }
    }

    #[test]
    fn text_input_json_is_strict_bounded_and_utf16_checked() {
        let _scope = test_scope();
        let parsed = parse_command(&text_input_command().to_string()).unwrap();
        assert!(matches!(
            parsed,
            ControllerCommand::DispatchTextInput {
                viewport: (800, 600),
                state: TextInputState {
                    ref text,
                    selection: AccessibilityTextSelection {
                        base_offset: 2,
                        extent_offset: 2,
                    },
                    composing: Some(AccessibilityTextSelection {
                        base_offset: 0,
                        extent_offset: 2,
                    }),
                },
                ..
            } if text == "🦊"
        ));

        let mut cases = Vec::new();
        let mut value = text_input_command();
        value["state"]["text"] = json!("x".repeat(MAX_TEXT_INPUT_BYTES + 1));
        cases.push(value);
        let mut value = text_input_command();
        value["state"]["selection"]["extent_offset"] = json!(3);
        cases.push(value);
        let mut value = text_input_command();
        value["state"]["composing"]["base_offset"] = json!(2);
        value["state"]["composing"]["extent_offset"] = json!(1);
        cases.push(value);
        let mut value = text_input_command();
        value["state"]["extra"] = json!(true);
        cases.push(value);

        for value in cases {
            assert!(
                parse_command(&value.to_string()).is_err(),
                "accepted invalid text input command: {value}"
            );
        }
    }

    #[test]
    fn find_text_json_is_strict_bounded_and_exact() {
        let _scope = test_scope();
        let command = json!({
            "v": 1,
            "type": "find_text",
            "context_id": 1,
            "document_id": 2,
            "query": "Vixen",
            "case_sensitive": false,
            "forward": true,
        });
        assert!(matches!(
            parse_command(&command.to_string()).unwrap(),
            ControllerCommand::FindText {
                ref query,
                case_sensitive: false,
                forward: true,
                ..
            } if query == "Vixen"
        ));
        let mut oversized = command.clone();
        oversized["query"] = json!("x".repeat(MAX_TEXT_BYTES + 1));
        assert!(parse_command(&oversized.to_string()).is_err());
        let mut extra = command;
        extra["extra"] = json!(true);
        assert!(parse_command(&extra.to_string()).is_err());
        assert_eq!(
            response_json(ControllerResponse::FindText(vixen_api::FindTextResult {
                matches: 3,
                active_match: Some(2),
            })),
            json!({"type": "find_text", "matches": 3, "active_match": 2})
        );
    }

    #[test]
    fn page_zoom_json_is_strict_and_bounded() {
        let _scope = test_scope();
        let command = json!({
            "v": 1,
            "type": "set_page_zoom",
            "context_id": 1,
            "zoom": 1.25,
        });
        assert!(matches!(
            parse_command(&command.to_string()).unwrap(),
            ControllerCommand::SetPageZoom { zoom: 1.25, .. }
        ));
        let mut invalid = command.clone();
        invalid["zoom"] = json!("1.25");
        assert!(parse_command(&invalid.to_string()).is_err());
        let mut extra = command;
        extra["extra"] = json!(true);
        assert!(parse_command(&extra.to_string()).is_err());
    }

    #[test]
    fn input_response_projects_navigation_outcomes_exactly() {
        let _scope = test_scope();
        let result = InputDispatchResult {
            effects: RuntimeEffects::default(),
            navigation_actions: vec![
                NavigationActionOutcome::SameDocument {
                    url: "https://ffi.test/#next".to_owned(),
                },
                NavigationActionOutcome::CrossDocument {
                    navigation_id: vixen_api::NavigationId::new(9).unwrap(),
                    kind: CrossDocumentNavigationKind::ContentReplacement {
                        replaced_document_id: vixen_api::DocumentId::new(7).unwrap(),
                    },
                },
            ],
        };

        assert_eq!(
            response_json(ControllerResponse::InputDispatched(result)),
            json!({
                "type": "input_dispatched",
                "effects": {
                    "console": [],
                    "dialogs": [],
                    "bindings": [],
                    "network": [],
                    "exceptions": [],
                },
                "navigation_actions": [
                    {"type": "same_document", "url": "https://ffi.test/#next"},
                    {
                        "type": "cross_document",
                        "navigation_id": 9,
                        "kind": {
                            "type": "content_replacement",
                            "replaced_document_id": 7,
                        },
                    },
                ],
            })
        );
    }

    #[test]
    fn panic_boundary_returns_stable_status() {
        let _scope = test_scope();
        assert_eq!(
            ffi_boundary(|| panic!("contained test panic")),
            VIXEN_STATUS_PANIC
        );
    }

    fn base_renderer_mouse_command() -> Value {
        json!({
            "v": 1,
            "type": "dispatch_renderer_mouse_event",
            "context_id": 1,
            "document_id": 2,
            "runtime_context_id": 3,
            "viewport": {"width": 800, "height": 600},
            "event_type": "mousedown",
            "event": {
                "x": 10.5,
                "y": 20.25,
                "button": 0,
                "buttons": 1,
                "detail": 1,
                "bubbles": true,
                "ctrl_key": false,
                "shift_key": false,
                "alt_key": false,
                "meta_key": false,
                "delta_x": 0.0,
                "delta_y": 0.0,
            },
        })
    }

    fn renderer_mouse_command() -> Value {
        let mut command = base_renderer_mouse_command();
        let revision = json!({
            "context_id": 1,
            "document_id": 2,
            "source_generation": 3,
            "style_generation": 3,
            "viewport_generation": 4,
            "resource_generation": 1,
        });
        command["query"] = json!({
            "v": 1,
            "query_id": 7,
            "context_id": 1,
            "document_id": 2,
            "displayed_commit_id": 5,
            "revision": revision,
            "handle": 6,
            "point": {"x": 10.5, "y": 20.25},
        });
        command["target"] = json!({
            "v": 1,
            "query_id": 7,
            "context_id": 1,
            "document_id": 2,
            "displayed_commit_id": 5,
            "revision": revision,
            "handle": 6,
            "node_id": 9,
            "fragment_id": 10,
            "viewport_point": {"x": 10.5, "y": 20.25},
            "local_point": {"x": 1.5, "y": 2.25},
        });
        command
    }

    fn key_command() -> Value {
        json!({
            "v": 1,
            "type": "dispatch_key_event",
            "context_id": 1,
            "document_id": 2,
            "runtime_context_id": 3,
            "viewport": {"width": 800, "height": 600},
            "event_type": "keydown",
            "event": {
                "key": "a",
                "code": "KeyA",
                "text": "a",
                "apply_text": true,
                "ctrl_key": false,
                "shift_key": false,
                "alt_key": false,
                "meta_key": false,
                "repeat": false,
                "location": 0,
            },
        })
    }

    fn text_input_command() -> Value {
        json!({
            "v": 1,
            "type": "dispatch_text_input",
            "context_id": 1,
            "document_id": 2,
            "runtime_context_id": 3,
            "viewport": {"width": 800, "height": 600},
            "state": {
                "text": "🦊",
                "selection": {"base_offset": 2, "extent_offset": 2},
                "composing": {"base_offset": 0, "extent_offset": 2},
            },
        })
    }

    fn open(profile: &TestProfile) -> Handle {
        let bytes = profile.utf8().as_bytes();
        let mut handle = 0;
        let mut output = VixenBuffer::EMPTY;
        assert_eq!(
            unsafe { vixen_open(bytes.as_ptr(), bytes.len(), &mut handle, &mut output) },
            VIXEN_STATUS_OK
        );
        let opened = take_json(output);
        assert_eq!(opened, json!({"v": 1, "type": "opened"}));
        Handle(handle)
    }

    fn test_scope() -> TestScope {
        let guard = ABI_TEST_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        assert!(controllers().lock().unwrap().is_empty());
        assert!(buffers().lock().unwrap().is_empty());
        TestScope { _guard: guard }
    }

    fn command(handle: u64, value: Value) -> VixenBuffer {
        let message = serde_json::to_vec(&value).unwrap();
        let mut output = VixenBuffer::EMPTY;
        assert_eq!(
            unsafe { vixen_command(handle, message.as_ptr(), message.len(), &mut output) },
            VIXEN_STATUS_OK
        );
        output
    }

    fn drain_events(handle: u64, sequences: &mut Vec<u64>) {
        loop {
            let mut output = VixenBuffer::EMPTY;
            match unsafe { vixen_poll_event(handle, &mut output) } {
                VIXEN_STATUS_OK => {
                    let event = take_json(output);
                    sequences.push(event["sequence"].as_u64().unwrap());
                }
                VIXEN_STATUS_NO_EVENT => {
                    assert_eq!(output.token, 0);
                    return;
                }
                status => panic!("unexpected poll status {status}"),
            }
        }
    }

    fn take_json(output: VixenBuffer) -> Value {
        assert_ne!(output.token, 0);
        assert!(!output.ptr.is_null());
        let bytes = unsafe { std::slice::from_raw_parts(output.ptr, output.len) };
        let value = serde_json::from_slice(bytes).unwrap();
        assert_eq!(vixen_buffer_release(output.token), VIXEN_STATUS_OK);
        value
    }

    fn assert_error_and_release(output: VixenBuffer, code: &str) {
        let value = take_json(output);
        assert_eq!(value["v"], 1);
        assert_eq!(value["type"], "error");
        assert_eq!(value["error"]["code"], code);
    }
}
