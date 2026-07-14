//! Bounded renderer channel independent of the browser command worker.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use vixen_api::{
    RENDER_PROTOCOL_VERSION, RenderBridgeSubmission, RenderBridgeUpdate, RenderBrokerCancellation,
    RenderBrokerRequest, RenderBrokerRequestKind, RenderBrokerResponse, RenderBrokerResponseKind,
    RenderMutation, RenderNode, RenderRequestId, RenderResource,
};

/// Total queued plus polled requests awaiting a response.
pub const RENDER_BROKER_QUEUE_CAPACITY: usize = 64;
pub const RENDER_BROKER_MAX_UPDATE_SOURCE_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum RenderBrokerMessage {
    Request(RenderBrokerRequest),
    Update(RenderBridgeUpdate),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderBrokerError {
    pub code: &'static str,
    pub message: String,
}

impl RenderBrokerError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RenderBrokerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for RenderBrokerError {}

struct PendingRequest {
    request: RenderBrokerRequest,
    response: mpsc::SyncSender<RenderBrokerResponse>,
    deadline: Instant,
}

enum QueuedMessage {
    Request(RenderRequestId),
    Update(RenderBridgeUpdate),
}

#[derive(Default)]
struct State {
    closed: bool,
    outbound: VecDeque<QueuedMessage>,
    pending: HashMap<RenderRequestId, PendingRequest>,
    submissions: VecDeque<RenderBridgeSubmission>,
}

struct Inner {
    next_request_id: AtomicU64,
    state: Mutex<State>,
    request_ready: Condvar,
}

/// Cloneable endpoint over one bounded request queue and exact response map.
/// It owns no BrowserCore or renderer state and never calls either side.
#[derive(Clone)]
pub struct RenderBroker {
    inner: Arc<Inner>,
}

impl Default for RenderBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderBroker {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                next_request_id: AtomicU64::new(1),
                state: Mutex::new(State::default()),
                request_ready: Condvar::new(),
            }),
        }
    }

    pub fn request(
        &self,
        kind: RenderBrokerRequestKind,
        timeout: Duration,
    ) -> Result<RenderBrokerResponse, RenderBrokerError> {
        let raw_id = self
            .inner
            .next_request_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| RenderBrokerError::new("render.id-exhausted", "request id exhausted"))?;
        let request_id = RenderRequestId::new(raw_id)
            .ok_or_else(|| RenderBrokerError::new("render.id-exhausted", "request id is zero"))?;
        let request = RenderBrokerRequest {
            version: RENDER_PROTOCOL_VERSION,
            request_id,
            kind,
        };
        request
            .validate()
            .map_err(|error| RenderBrokerError::new(error.code, error.message))?;
        let registered_at = Instant::now();
        let deadline = registered_at.checked_add(timeout).unwrap_or(registered_at);
        let (response, receive_response) = mpsc::sync_channel(1);
        {
            let mut state = self.lock_state()?;
            if state.closed {
                return Err(closed_error());
            }
            if state.pending.len() >= RENDER_BROKER_QUEUE_CAPACITY {
                return Err(RenderBrokerError::new(
                    "render.queue-full",
                    "renderer in-flight request limit reached",
                ));
            }
            state.pending.insert(
                request_id,
                PendingRequest {
                    request,
                    response,
                    deadline,
                },
            );
            state.outbound.push_back(QueuedMessage::Request(request_id));
            self.inner.request_ready.notify_one();
        }

        match receive_response.recv_timeout(timeout) {
            Ok(response) => Ok(response),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.remove_pending(request_id)?;
                Err(RenderBrokerError::new(
                    "render.timeout",
                    "renderer request deadline expired",
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.remove_pending(request_id)?;
                Err(closed_error())
            }
        }
    }

    pub fn poll_request(
        &self,
        timeout: Duration,
    ) -> Result<Option<RenderBrokerRequest>, RenderBrokerError> {
        let started_at = Instant::now();
        let deadline = started_at.checked_add(timeout).unwrap_or(started_at);
        let mut state = self.lock_state()?;
        loop {
            while let Some(index) = state
                .outbound
                .iter()
                .position(|message| matches!(message, QueuedMessage::Request(_)))
            {
                let Some(QueuedMessage::Request(request_id)) = state.outbound.remove(index) else {
                    unreachable!("located request queue entry changed")
                };
                let Some(pending) = state.pending.get(&request_id) else {
                    continue;
                };
                if Instant::now() >= pending.deadline {
                    continue;
                }
                return Ok(Some(pending.request.clone()));
            }
            if state.closed {
                return Err(closed_error());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            let (next_state, result) = self
                .inner
                .request_ready
                .wait_timeout(state, remaining)
                .map_err(|_| internal_error("renderer broker state is unavailable"))?;
            state = next_state;
            if result.timed_out()
                && !state
                    .outbound
                    .iter()
                    .any(|message| matches!(message, QueuedMessage::Request(_)))
            {
                return Ok(None);
            }
        }
    }

    pub fn publish_update(&self, update: RenderBridgeUpdate) -> Result<(), RenderBrokerError> {
        update
            .validate()
            .map_err(|error| RenderBrokerError::new(error.code, error.message))?;
        let source_size = update_source_size(&update).ok_or_else(|| {
            RenderBrokerError::new(
                "render.payload-too-large",
                "renderer update source size overflowed",
            )
        })?;
        if source_size > RENDER_BROKER_MAX_UPDATE_SOURCE_BYTES {
            return Err(RenderBrokerError::new(
                "render.payload-too-large",
                format!(
                    "renderer update source is {source_size} bytes; transport source limit is {RENDER_BROKER_MAX_UPDATE_SOURCE_BYTES}"
                ),
            ));
        }
        let encoded_size = serde_json::to_vec(&crate::render_wire::update_json(&update))
            .map_err(|_| internal_error("renderer update could not be encoded"))?
            .len();
        if encoded_size > crate::c_abi::VIXEN_MAX_OUTPUT_BYTES {
            return Err(RenderBrokerError::new(
                "render.payload-too-large",
                format!(
                    "renderer update is {encoded_size} bytes; transport limit is {}",
                    crate::c_abi::VIXEN_MAX_OUTPUT_BYTES
                ),
            ));
        }
        let mut state = self.lock_state()?;
        if state.closed {
            return Err(closed_error());
        }
        if state
            .outbound
            .iter()
            .filter(|message| matches!(message, QueuedMessage::Update(_)))
            .count()
            >= RENDER_BROKER_QUEUE_CAPACITY
        {
            return Err(RenderBrokerError::new(
                "render.queue-full",
                "renderer update queue is full",
            ));
        }
        state.outbound.push_back(QueuedMessage::Update(update));
        self.inner.request_ready.notify_one();
        Ok(())
    }

    pub fn poll_message(
        &self,
        timeout: Duration,
    ) -> Result<Option<RenderBrokerMessage>, RenderBrokerError> {
        let started_at = Instant::now();
        let deadline = started_at.checked_add(timeout).unwrap_or(started_at);
        let mut state = self.lock_state()?;
        loop {
            while let Some(message) = state.outbound.pop_front() {
                match message {
                    QueuedMessage::Request(request_id) => {
                        let Some(pending) = state.pending.get(&request_id) else {
                            continue;
                        };
                        if Instant::now() >= pending.deadline {
                            continue;
                        }
                        return Ok(Some(RenderBrokerMessage::Request(pending.request.clone())));
                    }
                    QueuedMessage::Update(update) => {
                        return Ok(Some(RenderBrokerMessage::Update(update)));
                    }
                }
            }
            if state.closed {
                return Err(closed_error());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            let (next_state, result) = self
                .inner
                .request_ready
                .wait_timeout(state, remaining)
                .map_err(|_| internal_error("renderer broker state is unavailable"))?;
            state = next_state;
            if result.timed_out() && state.outbound.is_empty() {
                return Ok(None);
            }
        }
    }

    pub fn submit(&self, submission: RenderBridgeSubmission) -> Result<(), RenderBrokerError> {
        submission
            .validate_transport()
            .map_err(|error| RenderBrokerError::new(error.code, error.message))?;
        let mut state = self.lock_state()?;
        if state.closed {
            return Err(closed_error());
        }
        if state.submissions.len() >= RENDER_BROKER_QUEUE_CAPACITY {
            return Err(RenderBrokerError::new(
                "render.queue-full",
                "renderer submission queue is full",
            ));
        }
        state.submissions.push_back(submission);
        Ok(())
    }

    pub fn poll_submission(&self) -> Result<Option<RenderBridgeSubmission>, RenderBrokerError> {
        let mut state = self.lock_state()?;
        if state.closed {
            return Err(closed_error());
        }
        Ok(state.submissions.pop_front())
    }

    pub fn respond(&self, response: RenderBrokerResponse) -> Result<(), RenderBrokerError> {
        let pending = {
            let mut state = self.lock_state()?;
            if state
                .pending
                .get(&response.request_id)
                .is_some_and(|pending| Instant::now() >= pending.deadline)
            {
                return Err(RenderBrokerError::new(
                    "render.stale",
                    "renderer response arrived after the request deadline",
                ));
            }
            let pending = state
                .pending
                .remove(&response.request_id)
                .ok_or_else(unknown_request_error)?;
            state.outbound.retain(|message| {
                !matches!(message, QueuedMessage::Request(request_id) if *request_id == response.request_id)
            });
            pending
        };
        if let Err(error) = response.validate_for(&pending.request) {
            let _ = pending.response.send(RenderBrokerResponse {
                version: RENDER_PROTOCOL_VERSION,
                request_id: response.request_id,
                kind: RenderBrokerResponseKind::Failed {
                    code: error.code.to_owned(),
                    message: error.message.clone(),
                },
            });
            return Err(RenderBrokerError::new(error.code, error.message));
        }
        pending.response.send(response).map_err(|_| {
            RenderBrokerError::new(
                "render.stale",
                "renderer response arrived after requester cancellation",
            )
        })
    }

    pub fn cancel(
        &self,
        request_id: RenderRequestId,
        reason: RenderBrokerCancellation,
    ) -> Result<bool, RenderBrokerError> {
        let Some(pending) = self.remove_pending(request_id)? else {
            return Ok(false);
        };
        let _ = pending.response.send(RenderBrokerResponse {
            version: RENDER_PROTOCOL_VERSION,
            request_id,
            kind: RenderBrokerResponseKind::Cancelled(reason),
        });
        Ok(true)
    }

    pub fn shutdown(&self) -> Result<(), RenderBrokerError> {
        let pending = {
            let mut state = self.lock_state()?;
            if state.closed {
                return Ok(());
            }
            state.closed = true;
            state.outbound.clear();
            state.submissions.clear();
            self.inner.request_ready.notify_all();
            std::mem::take(&mut state.pending)
        };
        for (request_id, request) in pending {
            let _ = request.response.send(RenderBrokerResponse {
                version: RENDER_PROTOCOL_VERSION,
                request_id,
                kind: RenderBrokerResponseKind::Cancelled(RenderBrokerCancellation::Shutdown),
            });
        }
        Ok(())
    }

    fn remove_pending(
        &self,
        request_id: RenderRequestId,
    ) -> Result<Option<PendingRequest>, RenderBrokerError> {
        let mut state = self.lock_state()?;
        state.outbound.retain(
            |message| !matches!(message, QueuedMessage::Request(queued) if *queued == request_id),
        );
        Ok(state.pending.remove(&request_id))
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, State>, RenderBrokerError> {
        self.inner
            .state
            .lock()
            .map_err(|_| internal_error("renderer broker state is unavailable"))
    }
}

fn closed_error() -> RenderBrokerError {
    RenderBrokerError::new("render.closed", "renderer broker is closed")
}

fn unknown_request_error() -> RenderBrokerError {
    RenderBrokerError::new(
        "render.unknown-request",
        "renderer response is stale, duplicate, or unknown",
    )
}

fn internal_error(message: &'static str) -> RenderBrokerError {
    RenderBrokerError::new("render.internal", message)
}

fn update_source_size(update: &RenderBridgeUpdate) -> Option<usize> {
    match update {
        RenderBridgeUpdate::FullSnapshot(snapshot) => snapshot
            .nodes
            .iter()
            .try_fold(0usize, |size, node| {
                size.checked_add(node_source_size(node)?)
            })?
            .checked_add(
                snapshot
                    .resources
                    .iter()
                    .try_fold(0usize, |size, resource| {
                        size.checked_add(resource_source_size(resource)?)
                    })?,
            ),
        RenderBridgeUpdate::MutationBatch(batch) => {
            batch.mutations.iter().try_fold(0usize, |size, mutation| {
                let mutation_size = match mutation {
                    RenderMutation::UpsertNode(node) => node_source_size(node)?,
                    RenderMutation::UpsertResource(resource) => resource_source_size(resource)?,
                    RenderMutation::SetViewport(_)
                    | RenderMutation::RemoveNode { .. }
                    | RenderMutation::RemoveResource { .. }
                    | RenderMutation::SetScrollIntent(_)
                    | RenderMutation::RemoveScrollIntent { .. } => 0,
                };
                size.checked_add(mutation_size)
            })
        }
        RenderBridgeUpdate::ReleaseHandles(_) => Some(0),
    }
}

fn node_source_size(node: &RenderNode) -> Option<usize> {
    let kind_size = match &node.kind {
        vixen_api::RenderNodeKind::Element { local_name } => local_name.len(),
        vixen_api::RenderNodeKind::Text { text }
        | vixen_api::RenderNodeKind::PseudoBefore { text }
        | vixen_api::RenderNodeKind::PseudoAfter { text } => text.len(),
    };
    let styles_size = node.styles.iter().try_fold(0usize, |size, style| {
        size.checked_add(style.name.len())?
            .checked_add(style.value.len())
    })?;
    let semantic_size = node.semantic.as_ref().map_or(Some(0), |semantic| {
        semantic
            .role
            .len()
            .checked_add(semantic.name.len())?
            .checked_add(semantic.value.as_ref().map_or(0, String::len))
    })?;
    kind_size
        .checked_add(styles_size)?
        .checked_add(semantic_size)
}

fn resource_source_size(resource: &RenderResource) -> Option<usize> {
    resource.mime.len().checked_add(resource.bytes.len())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier, Mutex};
    use std::thread;

    use vixen_api::{BrowsingContextId, DocumentId, RenderBrokerResponseKind, RenderRevision};

    use super::*;

    fn revision() -> RenderRevision {
        RenderRevision {
            context_id: BrowsingContextId::new(1).unwrap(),
            document_id: DocumentId::new(2).unwrap(),
            source_generation: 3,
            style_generation: 4,
            viewport_generation: 5,
            resource_generation: 6,
        }
    }

    fn request_kind() -> RenderBrokerRequestKind {
        RenderBrokerRequestKind::EnsureLayout {
            required_revision: revision(),
        }
    }

    #[test]
    fn request_response_progresses_while_unrelated_worker_is_blocked() {
        let broker = RenderBroker::new();
        let worker_lock = Arc::new(Mutex::new(()));
        let held = worker_lock.lock().unwrap();
        let requester = broker.clone();
        let join = thread::spawn(move || requester.request(request_kind(), Duration::from_secs(1)));
        let request = broker
            .poll_request(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        broker
            .respond(RenderBrokerResponse {
                version: RENDER_PROTOCOL_VERSION,
                request_id: request.request_id,
                kind: RenderBrokerResponseKind::Cancelled(RenderBrokerCancellation::Stop),
            })
            .unwrap();
        assert!(matches!(
            join.join().unwrap().unwrap().kind,
            RenderBrokerResponseKind::Cancelled(RenderBrokerCancellation::Stop)
        ));
        drop(held);
    }

    #[test]
    fn timeout_late_response_and_shutdown_fail_closed() {
        let broker = RenderBroker::new();
        let requester = broker.clone();
        let join =
            thread::spawn(move || requester.request(request_kind(), Duration::from_millis(20)));
        let request = broker
            .poll_request(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert_eq!(join.join().unwrap().unwrap_err().code, "render.timeout");
        assert_eq!(
            broker
                .respond(RenderBrokerResponse {
                    version: RENDER_PROTOCOL_VERSION,
                    request_id: request.request_id,
                    kind: RenderBrokerResponseKind::Cancelled(RenderBrokerCancellation::Deadline,),
                })
                .unwrap_err()
                .code,
            "render.unknown-request"
        );
        broker.shutdown().unwrap();
        assert_eq!(
            broker.poll_request(Duration::ZERO).unwrap_err().code,
            "render.closed"
        );
    }

    #[test]
    fn capacity_bounds_all_polled_requests_until_response_or_cancel() {
        let broker = RenderBroker::new();
        let mut joins = Vec::new();
        for _ in 0..RENDER_BROKER_QUEUE_CAPACITY {
            let requester = broker.clone();
            joins.push(thread::spawn(move || {
                requester.request(request_kind(), Duration::from_secs(5))
            }));
        }
        let mut request_ids = Vec::new();
        while request_ids.len() < RENDER_BROKER_QUEUE_CAPACITY {
            if let Some(request) = broker.poll_request(Duration::from_secs(1)).unwrap() {
                request_ids.push(request.request_id);
            }
        }
        assert_eq!(
            broker
                .request(request_kind(), Duration::from_millis(10))
                .unwrap_err()
                .code,
            "render.queue-full"
        );
        for request_id in request_ids {
            assert!(
                broker
                    .cancel(request_id, RenderBrokerCancellation::Navigation)
                    .unwrap()
            );
        }
        for join in joins {
            assert!(matches!(
                join.join().unwrap().unwrap().kind,
                RenderBrokerResponseKind::Cancelled(RenderBrokerCancellation::Navigation)
            ));
        }
    }

    #[test]
    fn shutdown_wakes_blocked_poll_and_all_registered_requesters() {
        let broker = RenderBroker::new();
        let barrier = Arc::new(Barrier::new(2));
        let polling = broker.clone();
        let polling_barrier = barrier.clone();
        let poll = thread::spawn(move || {
            polling_barrier.wait();
            polling.poll_request(Duration::from_secs(30))
        });
        barrier.wait();
        broker.shutdown().unwrap();
        assert_eq!(poll.join().unwrap().unwrap_err().code, "render.closed");

        let broker = RenderBroker::new();
        let requester = broker.clone();
        let request =
            thread::spawn(move || requester.request(request_kind(), Duration::from_secs(30)));
        let deadline = Instant::now() + Duration::from_secs(1);
        while broker.inner.state.lock().unwrap().pending.is_empty() {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        broker.shutdown().unwrap();

        assert!(matches!(
            request.join().unwrap().unwrap().kind,
            RenderBrokerResponseKind::Cancelled(RenderBrokerCancellation::Shutdown)
        ));
        assert_eq!(
            broker
                .request(request_kind(), Duration::from_secs(1))
                .unwrap_err()
                .code,
            "render.closed"
        );
    }

    #[test]
    fn asynchronous_updates_are_bounded_and_preserve_order() {
        use vixen_api::{FullRenderSnapshot, RenderBridgeUpdate, RenderViewport};

        let broker = RenderBroker::new();
        for generation in 1..=RENDER_BROKER_QUEUE_CAPACITY {
            broker
                .publish_update(RenderBridgeUpdate::FullSnapshot(FullRenderSnapshot::new(
                    RenderRevision {
                        source_generation: generation as u64,
                        ..revision()
                    },
                    RenderViewport {
                        width: 100,
                        height: 100,
                        device_scale: 1.0,
                        page_zoom: 1.0,
                    },
                )))
                .unwrap();
        }
        assert_eq!(
            broker
                .publish_update(RenderBridgeUpdate::FullSnapshot(FullRenderSnapshot::new(
                    revision(),
                    RenderViewport {
                        width: 100,
                        height: 100,
                        device_scale: 1.0,
                        page_zoom: 1.0,
                    },
                ),))
                .unwrap_err()
                .code,
            "render.queue-full"
        );
        for expected in 1..=RENDER_BROKER_QUEUE_CAPACITY {
            let Some(RenderBrokerMessage::Update(RenderBridgeUpdate::FullSnapshot(snapshot))) =
                broker.poll_message(Duration::ZERO).unwrap()
            else {
                panic!("expected a full snapshot update")
            };
            assert_eq!(snapshot.revision.source_generation, expected as u64);
        }
        assert!(broker.poll_message(Duration::ZERO).unwrap().is_none());
    }

    #[test]
    fn update_payload_limit_is_checked_before_queue_ownership_changes() {
        use vixen_api::{
            FullRenderSnapshot, RenderBridgeUpdate, RenderResource, RenderResourceId,
            RenderResourceKind, RenderViewport,
        };

        let broker = RenderBroker::new();
        let mut snapshot = FullRenderSnapshot::new(
            revision(),
            RenderViewport {
                width: 100,
                height: 100,
                device_scale: 1.0,
                page_zoom: 1.0,
            },
        );
        snapshot.resources.push(RenderResource {
            id: RenderResourceId::new(1).unwrap(),
            kind: RenderResourceKind::Image,
            mime: "image/png".to_owned(),
            bytes: vec![0; 800_000],
        });
        assert_eq!(
            broker
                .publish_update(RenderBridgeUpdate::FullSnapshot(snapshot))
                .unwrap_err()
                .code,
            "render.payload-too-large"
        );
        assert!(broker.poll_message(Duration::ZERO).unwrap().is_none());
    }

    #[test]
    fn asynchronous_submissions_are_bounded_until_consumed() {
        use vixen_api::{RenderBridgeSubmission, RenderResyncRequest};

        let broker = RenderBroker::new();
        for _ in 0..RENDER_BROKER_QUEUE_CAPACITY {
            broker
                .submit(RenderBridgeSubmission::Resync(
                    RenderResyncRequest::renderer_reset(
                        BrowsingContextId::new(1).unwrap(),
                        DocumentId::new(2).unwrap(),
                    ),
                ))
                .unwrap();
        }
        assert_eq!(
            broker
                .submit(RenderBridgeSubmission::Resync(
                    RenderResyncRequest::renderer_reset(
                        BrowsingContextId::new(1).unwrap(),
                        DocumentId::new(2).unwrap(),
                    ),
                ))
                .unwrap_err()
                .code,
            "render.queue-full"
        );
        for _ in 0..RENDER_BROKER_QUEUE_CAPACITY {
            assert!(broker.poll_submission().unwrap().is_some());
        }
        assert!(broker.poll_submission().unwrap().is_none());
    }
}
