//! Retained RGBA frame portion of the handwritten C ABI.
//!
//! Frame allocations use tokens distinct from JSON buffers. Raw pointers are
//! immutable views into registry-owned allocations and are never interpreted as
//! Rust objects when returned by callers.

#![allow(unsafe_code)]

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::atomic::AtomicU64;
use std::sync::{Mutex, OnceLock};

use vixen_api::{BrowsingContextId, DocumentId};

use crate::c_abi::{
    AbiError, VIXEN_STATUS_INTERNAL_ERROR, VIXEN_STATUS_INVALID_ARGUMENT, VIXEN_STATUS_OK,
    VIXEN_STATUS_PANIC, VIXEN_STATUS_UNKNOWN_BUFFER, VixenBuffer, browser_error, controller_entry,
    finish, initialize_output, next_token,
};
use crate::frame::{MAX_FRAME_BYTES, MAX_FRAME_DIMENSION, expected_rgba_len};

pub const VIXEN_STATUS_FRAME_LIMIT: u32 = 13;
pub const VIXEN_MAX_FRAME_DIMENSION: u32 = MAX_FRAME_DIMENSION;
pub const VIXEN_MAX_FRAME_BYTES: usize = MAX_FRAME_BYTES;
pub const VIXEN_MAX_OUTSTANDING_FRAMES: usize = 3;

const FFI_FRAME_LIMIT: &str = "ffi.frame-limit";

/// Rust-owned immutable packed RGBA8 frame descriptor.
///
/// A nonzero token retains `ptr` until exactly one successful call to
/// [`vixen_frame_release`]. Frame tokens are independent of browser handles and
/// JSON buffer tokens.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VixenFrame {
    pub token: u64,
    pub ptr: *const u8,
    pub len: usize,
    pub width: u32,
    pub height: u32,
    pub row_stride: usize,
    pub frame_id: u64,
    pub context_id: u64,
    pub document_id: u64,
}

impl VixenFrame {
    pub(crate) const EMPTY: Self = Self {
        token: 0,
        ptr: ptr::null(),
        len: 0,
        width: 0,
        height: 0,
        row_stride: 0,
        frame_id: 0,
        context_id: 0,
        document_id: 0,
    };
}

static NEXT_FRAME_TOKEN: AtomicU64 = AtomicU64::new(1);
static FRAMES: OnceLock<Mutex<HashMap<u64, Box<[u8]>>>> = OnceLock::new();

fn frames() -> &'static Mutex<HashMap<u64, Box<[u8]>>> {
    FRAMES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Capture one authoritative document generation into retained packed RGBA8.
///
/// Calls serialize on the browser handle's existing lock, including snapshot
/// and rendering. On success `out_json` remains all-zero; it is only the bounded
/// JSON error path. The frame pointer remains immutable and valid across handle
/// destruction until `vixen_frame_release(token)` succeeds. At most three frame
/// pointers may be retained process-wide.
///
/// # Safety
///
/// `out_frame` and `out_json` must address writable descriptors for the duration
/// of this call. Callers must not concurrently destroy `handle` with this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vixen_capture_frame(
    handle: u64,
    context_id: u64,
    document_id: u64,
    width: u32,
    height: u32,
    out_frame: *mut VixenFrame,
    out_json: *mut VixenBuffer,
) -> u32 {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let frame_output_valid = initialize_frame_output(out_frame);
        let json_output_valid = initialize_output(out_json);
        if !frame_output_valid || !json_output_valid {
            if json_output_valid {
                return finish(
                    Err(AbiError::invalid_argument(
                        "frame and JSON output pointers must not be null",
                    )),
                    out_json,
                );
            }
            return VIXEN_STATUS_INVALID_ARGUMENT;
        }

        let result = capture_impl(handle, context_id, document_id, (width, height), out_frame);
        finish(result, out_json)
    }));
    match outcome {
        Ok(status) => status,
        Err(_) => {
            cleanup_frame_output(out_frame);
            VIXEN_STATUS_PANIC
        }
    }
}

/// Release a retained RGBA frame. Zero, unknown, and repeated tokens fail safely.
#[unsafe(no_mangle)]
pub extern "C" fn vixen_frame_release(token: u64) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if token == 0 {
            return VIXEN_STATUS_UNKNOWN_BUFFER;
        }
        let removed = match frames().lock() {
            Ok(mut registry) => registry.remove(&token),
            Err(_) => return VIXEN_STATUS_INTERNAL_ERROR,
        };
        if removed.is_some() {
            VIXEN_STATUS_OK
        } else {
            VIXEN_STATUS_UNKNOWN_BUFFER
        }
    }))
    .unwrap_or(VIXEN_STATUS_PANIC)
}

fn capture_impl(
    handle: u64,
    context_id: u64,
    document_id: u64,
    viewport: (u32, u32),
    out_frame: *mut VixenFrame,
) -> Result<(), AbiError> {
    let context_id = BrowsingContextId::new(context_id)
        .ok_or_else(|| AbiError::invalid_argument("context id must be nonzero"))?;
    let document_id = DocumentId::new(document_id)
        .ok_or_else(|| AbiError::invalid_argument("document id must be nonzero"))?;
    let expected_len =
        expected_rgba_len(viewport).map_err(|error| AbiError::invalid_argument(error.message))?;
    let row_stride = (viewport.0 as usize)
        .checked_mul(4)
        .ok_or_else(|| AbiError::invalid_argument("frame row stride overflows size_t"))?;

    let entry = controller_entry(handle)?;
    let mut state = entry
        .lock()
        .map_err(|_| AbiError::internal("browser handle is unavailable"))?;
    let frame_id = state.next_frame_id;
    let next_frame_id = frame_id
        .checked_add(1)
        .ok_or_else(|| AbiError::internal("frame id exhausted"))?;
    ensure_frame_capacity()?;
    let rendered = state
        .controller
        .capture_rgba_frame(context_id, document_id, viewport)
        .map_err(browser_error)?;
    if rendered.rgba.len() != expected_len
        || rendered.width != viewport.0
        || rendered.height != viewport.1
    {
        return Err(AbiError::internal(
            "validated renderer output changed before frame registration",
        ));
    }

    let pending = register_frame(
        rendered.rgba.into_boxed_slice(),
        viewport,
        row_stride,
        frame_id,
        context_id.get(),
        document_id.get(),
    )?;
    pending.commit(out_frame);
    state.next_frame_id = next_frame_id;
    Ok(())
}

fn ensure_frame_capacity() -> Result<(), AbiError> {
    let registry = frames()
        .lock()
        .map_err(|_| AbiError::internal("frame registry is unavailable"))?;
    if registry.len() >= VIXEN_MAX_OUTSTANDING_FRAMES {
        return Err(frame_limit_error());
    }
    Ok(())
}

struct PendingFrame {
    descriptor: VixenFrame,
    committed: bool,
}

impl PendingFrame {
    fn commit(mut self, out_frame: *mut VixenFrame) {
        unsafe { out_frame.write(self.descriptor) };
        self.committed = true;
    }
}

impl Drop for PendingFrame {
    fn drop(&mut self) {
        if !self.committed
            && let Ok(mut registry) = frames().lock()
        {
            registry.remove(&self.descriptor.token);
        }
    }
}

fn register_frame(
    allocation: Box<[u8]>,
    viewport: (u32, u32),
    row_stride: usize,
    frame_id: u64,
    context_id: u64,
    document_id: u64,
) -> Result<PendingFrame, AbiError> {
    let mut registry = frames()
        .lock()
        .map_err(|_| AbiError::internal("frame registry is unavailable"))?;
    if registry.len() >= VIXEN_MAX_OUTSTANDING_FRAMES {
        return Err(frame_limit_error());
    }
    let token = next_token(&NEXT_FRAME_TOKEN, "frame token")?;
    let descriptor = VixenFrame {
        token,
        ptr: allocation.as_ptr(),
        len: allocation.len(),
        width: viewport.0,
        height: viewport.1,
        row_stride,
        frame_id,
        context_id,
        document_id,
    };
    registry.insert(token, allocation);
    Ok(PendingFrame {
        descriptor,
        committed: false,
    })
}

fn frame_limit_error() -> AbiError {
    AbiError::new(
        VIXEN_STATUS_FRAME_LIMIT,
        FFI_FRAME_LIMIT,
        format!("outstanding frame limit of {VIXEN_MAX_OUTSTANDING_FRAMES} reached"),
    )
}

fn initialize_frame_output(out_frame: *mut VixenFrame) -> bool {
    if out_frame.is_null() {
        return false;
    }
    unsafe { out_frame.write(VixenFrame::EMPTY) };
    true
}

fn cleanup_frame_output(out_frame: *mut VixenFrame) {
    if out_frame.is_null() {
        return;
    }
    let token = unsafe { out_frame.read() }.token;
    if token != 0
        && let Ok(mut registry) = frames().lock()
    {
        registry.remove(&token);
    }
    unsafe { out_frame.write(VixenFrame::EMPTY) };
}

#[cfg(test)]
pub(crate) fn frame_registry_is_empty() -> bool {
    frames().lock().is_ok_and(|registry| registry.is_empty())
}

#[cfg(test)]
pub(crate) fn register_test_frame(bytes: Vec<u8>, frame_id: u64) -> Result<VixenFrame, AbiError> {
    let pending = register_frame(bytes.into_boxed_slice(), (1, 1), 4, frame_id, 1, 1)?;
    let descriptor = pending.descriptor;
    let mut output = VixenFrame::EMPTY;
    pending.commit(&mut output);
    debug_assert_eq!(descriptor.token, output.token);
    Ok(output)
}
