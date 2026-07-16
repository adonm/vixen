use std::sync::{Arc, Mutex};
use std::time::Duration;

use vixen_api::{
    ApplyRenderBatchOutcome, FullRenderSnapshot, RenderBridgeUpdate, RenderBrokerRequestKind,
    RenderBrokerResponseKind, RenderCommit, RenderMutationBatch, RenderTextQueryBatch,
    RenderTextQueryBatchResult,
};
use vixen_engine::browser::{SynchronousRenderer, SynchronousRendererError};
use vixen_engine::script::RenderLayoutCancellation;

use crate::RenderBroker;
use crate::c_abi::{RendererState, drain_renderer_submissions};

#[cfg(not(test))]
const ENSURE_LAYOUT_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(test)]
const ENSURE_LAYOUT_TIMEOUT: Duration = Duration::from_millis(250);
const MAX_RECOVERY_ATTEMPTS: usize = 2;

pub(crate) struct FlutterSynchronousRenderer {
    renderer: RenderBroker,
    state: Arc<Mutex<RendererState>>,
}

impl FlutterSynchronousRenderer {
    pub(crate) fn new(renderer: RenderBroker, state: Arc<Mutex<RendererState>>) -> Self {
        Self { renderer, state }
    }
}

impl SynchronousRenderer for FlutterSynchronousRenderer {
    fn ensure_layout(
        &self,
        snapshot: FullRenderSnapshot,
        cancellation: &RenderLayoutCancellation,
    ) -> Result<RenderCommit, SynchronousRendererError> {
        let mut force_snapshot = false;
        for _ in 0..MAX_RECOVERY_ATTEMPTS {
            {
                let mut state = self.lock_state()?;
                drain_renderer_submissions(&self.renderer, &mut state).map_err(abi_error)?;
                if !state.needs_resync
                    && let Some(commit) = state.commits.accepted_commit()
                    && commit.revision == snapshot.revision
                    && commit.viewport == snapshot.viewport
                {
                    return Ok(commit.clone());
                }
                force_snapshot |= state.needs_resync;
                publish_renderer_source(
                    &self.renderer,
                    &mut state,
                    snapshot.clone(),
                    force_snapshot,
                )?;
            }

            let response = match self.renderer.request_cancellable(
                RenderBrokerRequestKind::EnsureLayout {
                    required_revision: snapshot.revision,
                },
                ENSURE_LAYOUT_TIMEOUT,
                || cancellation.reason(),
            ) {
                Ok(response) => response,
                Err(_) if cancellation.reason().is_some() => {
                    return Err(SynchronousRendererError::new(
                        "render.cancelled",
                        format!(
                            "EnsureLayout was cancelled: {:?}",
                            cancellation.reason().expect("cancellation checked")
                        ),
                    ));
                }
                Err(error) if error.code == "render.timeout" => {
                    force_snapshot = true;
                    continue;
                }
                Err(error) => return Err(broker_error(error)),
            };
            match response.kind {
                RenderBrokerResponseKind::Commit(response_commit) => {
                    let mut state = self.lock_state()?;
                    if drain_renderer_submissions(&self.renderer, &mut state).is_err() {
                        force_snapshot = true;
                        continue;
                    }
                    let Some(commit) = state.commits.accepted_commit() else {
                        force_snapshot = true;
                        continue;
                    };
                    if commit != &response_commit {
                        force_snapshot = true;
                        continue;
                    }
                    return Ok(commit.clone());
                }
                RenderBrokerResponseKind::Cancelled(reason) => {
                    return Err(SynchronousRendererError::new(
                        "render.cancelled",
                        format!("EnsureLayout was cancelled: {reason:?}"),
                    ));
                }
                RenderBrokerResponseKind::Failed { code, message } => {
                    force_snapshot = true;
                    if cancellation.reason().is_some() {
                        return Err(SynchronousRendererError::new(code, message));
                    }
                }
                kind => {
                    return Err(SynchronousRendererError::new(
                        "render.invalid-response",
                        format!("EnsureLayout returned an unexpected response: {kind:?}"),
                    ));
                }
            }
        }
        Err(SynchronousRendererError::new(
            "render.recovery-failed",
            "EnsureLayout did not recover after a bounded full resync",
        ))
    }

    fn query_text(
        &self,
        query: RenderTextQueryBatch,
        cancellation: &RenderLayoutCancellation,
    ) -> Result<RenderTextQueryBatchResult, SynchronousRendererError> {
        {
            let mut state = self.lock_state()?;
            drain_renderer_submissions(&self.renderer, &mut state).map_err(abi_error)?;
            state
                .commits
                .validate_text_query(&state.replica, &query)
                .map_err(|error| SynchronousRendererError::new(error.code, error.message))?;
        }
        let response = self
            .renderer
            .request_cancellable(
                RenderBrokerRequestKind::TextQuery(query.clone()),
                ENSURE_LAYOUT_TIMEOUT,
                || cancellation.reason(),
            )
            .map_err(broker_error)?;
        match response.kind {
            RenderBrokerResponseKind::TextQuery(result) => {
                let state = self.lock_state()?;
                state
                    .commits
                    .validate_text_query_result(&state.replica, &query, &result)
                    .map_err(|error| SynchronousRendererError::new(error.code, error.message))?;
                Ok(result)
            }
            RenderBrokerResponseKind::Cancelled(reason) => Err(SynchronousRendererError::new(
                "render.cancelled",
                format!("text query was cancelled: {reason:?}"),
            )),
            RenderBrokerResponseKind::Failed { code, message } => {
                Err(SynchronousRendererError::new(code, message))
            }
            kind => Err(SynchronousRendererError::new(
                "render.invalid-response",
                format!("text query returned an unexpected response: {kind:?}"),
            )),
        }
    }
}

pub(crate) fn publish_renderer_source(
    renderer: &RenderBroker,
    state: &mut RendererState,
    snapshot: FullRenderSnapshot,
    force_snapshot: bool,
) -> Result<(), SynchronousRendererError> {
    snapshot
        .validate()
        .map_err(|error| SynchronousRendererError::new(error.code, error.message))?;
    let force_snapshot = force_snapshot || state.needs_resync;
    if !force_snapshot && state.source.as_ref() == Some(&snapshot) {
        state.needs_resync = false;
        return Ok(());
    }

    let mut replica = state.replica.clone();
    let update = if !force_snapshot
        && let Some(base) = state.source.as_ref()
        && base.revision.context_id == snapshot.revision.context_id
        && base.revision.document_id == snapshot.revision.document_id
    {
        match RenderMutationBatch::between(base, &snapshot) {
            Ok(batch) => {
                match replica
                    .apply_batch(batch.clone())
                    .map_err(|error| SynchronousRendererError::new(error.code, error.message))?
                {
                    ApplyRenderBatchOutcome::Applied { .. } => {
                        RenderBridgeUpdate::MutationBatch(batch)
                    }
                    ApplyRenderBatchOutcome::ResyncRequired(_) => {
                        replica
                            .accept_full_snapshot(snapshot.clone())
                            .map_err(|error| {
                                SynchronousRendererError::new(error.code, error.message)
                            })?;
                        RenderBridgeUpdate::FullSnapshot(snapshot.clone())
                    }
                }
            }
            Err(_) => {
                replica
                    .accept_full_snapshot(snapshot.clone())
                    .map_err(|error| SynchronousRendererError::new(error.code, error.message))?;
                RenderBridgeUpdate::FullSnapshot(snapshot.clone())
            }
        }
    } else {
        replica
            .accept_full_snapshot(snapshot.clone())
            .map_err(|error| SynchronousRendererError::new(error.code, error.message))?;
        RenderBridgeUpdate::FullSnapshot(snapshot.clone())
    };

    renderer.publish_update(update).map_err(broker_error)?;
    state.replica = replica;
    state.source = Some(snapshot);
    state.needs_resync = false;
    Ok(())
}

impl FlutterSynchronousRenderer {
    fn lock_state(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, RendererState>, SynchronousRendererError> {
        self.state.lock().map_err(|_| {
            SynchronousRendererError::new(
                "render.internal",
                "renderer acceptance state is unavailable",
            )
        })
    }
}

fn broker_error(error: crate::RenderBrokerError) -> SynchronousRendererError {
    SynchronousRendererError::new(error.code, error.message)
}

fn abi_error(error: crate::c_abi::AbiError) -> SynchronousRendererError {
    SynchronousRendererError::new(error.code, error.message)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    use vixen_api::{
        BrowserCommand, BrowserCommandResult, BrowserEvent, RENDER_PROTOCOL_VERSION,
        RenderBridgeSubmission, RenderBrokerResponse, RenderBrokerResponseKind, RenderCommitId,
        RenderFragmentId, RenderGeometryEntry, RenderHitTestHandle, RenderPresented, RenderRect,
        RenderTextAffinity, RenderTextBox, RenderTextDirection, RenderTextQueryBatchResult,
        RenderTextQueryHandle, RenderTextQueryKind, RenderTextQueryResult, RenderTextQueryValue,
    };

    use super::*;
    use crate::FlutterBrowserController;

    static NEXT_PROFILE: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn same_task_style_mutation_reads_exact_commit_and_reuses_layout() {
        let broker = RenderBroker::new();
        let state = Arc::new(Mutex::new(RendererState::default()));
        let renderer = Arc::new(FlutterSynchronousRenderer::new(
            broker.clone(),
            Arc::clone(&state),
        ));
        let url = "https://layout.test/";
        let mut config = vixen_engine::browser::BrowserConfig::new(profile_path());
        config.document_overrides.insert(
            url.to_owned(),
            "<!doctype html><div id='target' style='width:10px;height:20px'>x</div>".to_owned(),
        );
        config.synchronous_renderer = Some(renderer);
        let mut controller = FlutterBrowserController::from_config(config).unwrap();
        let context_id = controller.create_context().unwrap();
        let navigation_id = controller.navigate(context_id, url).unwrap();
        wait_for_navigation(&mut controller, navigation_id.get());
        controller.set_page_zoom(context_id, 2.0).unwrap();
        let context = controller.context_state(context_id).unwrap();
        let mut browser = controller.subscribe_browser();

        let service = broker.clone();
        let renderer_thread = thread::spawn(move || service_one_layout(service));
        let result = browser
            .dispatch(BrowserCommand::Evaluate {
                context_id,
                document_id: context.document_id,
                runtime_context_id: context.runtime_context_id.unwrap(),
                source: "const e = document.getElementById('target'); e.setAttribute('style', 'width:64px;height:20px'); const a = e.getBoundingClientRect().width; const b = e.getBoundingClientRect().width; const text = e.firstChild; const range = document.createRange(); range.setStart(text, 0); range.setEnd(text, 1); const rangeWidth = range.getBoundingClientRect().width; range.collapse(false); const caretHeight = range.getBoundingClientRect().height; a * 1000000 + b * 10000 + rangeWidth * 100 + caretHeight".to_owned(),
            })
            .unwrap();
        renderer_thread.join().unwrap();

        assert!(
            matches!(
                &result,
                BrowserCommandResult::Evaluation(vixen_api::EvaluationResult {
                    value: vixen_api::ScriptValue::Int32(64_643_112),
                    ..
                })
            ),
            "unexpected geometry evaluation: {result:?}"
        );
        assert!(broker.poll_message(Duration::ZERO).unwrap().is_none());
        assert_eq!(
            state
                .lock()
                .unwrap()
                .commits
                .accepted_commit()
                .unwrap()
                .geometry_index
                .iter()
                .find(|entry| entry.border_box.width == 128.0)
                .unwrap()
                .border_box
                .height,
            40.0
        );
    }

    #[test]
    fn navigation_cancels_ensure_layout_and_late_commit_is_inert() {
        let broker = RenderBroker::new();
        let state = Arc::new(Mutex::new(RendererState::default()));
        let renderer = Arc::new(FlutterSynchronousRenderer::new(
            broker.clone(),
            Arc::clone(&state),
        ));
        let first_url = "https://layout-cancel.test/first";
        let second_url = "https://layout-cancel.test/second";
        let mut config = vixen_engine::browser::BrowserConfig::new(profile_path());
        config.document_overrides.insert(
            first_url.to_owned(),
            "<!doctype html><div id='target' style='width:10px'>x</div>".to_owned(),
        );
        config.document_overrides.insert(
            second_url.to_owned(),
            "<!doctype html><p>replacement</p>".to_owned(),
        );
        config.synchronous_renderer = Some(renderer);
        let mut controller = FlutterBrowserController::from_config(config).unwrap();
        let context_id = controller.create_context().unwrap();
        let navigation_id = controller.navigate(context_id, first_url).unwrap();
        wait_for_navigation(&mut controller, navigation_id.get());
        let context = controller.context_state(context_id).unwrap();
        let mut evaluating_browser = controller.subscribe_browser();

        let service = broker.clone();
        let (request_tx, request_rx) = std::sync::mpsc::sync_channel(1);
        let renderer_thread = thread::spawn(move || {
            assert!(matches!(
                service
                    .poll_message(Duration::from_secs(1))
                    .unwrap()
                    .expect("renderer source update"),
                crate::RenderBrokerMessage::Update(RenderBridgeUpdate::FullSnapshot(_))
            ));
            let request = match service
                .poll_message(Duration::from_secs(1))
                .unwrap()
                .expect("EnsureLayout request")
            {
                crate::RenderBrokerMessage::Request(request) => request,
                other => panic!("expected EnsureLayout request, got {other:?}"),
            };
            request_tx.send(request).unwrap();
        });
        let evaluation = thread::spawn(move || {
            evaluating_browser.dispatch(BrowserCommand::Evaluate {
                context_id,
                document_id: context.document_id,
                runtime_context_id: context.runtime_context_id.unwrap(),
                source: "const e = document.getElementById('target'); e.setAttribute('style', 'width:72px'); e.getBoundingClientRect().width".to_owned(),
            })
        });
        let request = request_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        renderer_thread.join().unwrap();

        let replacement = controller.navigate(context_id, second_url).unwrap();
        let error = evaluation.join().unwrap().unwrap_err();
        assert!(
            error.to_string().contains("cancel") || error.to_string().contains("interrupt"),
            "unexpected cancellation error: {error}"
        );
        assert_eq!(
            broker
                .respond(RenderBrokerResponse {
                    version: RENDER_PROTOCOL_VERSION,
                    request_id: request.request_id,
                    kind: RenderBrokerResponseKind::Cancelled(
                        vixen_api::RenderBrokerCancellation::Navigation,
                    ),
                })
                .unwrap_err()
                .code,
            "render.unknown-request"
        );
        wait_for_navigation(&mut controller, replacement.get());
        assert_eq!(
            controller.context_state(context_id).unwrap().url,
            second_url
        );
    }

    #[test]
    fn stop_cancels_layout_and_keeps_the_runtime_reusable() {
        let broker = RenderBroker::new();
        let state = Arc::new(Mutex::new(RendererState::default()));
        let renderer = Arc::new(FlutterSynchronousRenderer::new(
            broker.clone(),
            Arc::clone(&state),
        ));
        let url = "https://layout-stop.test/";
        let mut config = vixen_engine::browser::BrowserConfig::new(profile_path());
        config.document_overrides.insert(
            url.to_owned(),
            "<!doctype html><div id='target' style='width:10px'>x</div>".to_owned(),
        );
        config.synchronous_renderer = Some(renderer);
        let mut controller = FlutterBrowserController::from_config(config).unwrap();
        let context_id = controller.create_context().unwrap();
        let navigation_id = controller.navigate(context_id, url).unwrap();
        wait_for_navigation(&mut controller, navigation_id.get());
        let context = controller.context_state(context_id).unwrap();
        let mut evaluating_browser = controller.subscribe_browser();

        let service = broker.clone();
        let (request_tx, request_rx) = std::sync::mpsc::sync_channel(1);
        let renderer_thread = thread::spawn(move || {
            assert!(matches!(
                service
                    .poll_message(Duration::from_secs(1))
                    .unwrap()
                    .expect("renderer source update"),
                crate::RenderBrokerMessage::Update(RenderBridgeUpdate::FullSnapshot(_))
            ));
            request_tx.send(next_broker_request(&service)).unwrap();
        });
        let evaluation = thread::spawn(move || {
            let result = evaluating_browser.dispatch(BrowserCommand::Evaluate {
                context_id,
                document_id: context.document_id,
                runtime_context_id: context.runtime_context_id.unwrap(),
                source: "document.getElementById('target').getBoundingClientRect().width"
                    .to_owned(),
            });
            (evaluating_browser, result)
        });
        let request = request_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        renderer_thread.join().unwrap();

        controller.stop(context_id).unwrap();
        let (mut browser, result) = evaluation.join().unwrap();
        assert!(result.is_err());
        assert_eq!(
            broker
                .respond(RenderBrokerResponse {
                    version: RENDER_PROTOCOL_VERSION,
                    request_id: request.request_id,
                    kind: RenderBrokerResponseKind::Cancelled(
                        vixen_api::RenderBrokerCancellation::Stop,
                    ),
                })
                .unwrap_err()
                .code,
            "render.unknown-request"
        );
        assert!(matches!(
            browser
                .dispatch(BrowserCommand::Evaluate {
                    context_id,
                    document_id: context.document_id,
                    runtime_context_id: context.runtime_context_id.unwrap(),
                    source: "1 + 1".to_owned(),
                })
                .unwrap(),
            BrowserCommandResult::Evaluation(vixen_api::EvaluationResult {
                value: vixen_api::ScriptValue::Int32(2),
                ..
            })
        ));
    }

    #[test]
    fn source_publication_uses_exact_batches_and_resyncs_with_a_full_snapshot() {
        let broker = RenderBroker::new();
        let mut state = RendererState::default();
        let base = source_snapshot(1, "10px");
        publish_renderer_source(&broker, &mut state, base.clone(), false).unwrap();
        assert!(matches!(
            broker.poll_message(Duration::ZERO).unwrap(),
            Some(crate::RenderBrokerMessage::Update(
                RenderBridgeUpdate::FullSnapshot(snapshot)
            )) if snapshot == base
        ));

        let target = source_snapshot(2, "64px");
        publish_renderer_source(&broker, &mut state, target.clone(), false).unwrap();
        assert!(matches!(
            broker.poll_message(Duration::ZERO).unwrap(),
            Some(crate::RenderBrokerMessage::Update(
                RenderBridgeUpdate::MutationBatch(batch)
            )) if batch.base_revision == base.revision
                && batch.target_revision == target.revision
                && matches!(batch.mutations.as_slice(), [vixen_api::RenderMutation::UpsertNode(_)])
        ));

        broker
            .submit(RenderBridgeSubmission::Resync(
                vixen_api::RenderResyncRequest::renderer_reset(
                    target.revision.context_id,
                    target.revision.document_id,
                ),
            ))
            .unwrap();
        drain_renderer_submissions(&broker, &mut state).unwrap();
        assert!(state.needs_resync);
        let force_snapshot = state.needs_resync;
        publish_renderer_source(&broker, &mut state, target.clone(), force_snapshot).unwrap();
        assert!(matches!(
            broker.poll_message(Duration::ZERO).unwrap(),
            Some(crate::RenderBrokerMessage::Update(
                RenderBridgeUpdate::FullSnapshot(snapshot)
            )) if snapshot == target
        ));
    }

    #[test]
    fn draining_a_valid_commit_returns_it_for_browsercore_reconciliation() {
        let broker = RenderBroker::new();
        let mut state = RendererState::default();
        let snapshot = source_snapshot(1, "64px");
        publish_renderer_source(&broker, &mut state, snapshot.clone(), false).unwrap();
        let _ = broker.poll_message(Duration::ZERO).unwrap();
        let commit = commit_for_snapshot(&snapshot, 1);
        broker
            .submit(RenderBridgeSubmission::Commit(commit.clone()))
            .unwrap();

        let accepted = drain_renderer_submissions(&broker, &mut state).unwrap();

        assert_eq!(accepted, vec![commit]);
        assert!(state.commits.accepted_commit().is_some());
    }

    #[test]
    fn stale_presented_acknowledgement_is_consumed_without_poisoning_the_queue() {
        let broker = RenderBroker::new();
        let mut state = RendererState::default();
        let base = source_snapshot(1, "10px");
        publish_renderer_source(&broker, &mut state, base.clone(), false).unwrap();
        let _ = broker.poll_message(Duration::ZERO).unwrap();
        let commit = commit_for_snapshot(&base, 1);
        broker
            .submit(RenderBridgeSubmission::Commit(commit.clone()))
            .unwrap();
        drain_renderer_submissions(&broker, &mut state).unwrap();

        let target = source_snapshot(2, "64px");
        publish_renderer_source(&broker, &mut state, target, false).unwrap();
        let _ = broker.poll_message(Duration::ZERO).unwrap();
        broker
            .submit(RenderBridgeSubmission::Presented(RenderPresented {
                version: vixen_api::RENDER_PROTOCOL_VERSION,
                context_id: commit.revision.context_id,
                document_id: commit.revision.document_id,
                commit_id: commit.commit_id,
                revision: commit.revision,
            }))
            .unwrap();

        assert!(
            drain_renderer_submissions(&broker, &mut state)
                .unwrap()
                .is_empty()
        );
        assert!(broker.peek_submission().unwrap().is_none());
        assert!(state.commits.presented_commit().is_none());
    }

    #[test]
    fn stale_commit_is_released_without_poisoning_the_queue() {
        let broker = RenderBroker::new();
        let mut state = RendererState::default();
        let base = source_snapshot(1, "10px");
        publish_renderer_source(&broker, &mut state, base.clone(), false).unwrap();
        let _ = broker.poll_message(Duration::ZERO).unwrap();

        let target = source_snapshot(2, "64px");
        publish_renderer_source(&broker, &mut state, target.clone(), false).unwrap();
        let _ = broker.poll_message(Duration::ZERO).unwrap();
        let stale = commit_for_snapshot(&base, 1);
        broker
            .submit(RenderBridgeSubmission::Commit(stale.clone()))
            .unwrap();

        assert!(
            drain_renderer_submissions(&broker, &mut state)
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            broker.poll_message(Duration::ZERO).unwrap(),
            Some(crate::RenderBrokerMessage::Update(
                RenderBridgeUpdate::ReleaseHandles(release)
            )) if release.commit_id == stale.commit_id
                && release.hit_test_handle == stale.hit_test_handle
                && release.text_query_handle == stale.text_query_handle
        ));
        assert!(broker.peek_submission().unwrap().is_none());
        assert!(state.commits.accepted_commit().is_none());

        let current = commit_for_snapshot(&target, 2);
        broker
            .submit(RenderBridgeSubmission::Commit(current.clone()))
            .unwrap();
        assert_eq!(
            drain_renderer_submissions(&broker, &mut state).unwrap(),
            vec![current]
        );
    }

    #[test]
    fn pending_resync_forces_an_unchanged_full_snapshot() {
        let broker = RenderBroker::new();
        let mut state = RendererState::default();
        let snapshot = source_snapshot(1, "64px");
        publish_renderer_source(&broker, &mut state, snapshot.clone(), false).unwrap();
        let _ = broker.poll_message(Duration::ZERO).unwrap();
        state.needs_resync = true;

        publish_renderer_source(&broker, &mut state, snapshot.clone(), false).unwrap();

        assert!(matches!(
            broker.poll_message(Duration::ZERO).unwrap(),
            Some(crate::RenderBrokerMessage::Update(
                RenderBridgeUpdate::FullSnapshot(republished)
            )) if republished == snapshot
        ));
        assert!(!state.needs_resync);
    }

    #[test]
    fn malformed_commit_recovers_once_and_does_not_poison_reuse() {
        let broker = RenderBroker::new();
        let state = Arc::new(Mutex::new(RendererState::default()));
        let renderer = Arc::new(FlutterSynchronousRenderer::new(
            broker.clone(),
            Arc::clone(&state),
        ));
        let snapshot = source_snapshot(1, "64px");
        let requested = snapshot.clone();
        let requesting_renderer = Arc::clone(&renderer);
        let request = thread::spawn(move || {
            requesting_renderer.ensure_layout(
                requested,
                &vixen_engine::script::RenderLayoutCancellation::default(),
            )
        });

        assert!(matches!(
            next_source_update(&broker),
            RenderBridgeUpdate::FullSnapshot(current) if current == snapshot
        ));
        let first_request = next_broker_request(&broker);
        let mut malformed = commit_for_snapshot(&snapshot, 1);
        malformed.geometry_index[0].border_box.x = f64::NAN;
        broker
            .submit(RenderBridgeSubmission::Commit(malformed.clone()))
            .unwrap();
        broker
            .respond(RenderBrokerResponse {
                version: RENDER_PROTOCOL_VERSION,
                request_id: first_request.request_id,
                kind: RenderBrokerResponseKind::Commit(malformed),
            })
            .unwrap();

        assert!(matches!(
            next_source_update(&broker),
            RenderBridgeUpdate::FullSnapshot(current) if current == snapshot
        ));
        let second_request = next_broker_request(&broker);
        let recovered = commit_for_snapshot(&snapshot, 2);
        broker
            .submit(RenderBridgeSubmission::Commit(recovered.clone()))
            .unwrap();
        broker
            .respond(RenderBrokerResponse {
                version: RENDER_PROTOCOL_VERSION,
                request_id: second_request.request_id,
                kind: RenderBrokerResponseKind::Commit(recovered.clone()),
            })
            .unwrap();

        assert_eq!(request.join().unwrap().unwrap(), recovered);
        assert_eq!(
            renderer
                .ensure_layout(
                    snapshot,
                    &vixen_engine::script::RenderLayoutCancellation::default(),
                )
                .unwrap()
                .commit_id,
            RenderCommitId::new(2).unwrap()
        );
        assert!(broker.poll_message(Duration::ZERO).unwrap().is_none());
    }

    fn service_one_layout(broker: RenderBroker) {
        let update = broker
            .poll_message(Duration::from_secs(1))
            .unwrap()
            .expect("renderer source update");
        let snapshot = match update {
            crate::RenderBrokerMessage::Update(RenderBridgeUpdate::FullSnapshot(snapshot)) => {
                snapshot
            }
            other => panic!("expected full renderer snapshot, got {other:?}"),
        };
        let target = snapshot
            .nodes
            .iter()
            .find(|node| {
                node.styles
                    .iter()
                    .any(|style| style.name == "width" && style.value == "64px")
            })
            .expect("mutated target render node");
        let output_scale = snapshot.viewport.device_scale * snapshot.viewport.page_zoom;
        let geometry_index = snapshot
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                let target_node = node.id == target.id;
                let rect = RenderRect {
                    x: 0.0,
                    y: index as f64 * 24.0 * output_scale,
                    width: if target_node { 64.0 } else { 800.0 } * output_scale,
                    height: if target_node { 20.0 } else { 24.0 } * output_scale,
                };
                RenderGeometryEntry {
                    node_id: node.id,
                    fragment_id: RenderFragmentId::new(index as u64 + 1).unwrap(),
                    border_box: rect,
                    padding_box: rect,
                    content_box: rect,
                    clip: None,
                    scroll_node_id: None,
                    paint_order: index as u32,
                }
            })
            .collect();
        let commit = RenderCommit {
            version: RENDER_PROTOCOL_VERSION,
            commit_id: RenderCommitId::new(1).unwrap(),
            revision: snapshot.revision,
            viewport: snapshot.viewport,
            geometry_index,
            hit_test_handle: RenderHitTestHandle::new(1).unwrap(),
            text_query_handle: RenderTextQueryHandle::new(1).unwrap(),
            scroll_snapshot: Vec::new(),
            semantic_bounds: Vec::new(),
            truncations: Vec::new(),
        };
        broker
            .submit(RenderBridgeSubmission::Commit(commit.clone()))
            .unwrap();
        let request = match broker
            .poll_message(Duration::from_secs(1))
            .unwrap()
            .expect("EnsureLayout request")
        {
            crate::RenderBrokerMessage::Request(request) => request,
            other => panic!("expected EnsureLayout request, got {other:?}"),
        };
        assert!(matches!(
            request.kind,
            RenderBrokerRequestKind::EnsureLayout { required_revision }
                if required_revision == snapshot.revision
        ));
        broker
            .respond(RenderBrokerResponse {
                version: RENDER_PROTOCOL_VERSION,
                request_id: request.request_id,
                kind: RenderBrokerResponseKind::Commit(commit),
            })
            .unwrap();
        for _ in 0..2 {
            let request = match broker
                .poll_message(Duration::from_secs(1))
                .unwrap()
                .expect("text query request")
            {
                crate::RenderBrokerMessage::Request(request) => request,
                other => panic!("expected text query request, got {other:?}"),
            };
            let RenderBrokerRequestKind::TextQuery(batch) = &request.kind else {
                panic!("expected text query request")
            };
            let results = batch
                .queries
                .iter()
                .map(|query| RenderTextQueryResult {
                    query_id: query.query_id,
                    value: match query.kind {
                        RenderTextQueryKind::CaretForOffset { affinity, .. } => {
                            RenderTextQueryValue::Caret {
                                rect: RenderRect {
                                    x: 31.0 * output_scale,
                                    y: 0.0,
                                    width: output_scale,
                                    height: 12.0 * output_scale,
                                },
                                affinity,
                            }
                        }
                        RenderTextQueryKind::RangeBoxes { .. } => {
                            RenderTextQueryValue::RangeBoxes(vec![RenderTextBox {
                                rect: RenderRect {
                                    x: 0.0,
                                    y: 0.0,
                                    width: 31.0 * output_scale,
                                    height: 12.0 * output_scale,
                                },
                                direction: RenderTextDirection::LeftToRight,
                            }])
                        }
                        RenderTextQueryKind::OffsetForPoint { .. } => {
                            RenderTextQueryValue::Offset {
                                utf16_offset: 0,
                                affinity: RenderTextAffinity::Downstream,
                            }
                        }
                    },
                })
                .collect();
            broker
                .respond(RenderBrokerResponse {
                    version: RENDER_PROTOCOL_VERSION,
                    request_id: request.request_id,
                    kind: RenderBrokerResponseKind::TextQuery(RenderTextQueryBatchResult {
                        version: RENDER_PROTOCOL_VERSION,
                        context_id: batch.context_id,
                        document_id: batch.document_id,
                        commit_id: batch.commit_id,
                        revision: batch.revision,
                        results,
                        truncations: Vec::new(),
                    }),
                })
                .unwrap();
        }
    }

    fn next_source_update(broker: &RenderBroker) -> RenderBridgeUpdate {
        loop {
            match broker
                .poll_message(Duration::from_secs(1))
                .unwrap()
                .expect("renderer source update")
            {
                crate::RenderBrokerMessage::Update(RenderBridgeUpdate::ReleaseHandles(_)) => {}
                crate::RenderBrokerMessage::Update(update) => return update,
                other => panic!("expected renderer source update, got {other:?}"),
            }
        }
    }

    fn next_broker_request(broker: &RenderBroker) -> vixen_api::RenderBrokerRequest {
        match broker
            .poll_message(Duration::from_secs(1))
            .unwrap()
            .expect("renderer request")
        {
            crate::RenderBrokerMessage::Request(request) => request,
            other => panic!("expected renderer request, got {other:?}"),
        }
    }

    fn wait_for_navigation(controller: &mut FlutterBrowserController, navigation_id: u64) {
        for _ in 0..64 {
            let event = controller
                .wait_next_event(Duration::from_secs(1))
                .unwrap()
                .expect("navigation event");
            if matches!(
                event,
                BrowserEvent::NavigationPhaseChanged {
                    navigation_id: current,
                    phase: vixen_api::NavigationPhase::Settled,
                    ..
                } if current.get() == navigation_id
            ) {
                return;
            }
        }
        panic!("navigation did not settle");
    }

    fn source_snapshot(generation: u64, width: &str) -> FullRenderSnapshot {
        let revision = vixen_api::RenderRevision {
            context_id: vixen_api::BrowsingContextId::new(1).unwrap(),
            document_id: vixen_api::DocumentId::new(2).unwrap(),
            source_generation: generation,
            style_generation: generation,
            viewport_generation: 1,
            resource_generation: generation,
        };
        let mut snapshot = FullRenderSnapshot::new(
            revision,
            vixen_api::RenderViewport {
                width: 800,
                height: 600,
                device_scale: 1.0,
                page_zoom: 1.0,
            },
        );
        snapshot.nodes.push(vixen_api::RenderNode {
            id: vixen_api::RenderNodeId::new(1).unwrap(),
            parent_id: None,
            sibling_index: 0,
            depth: 0,
            kind: vixen_api::RenderNodeKind::Element {
                local_name: "div".to_owned(),
            },
            styles: vec![vixen_api::RenderStyleProperty {
                name: "width".to_owned(),
                value: width.to_owned(),
            }],
            resource_ids: Vec::new(),
            semantic: None,
        });
        snapshot
    }

    fn commit_for_snapshot(snapshot: &FullRenderSnapshot, commit_id: u64) -> RenderCommit {
        let rect = RenderRect {
            x: 0.0,
            y: 0.0,
            width: 64.0,
            height: 20.0,
        };
        RenderCommit {
            version: RENDER_PROTOCOL_VERSION,
            commit_id: RenderCommitId::new(commit_id).unwrap(),
            revision: snapshot.revision,
            viewport: snapshot.viewport,
            geometry_index: snapshot
                .nodes
                .iter()
                .enumerate()
                .map(|(index, node)| RenderGeometryEntry {
                    node_id: node.id,
                    fragment_id: RenderFragmentId::new(index as u64 + 1).unwrap(),
                    border_box: rect,
                    padding_box: rect,
                    content_box: rect,
                    clip: None,
                    scroll_node_id: None,
                    paint_order: index as u32,
                })
                .collect(),
            hit_test_handle: RenderHitTestHandle::new(commit_id).unwrap(),
            text_query_handle: RenderTextQueryHandle::new(commit_id).unwrap(),
            scroll_snapshot: Vec::new(),
            semantic_bounds: Vec::new(),
            truncations: Vec::new(),
        }
    }

    fn profile_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-profiles")
            .join(format!(
                "vixen-sync-layout-{}-{}.redb",
                std::process::id(),
                NEXT_PROFILE.fetch_add(1, Ordering::Relaxed)
            ))
    }
}
