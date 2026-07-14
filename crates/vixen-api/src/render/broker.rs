//! Dedicated renderer request/response DTOs.

use crate::RenderRequestId;

use super::{
    RenderCommit, RenderHitTestQuery, RenderInputTarget, RenderProtocolError, RenderRevision,
    RenderTextQueryBatch, RenderTextQueryBatchResult, render_error_codes, validate_version,
};

#[derive(Debug, Clone, PartialEq)]
pub enum RenderBrokerRequestKind {
    EnsureLayout { required_revision: RenderRevision },
    HitTest(RenderHitTestQuery),
    TextQuery(RenderTextQueryBatch),
}

#[derive(Debug, Clone, PartialEq)]
pub struct RenderBrokerRequest {
    pub version: u16,
    pub request_id: RenderRequestId,
    pub kind: RenderBrokerRequestKind,
}

impl RenderBrokerRequest {
    pub fn validate(&self) -> Result<(), RenderProtocolError> {
        validate_version(self.version)?;
        match &self.kind {
            RenderBrokerRequestKind::EnsureLayout { required_revision } => {
                required_revision.validate()
            }
            RenderBrokerRequestKind::HitTest(query) => validate_version(query.version),
            RenderBrokerRequestKind::TextQuery(query) => validate_version(query.version),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderBrokerCancellation {
    Navigation,
    Stop,
    ContextClosed,
    Shutdown,
    Deadline,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RenderBrokerResponseKind {
    Commit(RenderCommit),
    HitTest(Option<RenderInputTarget>),
    TextQuery(RenderTextQueryBatchResult),
    Cancelled(RenderBrokerCancellation),
    Failed { code: String, message: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct RenderBrokerResponse {
    pub version: u16,
    pub request_id: RenderRequestId,
    pub kind: RenderBrokerResponseKind,
}

impl RenderBrokerResponse {
    pub fn validate_for(&self, request: &RenderBrokerRequest) -> Result<(), RenderProtocolError> {
        validate_version(self.version)?;
        if self.request_id != request.request_id {
            return Err(RenderProtocolError::new(
                render_error_codes::STALE,
                "renderer broker response request id does not match",
            ));
        }
        let matching_kind = matches!(
            (&request.kind, &self.kind),
            (
                RenderBrokerRequestKind::EnsureLayout { .. },
                RenderBrokerResponseKind::Commit(_)
            ) | (
                RenderBrokerRequestKind::HitTest(_),
                RenderBrokerResponseKind::HitTest(_)
            ) | (
                RenderBrokerRequestKind::TextQuery(_),
                RenderBrokerResponseKind::TextQuery(_)
            ) | (_, RenderBrokerResponseKind::Cancelled(_))
                | (_, RenderBrokerResponseKind::Failed { .. })
        );
        if !matching_kind {
            return Err(RenderProtocolError::new(
                render_error_codes::INVALID_GRAPH,
                "renderer broker response kind does not match its request",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BrowsingContextId, DocumentId, RENDER_PROTOCOL_VERSION};

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
    fn responses_are_correlated_and_kind_checked() {
        let request = RenderBrokerRequest {
            version: RENDER_PROTOCOL_VERSION,
            request_id: RenderRequestId::new(7).unwrap(),
            kind: RenderBrokerRequestKind::EnsureLayout {
                required_revision: revision(),
            },
        };
        assert!(request.validate().is_ok());
        let wrong_id = RenderBrokerResponse {
            version: RENDER_PROTOCOL_VERSION,
            request_id: RenderRequestId::new(8).unwrap(),
            kind: RenderBrokerResponseKind::Cancelled(RenderBrokerCancellation::Deadline),
        };
        assert_eq!(
            wrong_id.validate_for(&request).unwrap_err().code,
            render_error_codes::STALE
        );
        let wrong_kind = RenderBrokerResponse {
            version: RENDER_PROTOCOL_VERSION,
            request_id: request.request_id,
            kind: RenderBrokerResponseKind::HitTest(None),
        };
        assert_eq!(
            wrong_kind.validate_for(&request).unwrap_err().code,
            render_error_codes::INVALID_GRAPH
        );
    }
}
