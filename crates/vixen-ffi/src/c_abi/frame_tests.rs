use super::*;

#[test]
fn frame_outputs_are_reset_and_null_outputs_fail_closed() {
    let _scope = test_scope();
    let mut frame = dirty_frame();
    let mut output = VixenBuffer {
        token: 9,
        ptr: ptr::dangling(),
        len: 9,
    };
    assert_eq!(
        unsafe { vixen_capture_frame(0, 1, 1, 1, 1, ptr::null_mut(), &mut output) },
        VIXEN_STATUS_INVALID_ARGUMENT
    );
    assert_error_and_release(output, FFI_INVALID_ARGUMENT);

    assert_eq!(
        unsafe { vixen_capture_frame(0, 1, 1, 1, 1, &mut frame, ptr::null_mut()) },
        VIXEN_STATUS_INVALID_ARGUMENT
    );
    assert_empty_frame(frame);
}

#[test]
fn frame_dimensions_and_nonzero_ids_are_validated_without_egl() {
    let _scope = test_scope();
    for (context_id, document_id, width, height) in [
        (0, 1, 1, 1),
        (1, 0, 1, 1),
        (1, 1, 0, 1),
        (1, 1, 1, 0),
        (1, 1, VIXEN_MAX_FRAME_DIMENSION + 1, 1),
        (1, 1, 1, VIXEN_MAX_FRAME_DIMENSION + 1),
    ] {
        let mut frame = dirty_frame();
        let mut output = VixenBuffer::EMPTY;
        assert_eq!(
            unsafe {
                vixen_capture_frame(
                    u64::MAX,
                    context_id,
                    document_id,
                    width,
                    height,
                    &mut frame,
                    &mut output,
                )
            },
            VIXEN_STATUS_INVALID_ARGUMENT
        );
        assert_empty_frame(frame);
        assert_error_and_release(output, FFI_INVALID_ARGUMENT);
    }
}

#[test]
fn frame_capture_rejects_unknown_handles_contexts_and_stale_documents_before_egl() {
    let _scope = test_scope();
    let profile = TestProfile::new();
    let handle = open(&profile);
    let created = take_json(command(handle.0, json!({"v": 1, "type": "create_context"})));
    let context_id = created["response"]["context_id"].as_u64().unwrap();
    let state = take_json(command(
        handle.0,
        json!({"v": 1, "type": "context_state", "context_id": context_id}),
    ));
    let document_id = state["response"]["state"]["document_id"].as_u64().unwrap();

    assert_capture_error(
        u64::MAX,
        context_id,
        document_id,
        VIXEN_STATUS_UNKNOWN_HANDLE,
        FFI_UNKNOWN_HANDLE,
    );
    assert_capture_error(
        handle.0,
        u64::MAX,
        document_id,
        VIXEN_STATUS_BROWSER_ERROR,
        "browser.unknown-context",
    );
    assert_capture_error(
        handle.0,
        context_id,
        document_id + 1,
        VIXEN_STATUS_BROWSER_ERROR,
        "browser.stale-document",
    );
}

#[test]
fn retained_frames_have_a_separate_bounded_token_registry() {
    let _scope = test_scope();
    let frames = (1..=VIXEN_MAX_OUTSTANDING_FRAMES)
        .map(|frame_id| {
            crate::c_frame::register_test_frame(vec![frame_id as u8; 4], frame_id as u64).unwrap()
        })
        .collect::<Vec<_>>();
    for (index, frame) in frames.iter().enumerate() {
        assert_ne!(frame.token, 0);
        assert!(!frame.ptr.is_null());
        assert_eq!(frame.len, 4);
        assert_eq!(frame.row_stride, 4);
        assert_eq!(frame.frame_id, index as u64 + 1);
        assert_eq!(
            unsafe { std::slice::from_raw_parts(frame.ptr, frame.len) },
            &[index as u8 + 1; 4]
        );
    }

    let error = crate::c_frame::register_test_frame(vec![0; 4], 4).unwrap_err();
    assert_eq!(error.status, VIXEN_STATUS_FRAME_LIMIT);
    assert_eq!(error.code, "ffi.frame-limit");

    let profile = TestProfile::new();
    let handle = open(&profile);
    assert_capture_error(
        handle.0,
        1,
        1,
        VIXEN_STATUS_FRAME_LIMIT,
        "ffi.frame-limit",
    );

    assert_eq!(vixen_frame_release(frames[0].token), VIXEN_STATUS_OK);
    assert_eq!(
        vixen_frame_release(frames[0].token),
        VIXEN_STATUS_UNKNOWN_BUFFER
    );
    assert_eq!(vixen_frame_release(0), VIXEN_STATUS_UNKNOWN_BUFFER);
    let replacement = crate::c_frame::register_test_frame(vec![4; 4], 4).unwrap();
    for frame in frames.iter().skip(1) {
        assert_eq!(vixen_frame_release(frame.token), VIXEN_STATUS_OK);
    }
    assert_eq!(vixen_frame_release(replacement.token), VIXEN_STATUS_OK);
}

#[test]
fn frame_id_exhaustion_fails_before_snapshot_or_render() {
    let _scope = test_scope();
    let profile = TestProfile::new();
    let handle = open(&profile);
    controller_entry(handle.0)
        .unwrap()
        .state
        .lock()
        .unwrap()
        .next_frame_id = u64::MAX;
    assert_capture_error(handle.0, 1, 1, VIXEN_STATUS_INTERNAL_ERROR, FFI_INTERNAL);
}

#[test]
fn real_fixture_capture_is_deterministic_when_egl_is_available() {
    let _scope = test_scope();
    let profile = TestProfile::new();
    let url = "https://ffi.test/frame-fixture";
    let mut config = vixen_engine::browser::BrowserConfig::new(&profile.0);
    config.document_overrides.insert(
        url.to_owned(),
        include_str!("../../tests/fixtures/frame.html").to_owned(),
    );
    let handle = open_controller(FlutterBrowserController::from_config(config).unwrap());
    let created = take_json(command(handle.0, json!({"v": 1, "type": "create_context"})));
    let context_id = created["response"]["context_id"].as_u64().unwrap();
    let mut ignored_sequences = Vec::new();
    drain_events(handle.0, &mut ignored_sequences);
    let navigated = take_json(command(
        handle.0,
        json!({"v": 1, "type": "navigate", "context_id": context_id, "url": url}),
    ));
    let navigation_id = navigated["response"]["navigation_id"].as_u64().unwrap();
    wait_for_navigation_settled(handle.0, navigation_id);
    let state = take_json(command(
        handle.0,
        json!({"v": 1, "type": "context_state", "context_id": context_id}),
    ));
    let document_id = state["response"]["state"]["document_id"].as_u64().unwrap();

    let Some(first) = capture_or_skip(handle.0, context_id, document_id, (64, 48)) else {
        return;
    };
    let first_rgba = unsafe { std::slice::from_raw_parts(first.ptr, first.len) }.to_vec();
    let second = capture_or_skip(handle.0, context_id, document_id, (64, 48))
        .expect("EGL availability changed between captures");
    let second_rgba = unsafe { std::slice::from_raw_parts(second.ptr, second.len) };

    assert_eq!(first.width, 64);
    assert_eq!(first.height, 48);
    assert_eq!(first.row_stride, 64 * 4);
    assert_eq!(first.len, 64 * 48 * 4);
    assert_eq!(first.context_id, context_id);
    assert_eq!(first.document_id, document_id);
    assert_eq!(second.frame_id, first.frame_id + 1);
    assert_ne!(second.token, first.token);
    assert_eq!(second_rgba, first_rgba);
    assert!(
        first_rgba
            .chunks_exact(4)
            .skip(1)
            .any(|pixel| pixel != &first_rgba[..4]),
        "fixture should render more than one RGBA color"
    );
    assert_eq!(vixen_frame_release(first.token), VIXEN_STATUS_OK);
    assert_eq!(vixen_frame_release(second.token), VIXEN_STATUS_OK);
}

fn dirty_frame() -> VixenFrame {
    VixenFrame {
        token: 9,
        ptr: ptr::dangling(),
        len: 9,
        width: 9,
        height: 9,
        row_stride: 9,
        frame_id: 9,
        context_id: 9,
        document_id: 9,
    }
}

fn assert_empty_frame(frame: VixenFrame) {
    assert_eq!(frame.token, 0);
    assert!(frame.ptr.is_null());
    assert_eq!(frame.len, 0);
    assert_eq!(frame.width, 0);
    assert_eq!(frame.height, 0);
    assert_eq!(frame.row_stride, 0);
    assert_eq!(frame.frame_id, 0);
    assert_eq!(frame.context_id, 0);
    assert_eq!(frame.document_id, 0);
}

fn assert_capture_error(
    handle: u64,
    context_id: u64,
    document_id: u64,
    expected_status: u32,
    expected_code: &str,
) {
    let mut frame = dirty_frame();
    let mut output = VixenBuffer::EMPTY;
    assert_eq!(
        unsafe {
            vixen_capture_frame(
                handle,
                context_id,
                document_id,
                1,
                1,
                &mut frame,
                &mut output,
            )
        },
        expected_status
    );
    assert_empty_frame(frame);
    assert_error_and_release(output, expected_code);
}

fn capture_or_skip(
    handle: u64,
    context_id: u64,
    document_id: u64,
    viewport: (u32, u32),
) -> Option<VixenFrame> {
    let mut frame = dirty_frame();
    let mut output = VixenBuffer::EMPTY;
    let status = unsafe {
        vixen_capture_frame(
            handle,
            context_id,
            document_id,
            viewport.0,
            viewport.1,
            &mut frame,
            &mut output,
        )
    };
    if status == VIXEN_STATUS_OK {
        assert_eq!(output.token, 0);
        assert!(output.ptr.is_null());
        assert_eq!(output.len, 0);
        return Some(frame);
    }
    assert_empty_frame(frame);
    let error = take_json(output);
    if status == VIXEN_STATUS_BROWSER_ERROR && error["error"]["code"] == "unsupported.screenshot" {
        eprintln!(
            "skipping real EGL frame capture: {}",
            error["error"]["message"]
                .as_str()
                .unwrap_or("unknown reason")
        );
        return None;
    }
    panic!("real frame capture failed with status {status}: {error}");
}

fn wait_for_navigation_settled(handle: u64, navigation_id: u64) {
    for _ in 0..64 {
        let mut output = VixenBuffer::EMPTY;
        let status = unsafe { vixen_wait_event(handle, 1_000, &mut output) };
        assert_eq!(status, VIXEN_STATUS_OK);
        let event = take_json(output);
        if event["event"]["type"] == "navigation_phase_changed"
            && event["event"]["navigation_id"].as_u64() == Some(navigation_id)
            && event["event"]["phase"] == "settled"
        {
            return;
        }
    }
    panic!("fixture navigation did not settle");
}

fn open_controller(controller: FlutterBrowserController) -> Handle {
    let handle = next_token(&NEXT_HANDLE, "browser handle").unwrap();
    controllers().lock().unwrap().insert(
        handle,
        Arc::new(ControllerEntry {
            state: Mutex::new(ControllerState {
                controller,
                render_replica: vixen_api::RenderReplica::default(),
                render_commits: vixen_api::RenderCommitState::default(),
                next_event_sequence: 1,
                next_frame_id: 1,
            }),
            renderer: crate::RenderBroker::new(),
        }),
    );
    Handle(handle)
}
