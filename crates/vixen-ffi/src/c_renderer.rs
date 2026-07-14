//! Dedicated C ABI renderer broker, independent of controller serialization.

#![allow(unsafe_code)]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

use serde_json::json;

use crate::ABI_VERSION;
use crate::c_abi::{
    AbiError, VIXEN_MAX_MESSAGE_BYTES, VIXEN_MAX_WAIT_MILLISECONDS, VIXEN_STATUS_INVALID_ARGUMENT,
    VIXEN_STATUS_NO_EVENT, VIXEN_STATUS_OK, VIXEN_STATUS_PANIC, VixenBuffer, controller_entry,
    copy_utf8_input, finish, initialize_output, write_json,
};
use crate::render_wire::{message_json, parse_response, parse_submission};

/// Poll the dedicated renderer queue without taking the browser controller lock.
///
/// # Safety
/// `out_json` must address one writable [`VixenBuffer`] for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vixen_renderer_poll(
    handle: u64,
    timeout_milliseconds: u64,
    out_json: *mut VixenBuffer,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if !initialize_output(out_json) {
            return VIXEN_STATUS_INVALID_ARGUMENT;
        }
        if timeout_milliseconds > VIXEN_MAX_WAIT_MILLISECONDS {
            return finish(
                Err(AbiError::invalid_argument(
                    "renderer poll timeout is too large",
                )),
                out_json,
            );
        }
        let result = (|| {
            let entry = controller_entry(handle)?;
            entry
                .renderer
                .poll_message(Duration::from_millis(timeout_milliseconds))
                .map_err(broker_error)
        })();
        match result {
            Ok(Some(message)) => match write_json(out_json, &message_json(&message)) {
                Ok(()) => VIXEN_STATUS_OK,
                Err(error) => finish(Err(error), out_json),
            },
            Ok(None) => VIXEN_STATUS_NO_EVENT,
            Err(error) => finish(Err(error), out_json),
        }
    }))
    .unwrap_or(VIXEN_STATUS_PANIC)
}

/// Submit one correlated renderer response without taking the controller lock.
///
/// # Safety
/// `message` must address `message_len` readable bytes and `out_json` one
/// writable [`VixenBuffer`] for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vixen_renderer_respond(
    handle: u64,
    message: *const u8,
    message_len: usize,
    out_json: *mut VixenBuffer,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if !initialize_output(out_json) {
            return VIXEN_STATUS_INVALID_ARGUMENT;
        }
        let result = (|| {
            let message = copy_utf8_input(
                message,
                message_len,
                VIXEN_MAX_MESSAGE_BYTES,
                "renderer response",
            )?;
            let response = parse_response(&message)?;
            let entry = controller_entry(handle)?;
            entry.renderer.respond(response).map_err(broker_error)?;
            write_json(
                out_json,
                &json!({"v": ABI_VERSION, "type": "renderer_accepted"}),
            )
        })();
        finish(result, out_json)
    }))
    .unwrap_or(VIXEN_STATUS_PANIC)
}

/// Submit one ordinary asynchronous renderer commit, presentation, or resync.
///
/// # Safety
/// `message` must address `message_len` readable bytes and `out_json` one
/// writable [`VixenBuffer`] for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vixen_renderer_submit(
    handle: u64,
    message: *const u8,
    message_len: usize,
    out_json: *mut VixenBuffer,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if !initialize_output(out_json) {
            return VIXEN_STATUS_INVALID_ARGUMENT;
        }
        let result = (|| {
            let message = copy_utf8_input(
                message,
                message_len,
                VIXEN_MAX_MESSAGE_BYTES,
                "renderer submission",
            )?;
            let submission = parse_submission(&message)?;
            let entry = controller_entry(handle)?;
            entry.renderer.submit(submission).map_err(broker_error)?;
            write_json(
                out_json,
                &json!({"v": ABI_VERSION, "type": "renderer_submitted"}),
            )
        })();
        finish(result, out_json)
    }))
    .unwrap_or(VIXEN_STATUS_PANIC)
}

/// Cancel all pending renderer requests and wake blocked renderer polls without
/// waiting for the serialized browser command worker.
///
/// # Safety
/// `out_json` must address one writable [`VixenBuffer`] for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vixen_renderer_shutdown(handle: u64, out_json: *mut VixenBuffer) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if !initialize_output(out_json) {
            return VIXEN_STATUS_INVALID_ARGUMENT;
        }
        let result = (|| {
            let entry = controller_entry(handle)?;
            entry.renderer.shutdown().map_err(broker_error)?;
            write_json(
                out_json,
                &json!({"v": ABI_VERSION, "type": "renderer_shutdown"}),
            )
        })();
        finish(result, out_json)
    }))
    .unwrap_or(VIXEN_STATUS_PANIC)
}

fn broker_error(error: crate::RenderBrokerError) -> AbiError {
    AbiError::invalid_command(format!("{}: {}", error.code, error.message))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    use serde_json::Value;
    use vixen_api::{
        BrowsingContextId, DocumentId, FullRenderSnapshot, RenderBridgeSubmission,
        RenderBridgeUpdate, RenderBrokerCancellation, RenderBrokerRequestKind,
        RenderBrokerResponseKind, RenderRevision, RenderViewport,
    };

    use super::*;
    use crate::c_abi::{
        VIXEN_STATUS_UNKNOWN_BUFFER, vixen_buffer_release, vixen_destroy, vixen_open,
    };

    static NEXT_PROFILE: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn c_broker_progresses_with_controller_lock_held_and_releases_buffers() {
        let profile = std::env::temp_dir().join(format!(
            "vixen-renderer-broker-{}-{}",
            std::process::id(),
            NEXT_PROFILE.fetch_add(1, Ordering::Relaxed)
        ));
        let bytes = profile.to_string_lossy().as_bytes().to_vec();
        let mut handle = 0;
        let mut opened = VixenBuffer::EMPTY;
        assert_eq!(
            unsafe { vixen_open(bytes.as_ptr(), bytes.len(), &mut handle, &mut opened) },
            VIXEN_STATUS_OK
        );
        assert_eq!(vixen_buffer_release(opened.token), VIXEN_STATUS_OK);
        let entry = controller_entry(handle).unwrap();
        let held = entry.state.lock().unwrap();
        let requester = entry.renderer.clone();
        let join = thread::spawn(move || {
            requester.request(
                RenderBrokerRequestKind::EnsureLayout {
                    required_revision: RenderRevision {
                        context_id: BrowsingContextId::new(1).unwrap(),
                        document_id: DocumentId::new(2).unwrap(),
                        source_generation: 3,
                        style_generation: 4,
                        viewport_generation: 5,
                        resource_generation: 6,
                    },
                },
                Duration::from_secs(2),
            )
        });

        let mut request = VixenBuffer::EMPTY;
        assert_eq!(
            unsafe { vixen_renderer_poll(handle, 1_000, &mut request) },
            VIXEN_STATUS_OK
        );
        let request_json: Value =
            serde_json::from_slice(unsafe { std::slice::from_raw_parts(request.ptr, request.len) })
                .unwrap();
        assert_eq!(request_json["request"]["type"], "ensure_layout");
        let request_id = request_json["request_id"].as_u64().unwrap();
        assert_eq!(vixen_buffer_release(request.token), VIXEN_STATUS_OK);
        assert_eq!(
            vixen_buffer_release(request.token),
            VIXEN_STATUS_UNKNOWN_BUFFER
        );

        let response = format!(
            "{{\"v\":1,\"type\":\"renderer_response\",\"request_id\":{request_id},\"response\":{{\"type\":\"cancelled\",\"reason\":\"stop\"}}}}"
        );
        let mut accepted = VixenBuffer::EMPTY;
        assert_eq!(
            unsafe {
                vixen_renderer_respond(handle, response.as_ptr(), response.len(), &mut accepted)
            },
            VIXEN_STATUS_OK
        );
        assert_eq!(vixen_buffer_release(accepted.token), VIXEN_STATUS_OK);
        assert!(matches!(
            join.join().unwrap().unwrap().kind,
            RenderBrokerResponseKind::Cancelled(_)
        ));

        entry
            .renderer
            .publish_update(RenderBridgeUpdate::FullSnapshot(FullRenderSnapshot::new(
                RenderRevision {
                    context_id: BrowsingContextId::new(1).unwrap(),
                    document_id: DocumentId::new(2).unwrap(),
                    source_generation: 3,
                    style_generation: 4,
                    viewport_generation: 5,
                    resource_generation: 6,
                },
                RenderViewport {
                    width: 240,
                    height: 160,
                    device_scale: 1.0,
                    page_zoom: 1.0,
                },
            )))
            .unwrap();
        let mut update = VixenBuffer::EMPTY;
        assert_eq!(
            unsafe { vixen_renderer_poll(handle, 0, &mut update) },
            VIXEN_STATUS_OK
        );
        let update_json: Value =
            serde_json::from_slice(unsafe { std::slice::from_raw_parts(update.ptr, update.len) })
                .unwrap();
        assert_eq!(update_json["type"], "renderer_update");
        assert_eq!(vixen_buffer_release(update.token), VIXEN_STATUS_OK);

        let submission = b"{\"v\":1,\"type\":\"renderer_submission\",\"submission\":{\"type\":\"presented\",\"context_id\":1,\"document_id\":2,\"commit_id\":3,\"revision\":{\"context_id\":1,\"document_id\":2,\"source_generation\":3,\"style_generation\":4,\"viewport_generation\":5,\"resource_generation\":6}}}";
        let mut submitted = VixenBuffer::EMPTY;
        assert_eq!(
            unsafe {
                vixen_renderer_submit(
                    handle,
                    submission.as_ptr(),
                    submission.len(),
                    &mut submitted,
                )
            },
            VIXEN_STATUS_OK
        );
        assert_eq!(vixen_buffer_release(submitted.token), VIXEN_STATUS_OK);
        assert!(matches!(
            entry.renderer.poll_submission().unwrap(),
            Some(RenderBridgeSubmission::Presented(_))
        ));

        let requester = entry.renderer.clone();
        let join = thread::spawn(move || {
            requester.request(
                RenderBrokerRequestKind::EnsureLayout {
                    required_revision: RenderRevision {
                        context_id: BrowsingContextId::new(1).unwrap(),
                        document_id: DocumentId::new(2).unwrap(),
                        source_generation: 4,
                        style_generation: 4,
                        viewport_generation: 5,
                        resource_generation: 6,
                    },
                },
                Duration::from_secs(2),
            )
        });
        let mut request = VixenBuffer::EMPTY;
        assert_eq!(
            unsafe { vixen_renderer_poll(handle, 1_000, &mut request) },
            VIXEN_STATUS_OK
        );
        assert_eq!(vixen_buffer_release(request.token), VIXEN_STATUS_OK);
        let mut shutdown = VixenBuffer::EMPTY;
        assert_eq!(
            unsafe { vixen_renderer_shutdown(handle, &mut shutdown) },
            VIXEN_STATUS_OK
        );
        assert_eq!(vixen_buffer_release(shutdown.token), VIXEN_STATUS_OK);
        assert!(matches!(
            join.join().unwrap().unwrap().kind,
            RenderBrokerResponseKind::Cancelled(RenderBrokerCancellation::Shutdown)
        ));
        drop(held);
        assert_eq!(vixen_destroy(handle), VIXEN_STATUS_OK);
        let _ = std::fs::remove_file(profile);
    }
}
