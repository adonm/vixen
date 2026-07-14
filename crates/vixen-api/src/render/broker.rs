//! Dedicated renderer request/response DTOs.

use std::collections::{BTreeMap, BTreeSet};

use crate::RenderRequestId;

use super::{
    FullRenderSnapshot, RENDER_MAX_GEOMETRY_ENTRIES, RENDER_MAX_SCROLL_ENTRIES,
    RENDER_MAX_SEMANTIC_BOUNDS, RENDER_MAX_TEXT_BOXES, RENDER_MAX_TEXT_QUERIES,
    RENDER_MAX_TRUNCATION_DIAGNOSTICS, RenderCommit, RenderHandleRelease, RenderHitTestQuery,
    RenderInputTarget, RenderMutationBatch, RenderPresented, RenderProtocolError,
    RenderResyncRequest, RenderRevision, RenderTextQueryBatch, RenderTextQueryBatchResult,
    RenderTextQueryKind, RenderTextQueryValue, render_error_codes, validate_version,
};

/// Ordinary asynchronous BrowserCore-to-renderer traffic. Synchronous layout
/// and bounded queries remain [`RenderBrokerRequestKind`] requests.
#[derive(Debug, Clone, PartialEq)]
pub enum RenderBridgeUpdate {
    FullSnapshot(FullRenderSnapshot),
    MutationBatch(RenderMutationBatch),
    ReleaseHandles(RenderHandleRelease),
}

impl RenderBridgeUpdate {
    pub fn validate(&self) -> Result<(), RenderProtocolError> {
        match self {
            Self::FullSnapshot(snapshot) => snapshot.validate(),
            Self::MutationBatch(batch) => batch.validate(),
            Self::ReleaseHandles(release) => validate_version(release.version),
        }
    }
}

/// Ordinary asynchronous renderer-to-BrowserCore traffic. Consumers validate
/// these DTOs against source and commit state before changing browser truth.
#[derive(Debug, Clone, PartialEq)]
pub enum RenderBridgeSubmission {
    Commit(RenderCommit),
    Presented(RenderPresented),
    Resync(RenderResyncRequest),
}

impl RenderBridgeSubmission {
    pub fn validate_transport(&self) -> Result<(), RenderProtocolError> {
        match self {
            Self::Commit(commit) => validate_commit_for_revision(commit, commit.revision),
            Self::Presented(presented) => {
                validate_version(presented.version)?;
                presented.revision.validate()?;
                validate_revision_identity(
                    presented.context_id.get(),
                    presented.document_id.get(),
                    presented.revision,
                    "presentation",
                )
            }
            Self::Resync(request) => {
                validate_version(request.version)?;
                if let Some(revision) = request.current_revision {
                    revision.validate()?;
                    validate_revision_identity(
                        request.context_id.get(),
                        request.document_id.get(),
                        revision,
                        "resync current revision",
                    )?;
                }
                if let Some(revision) = request.rejected_base_revision {
                    revision.validate()?;
                    validate_revision_identity(
                        request.context_id.get(),
                        request.document_id.get(),
                        revision,
                        "resync rejected revision",
                    )?;
                }
                Ok(())
            }
        }
    }
}

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
            RenderBrokerRequestKind::HitTest(query) => {
                validate_version(query.version)?;
                query.revision.validate()?;
                validate_revision_identity(
                    query.context_id.get(),
                    query.document_id.get(),
                    query.revision,
                    "hit-test request",
                )?;
                query.point.validate("hit-test request point")
            }
            RenderBrokerRequestKind::TextQuery(batch) => {
                validate_version(batch.version)?;
                batch.revision.validate()?;
                validate_revision_identity(
                    batch.context_id.get(),
                    batch.document_id.get(),
                    batch.revision,
                    "text-query request",
                )?;
                if batch.queries.len() > RENDER_MAX_TEXT_QUERIES {
                    return Err(limit_error(
                        "text query",
                        batch.queries.len(),
                        RENDER_MAX_TEXT_QUERIES,
                    ));
                }
                let mut query_ids = BTreeSet::new();
                for query in &batch.queries {
                    if !query_ids.insert(query.query_id) {
                        return Err(RenderProtocolError::new(
                            render_error_codes::DUPLICATE_ID,
                            "text-query request repeats a query id",
                        ));
                    }
                    match query.kind {
                        RenderTextQueryKind::OffsetForPoint { point } => {
                            point.validate("text-query request point")?;
                        }
                        RenderTextQueryKind::RangeBoxes {
                            utf16_start,
                            utf16_end,
                        } if utf16_start > utf16_end => {
                            return Err(RenderProtocolError::new(
                                render_error_codes::INVALID_GEOMETRY,
                                "text-query request range is reversed",
                            ));
                        }
                        RenderTextQueryKind::CaretForOffset { .. }
                        | RenderTextQueryKind::RangeBoxes { .. } => {}
                    }
                }
                Ok(())
            }
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
        if let RenderBrokerResponseKind::Failed { code, message } = &self.kind
            && (code.len() > 256 || message.len() > 4_096)
        {
            return Err(RenderProtocolError::new(
                render_error_codes::LIMIT,
                "renderer broker failure text exceeds its transport limit",
            ));
        }
        match (&request.kind, &self.kind) {
            (
                RenderBrokerRequestKind::EnsureLayout { required_revision },
                RenderBrokerResponseKind::Commit(commit),
            ) => validate_commit_for_revision(commit, *required_revision),
            (
                RenderBrokerRequestKind::HitTest(query),
                RenderBrokerResponseKind::HitTest(Some(target)),
            ) => validate_hit_test_response(query, *target),
            (
                RenderBrokerRequestKind::TextQuery(batch),
                RenderBrokerResponseKind::TextQuery(result),
            ) => validate_text_query_response(batch, result),
            (_, RenderBrokerResponseKind::Cancelled(_))
            | (_, RenderBrokerResponseKind::Failed { .. })
            | (RenderBrokerRequestKind::HitTest(_), RenderBrokerResponseKind::HitTest(None)) => {
                Ok(())
            }
            _ => unreachable!("response kind was checked above"),
        }
    }
}

fn validate_commit_for_revision(
    commit: &RenderCommit,
    required_revision: RenderRevision,
) -> Result<(), RenderProtocolError> {
    validate_version(commit.version)?;
    if commit.revision != required_revision {
        return Err(stale_error(
            "renderer commit does not match the requested revision",
        ));
    }
    commit.viewport.validate()?;
    if commit.geometry_index.len() > RENDER_MAX_GEOMETRY_ENTRIES {
        return Err(limit_error(
            "geometry entry",
            commit.geometry_index.len(),
            RENDER_MAX_GEOMETRY_ENTRIES,
        ));
    }
    if commit.scroll_snapshot.len() > RENDER_MAX_SCROLL_ENTRIES {
        return Err(limit_error(
            "scroll entry",
            commit.scroll_snapshot.len(),
            RENDER_MAX_SCROLL_ENTRIES,
        ));
    }
    if commit.semantic_bounds.len() > RENDER_MAX_SEMANTIC_BOUNDS {
        return Err(limit_error(
            "semantic bounds",
            commit.semantic_bounds.len(),
            RENDER_MAX_SEMANTIC_BOUNDS,
        ));
    }
    if commit.truncations.len() > RENDER_MAX_TRUNCATION_DIAGNOSTICS {
        return Err(limit_error(
            "truncation diagnostic",
            commit.truncations.len(),
            RENDER_MAX_TRUNCATION_DIAGNOSTICS,
        ));
    }
    if commit
        .truncations
        .iter()
        .any(|truncation| truncation.required)
    {
        return Err(RenderProtocolError::new(
            render_error_codes::TRUNCATED_REQUIRED,
            "renderer commit omitted required output",
        ));
    }
    Ok(())
}

fn validate_hit_test_response(
    query: &RenderHitTestQuery,
    target: RenderInputTarget,
) -> Result<(), RenderProtocolError> {
    validate_version(target.version)?;
    if target.query_id != query.query_id
        || target.context_id != query.context_id
        || target.document_id != query.document_id
        || target.displayed_commit_id != query.displayed_commit_id
        || target.revision != query.revision
        || target.handle != query.handle
        || target.viewport_point != query.point
    {
        return Err(stale_error(
            "renderer hit-test response does not match its request",
        ));
    }
    target.viewport_point.validate("hit-test response point")?;
    target.local_point.validate("hit-test response local point")
}

fn validate_text_query_response(
    request: &RenderTextQueryBatch,
    response: &RenderTextQueryBatchResult,
) -> Result<(), RenderProtocolError> {
    validate_version(response.version)?;
    if response.context_id != request.context_id
        || response.document_id != request.document_id
        || response.commit_id != request.commit_id
        || response.revision != request.revision
    {
        return Err(stale_error(
            "renderer text-query response does not match its request",
        ));
    }
    if response.truncations.len() > RENDER_MAX_TRUNCATION_DIAGNOSTICS {
        return Err(limit_error(
            "truncation diagnostic",
            response.truncations.len(),
            RENDER_MAX_TRUNCATION_DIAGNOSTICS,
        ));
    }
    if !request.allow_truncation
        && response
            .truncations
            .iter()
            .any(|truncation| truncation.required || truncation.omitted > 0)
    {
        return Err(RenderProtocolError::new(
            render_error_codes::TRUNCATED_REQUIRED,
            "renderer text-query response was truncated",
        ));
    }
    if response.results.len() != request.queries.len() {
        return Err(RenderProtocolError::new(
            render_error_codes::INVALID_GRAPH,
            "renderer text-query result count does not match its request",
        ));
    }
    let expected = request
        .queries
        .iter()
        .map(|query| (query.query_id, query.kind))
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    let mut text_boxes = 0usize;
    for result in &response.results {
        if !seen.insert(result.query_id) {
            return Err(RenderProtocolError::new(
                render_error_codes::DUPLICATE_ID,
                "renderer text-query response repeats a query id",
            ));
        }
        let Some(request_kind) = expected.get(&result.query_id) else {
            return Err(stale_error(
                "renderer text-query response contains an unknown query id",
            ));
        };
        match (request_kind, &result.value) {
            (RenderTextQueryKind::OffsetForPoint { .. }, RenderTextQueryValue::Offset { .. }) => {}
            (
                RenderTextQueryKind::CaretForOffset { .. },
                RenderTextQueryValue::Caret { rect, .. },
            ) => rect.validate("text-query caret response")?,
            (RenderTextQueryKind::RangeBoxes { .. }, RenderTextQueryValue::RangeBoxes(boxes)) => {
                text_boxes = text_boxes
                    .checked_add(boxes.len())
                    .ok_or_else(|| limit_error("text box", usize::MAX, RENDER_MAX_TEXT_BOXES))?;
                if text_boxes > RENDER_MAX_TEXT_BOXES {
                    return Err(limit_error("text box", text_boxes, RENDER_MAX_TEXT_BOXES));
                }
                for text_box in boxes {
                    text_box.rect.validate("text-query range response")?;
                }
            }
            _ => {
                return Err(RenderProtocolError::new(
                    render_error_codes::INVALID_GRAPH,
                    "renderer text-query response value kind does not match its request",
                ));
            }
        }
    }
    Ok(())
}

fn validate_revision_identity(
    context_id: u64,
    document_id: u64,
    revision: RenderRevision,
    description: &str,
) -> Result<(), RenderProtocolError> {
    if revision.context_id.get() != context_id || revision.document_id.get() != document_id {
        return Err(stale_error(format!(
            "{description} identity does not match its revision"
        )));
    }
    Ok(())
}

fn stale_error(message: impl Into<String>) -> RenderProtocolError {
    RenderProtocolError::new(render_error_codes::STALE, message)
}

fn limit_error(description: &str, actual: usize, limit: usize) -> RenderProtocolError {
    RenderProtocolError::new(
        render_error_codes::LIMIT,
        format!("{description} count {actual} exceeds limit {limit}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BrowsingContextId, DocumentId, RENDER_PROTOCOL_VERSION, RenderCommitId, RenderFragmentId,
        RenderHitTestHandle, RenderNodeId, RenderPoint, RenderQueryId, RenderTextQueryHandle,
        RenderViewport,
    };

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

        let wrong_revision = RenderBrokerResponse {
            version: RENDER_PROTOCOL_VERSION,
            request_id: request.request_id,
            kind: RenderBrokerResponseKind::Commit(RenderCommit {
                version: RENDER_PROTOCOL_VERSION,
                commit_id: RenderCommitId::new(1).unwrap(),
                revision: RenderRevision {
                    source_generation: 4,
                    ..revision()
                },
                viewport: RenderViewport {
                    width: 100,
                    height: 100,
                    device_scale: 1.0,
                    page_zoom: 1.0,
                },
                geometry_index: Vec::new(),
                hit_test_handle: RenderHitTestHandle::new(2).unwrap(),
                text_query_handle: RenderTextQueryHandle::new(3).unwrap(),
                scroll_snapshot: Vec::new(),
                semantic_bounds: Vec::new(),
                truncations: Vec::new(),
            }),
        };
        assert_eq!(
            wrong_revision.validate_for(&request).unwrap_err().code,
            render_error_codes::STALE
        );
    }

    #[test]
    fn hit_test_response_must_echo_every_request_identity() {
        let query = RenderHitTestQuery {
            version: RENDER_PROTOCOL_VERSION,
            query_id: RenderQueryId::new(1).unwrap(),
            context_id: revision().context_id,
            document_id: revision().document_id,
            displayed_commit_id: RenderCommitId::new(2).unwrap(),
            revision: revision(),
            handle: RenderHitTestHandle::new(3).unwrap(),
            point: RenderPoint { x: 4.0, y: 5.0 },
        };
        let request = RenderBrokerRequest {
            version: RENDER_PROTOCOL_VERSION,
            request_id: RenderRequestId::new(6).unwrap(),
            kind: RenderBrokerRequestKind::HitTest(query),
        };
        let mut target = RenderInputTarget {
            version: RENDER_PROTOCOL_VERSION,
            query_id: query.query_id,
            context_id: query.context_id,
            document_id: query.document_id,
            displayed_commit_id: query.displayed_commit_id,
            revision: query.revision,
            handle: query.handle,
            node_id: RenderNodeId::new(7).unwrap(),
            fragment_id: RenderFragmentId::new(8).unwrap(),
            viewport_point: query.point,
            local_point: RenderPoint { x: 1.0, y: 2.0 },
        };
        let response = |target| RenderBrokerResponse {
            version: RENDER_PROTOCOL_VERSION,
            request_id: request.request_id,
            kind: RenderBrokerResponseKind::HitTest(Some(target)),
        };
        assert!(response(target).validate_for(&request).is_ok());
        target.handle = RenderHitTestHandle::new(9).unwrap();
        assert_eq!(
            response(target).validate_for(&request).unwrap_err().code,
            render_error_codes::STALE
        );
    }
}
