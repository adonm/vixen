//! Bounded renderer channel independent of the browser command worker.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use vixen_api::{
    RENDER_PROTOCOL_VERSION, RenderBrokerCancellation, RenderBrokerRequest,
    RenderBrokerRequestKind, RenderBrokerResponse, RenderBrokerResponseKind, RenderRequestId,
};

pub const RENDER_BROKER_QUEUE_CAPACITY: usize = 64;

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
}

struct Inner {
    next_request_id: AtomicU64,
    closed: AtomicBool,
    requests: mpsc::SyncSender<RenderBrokerRequest>,
    receiver: Mutex<mpsc::Receiver<RenderBrokerRequest>>,
    pending: Mutex<HashMap<RenderRequestId, PendingRequest>>,
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
        let (requests, receiver) = mpsc::sync_channel(RENDER_BROKER_QUEUE_CAPACITY);
        Self {
            inner: Arc::new(Inner {
                next_request_id: AtomicU64::new(1),
                closed: AtomicBool::new(false),
                requests,
                receiver: Mutex::new(receiver),
                pending: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn request(
        &self,
        kind: RenderBrokerRequestKind,
        timeout: Duration,
    ) -> Result<RenderBrokerResponse, RenderBrokerError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(closed_error());
        }
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
        let (response, receive_response) = mpsc::sync_channel(1);
        self.inner
            .pending
            .lock()
            .map_err(|_| internal_error("renderer pending map is unavailable"))?
            .insert(
                request_id,
                PendingRequest {
                    request: request.clone(),
                    response,
                },
            );
        if let Err(error) = self.inner.requests.try_send(request) {
            self.remove_pending(request_id)?;
            return Err(match error {
                mpsc::TrySendError::Full(_) => {
                    RenderBrokerError::new("render.queue-full", "renderer request queue is full")
                }
                mpsc::TrySendError::Disconnected(_) => closed_error(),
            });
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
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(closed_error()),
        }
    }

    pub fn poll_request(
        &self,
        timeout: Duration,
    ) -> Result<Option<RenderBrokerRequest>, RenderBrokerError> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.inner.closed.load(Ordering::Acquire) {
                return Err(closed_error());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let received = self
                .inner
                .receiver
                .lock()
                .map_err(|_| internal_error("renderer request receiver is unavailable"))?
                .recv_timeout(remaining);
            match received {
                Ok(request) => {
                    let pending = self
                        .inner
                        .pending
                        .lock()
                        .map_err(|_| internal_error("renderer pending map is unavailable"))?;
                    if pending.contains_key(&request.request_id) {
                        return Ok(Some(request));
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => return Ok(None),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(closed_error()),
            }
        }
    }

    pub fn respond(&self, response: RenderBrokerResponse) -> Result<(), RenderBrokerError> {
        let pending = self
            .inner
            .pending
            .lock()
            .map_err(|_| internal_error("renderer pending map is unavailable"))?
            .remove(&response.request_id)
            .ok_or_else(|| {
                RenderBrokerError::new(
                    "render.unknown-request",
                    "renderer response is stale, duplicate, or unknown",
                )
            })?;
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
        if self.inner.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let pending = std::mem::take(
            &mut *self
                .inner
                .pending
                .lock()
                .map_err(|_| internal_error("renderer pending map is unavailable"))?,
        );
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
        Ok(self
            .inner
            .pending
            .lock()
            .map_err(|_| internal_error("renderer pending map is unavailable"))?
            .remove(&request_id))
    }
}

fn closed_error() -> RenderBrokerError {
    RenderBrokerError::new("render.closed", "renderer broker is closed")
}

fn internal_error(message: &'static str) -> RenderBrokerError {
    RenderBrokerError::new("render.internal", message)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
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

    #[test]
    fn request_response_progresses_while_unrelated_worker_is_blocked() {
        let broker = RenderBroker::new();
        let worker_lock = Arc::new(Mutex::new(()));
        let held = worker_lock.lock().unwrap();
        let requester = broker.clone();
        let join = thread::spawn(move || {
            requester.request(
                RenderBrokerRequestKind::EnsureLayout {
                    required_revision: revision(),
                },
                Duration::from_secs(1),
            )
        });
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
        let join = thread::spawn(move || {
            requester.request(
                RenderBrokerRequestKind::EnsureLayout {
                    required_revision: revision(),
                },
                Duration::from_millis(20),
            )
        });
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
}
