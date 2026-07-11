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
    BrowserError, BrowserEvent, BrowsingContextId, BrowsingContextState,
    CrossDocumentNavigationKind, DiagnosticScope, DownloadEvent, EngineDiagnostic,
    EngineDiagnosticCategory, NavigationCancellationReason, NavigationPhase, RuntimeConsoleArg,
    RuntimeConsoleValue, RuntimeEffects, RuntimeNetworkEvent,
};

use crate::{ABI_VERSION, ControllerCommand, ControllerResponse, FlutterBrowserController};

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
    const EMPTY: Self = Self {
        token: 0,
        ptr: ptr::null(),
        len: 0,
    };
}

struct ControllerState {
    controller: FlutterBrowserController,
    next_event_sequence: u64,
}

type ControllerEntry = Arc<Mutex<ControllerState>>;

static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);
static NEXT_BUFFER: AtomicU64 = AtomicU64::new(1);
static CONTROLLERS: OnceLock<Mutex<HashMap<u64, ControllerEntry>>> = OnceLock::new();
static BUFFERS: OnceLock<Mutex<HashMap<u64, Box<[u8]>>>> = OnceLock::new();

fn controllers() -> &'static Mutex<HashMap<u64, ControllerEntry>> {
    CONTROLLERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn buffers() -> &'static Mutex<HashMap<u64, Box<[u8]>>> {
    BUFFERS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Debug)]
struct AbiError {
    status: u32,
    code: &'static str,
    message: String,
}

impl AbiError {
    fn new(status: u32, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(VIXEN_STATUS_INVALID_ARGUMENT, FFI_INVALID_ARGUMENT, message)
    }

    fn invalid_command(message: impl Into<String>) -> Self {
        Self::new(VIXEN_STATUS_INVALID_COMMAND, FFI_INVALID_COMMAND, message)
    }

    fn internal(message: impl Into<String>) -> Self {
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
            let controller = FlutterBrowserController::open(profile_path).map_err(browser_error)?;
            let handle = next_token(&NEXT_HANDLE, "browser handle")?;
            let entry = Arc::new(Mutex::new(ControllerState {
                controller,
                next_event_sequence: 1,
            }));
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
        if removed.is_some() {
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
            let response = entry
                .lock()
                .map_err(|_| AbiError::internal("browser handle is unavailable"))?
                .controller
                .dispatch(command)
                .map_err(browser_error)?;
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

fn initialize_output(out_json: *mut VixenBuffer) -> bool {
    if out_json.is_null() {
        return false;
    }
    unsafe { out_json.write(VixenBuffer::EMPTY) };
    true
}

fn finish(result: Result<(), AbiError>, out_json: *mut VixenBuffer) -> u32 {
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

fn write_json(out_json: *mut VixenBuffer, value: &Value) -> Result<(), AbiError> {
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
    if registry.len() >= VIXEN_MAX_OUTSTANDING_BUFFERS {
        return Err(AbiError::new(
            VIXEN_STATUS_BUFFER_LIMIT,
            FFI_BUFFER_LIMIT,
            format!("outstanding output buffer limit of {VIXEN_MAX_OUTSTANDING_BUFFERS} reached"),
        ));
    }
    let token = next_token(&NEXT_BUFFER, "buffer token")?;
    let descriptor = VixenBuffer {
        token,
        ptr: allocation.as_ptr(),
        len: allocation.len(),
    };
    registry.insert(token, allocation);
    unsafe { out_json.write(descriptor) };
    Ok(())
}

fn next_token(counter: &AtomicU64, kind: &str) -> Result<u64, AbiError> {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| AbiError::internal(format!("{kind} exhausted")))
}

fn controller_entry(handle: u64) -> Result<ControllerEntry, AbiError> {
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

fn browser_error(error: BrowserError) -> AbiError {
    AbiError::new(VIXEN_STATUS_BROWSER_ERROR, error.code, error.message)
}

fn copy_utf8_input(
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

fn required_context_id(object: &Map<String, Value>) -> Result<BrowsingContextId, AbiError> {
    BrowsingContextId::new(required_u64(object, "context_id")?)
        .ok_or_else(|| AbiError::invalid_command("context_id must be nonzero"))
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
        assert_eq!(VIXEN_MAX_PROFILE_PATH_BYTES, 4096);
        assert_eq!(VIXEN_MAX_MESSAGE_BYTES, 65_536);
        assert_eq!(VIXEN_MAX_OUTPUT_BYTES, 1_048_576);
        assert_eq!(VIXEN_MAX_OUTSTANDING_BUFFERS, 64);
        assert_eq!(VIXEN_MAX_WAIT_MILLISECONDS, 60_000);
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
    fn panic_boundary_returns_stable_status() {
        let _scope = test_scope();
        assert_eq!(
            ffi_boundary(|| panic!("contained test panic")),
            VIXEN_STATUS_PANIC
        );
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
