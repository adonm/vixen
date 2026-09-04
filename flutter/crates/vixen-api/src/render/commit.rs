//! Atomic renderer commits, presentation, queries, input, and semantic actions.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    BrowsingContextId, DocumentId, RenderCommitId, RenderFragmentId, RenderNodeId, RenderQueryId,
    RenderScrollCommandId, RenderScrollNodeId, SemanticActionRequestId, SemanticNodeId,
};

use super::{
    RENDER_MAX_GEOMETRY_ENTRIES, RENDER_MAX_SCROLL_ENTRIES, RENDER_MAX_SEEN_SCROLL_COMMANDS,
    RENDER_MAX_SEEN_SEMANTIC_ACTIONS, RENDER_MAX_SEMANTIC_BOUNDS, RENDER_MAX_SEMANTIC_VALUE_BYTES,
    RENDER_MAX_TEXT_BOXES, RENDER_MAX_TEXT_QUERIES, RENDER_MAX_TRUNCATION_DIAGNOSTICS,
    RENDER_PROTOCOL_VERSION, RenderHitTestHandle, RenderPoint, RenderProtocolError, RenderRect,
    RenderReplica, RenderRevision, RenderSemanticActionKind, RenderSize, RenderTextQueryHandle,
    RenderTruncationDiagnostic, RenderViewport, render_error_codes, validate_version,
};

/// Immutable basic geometry for one formatter-produced fragment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderGeometryEntry {
    pub node_id: RenderNodeId,
    pub fragment_id: RenderFragmentId,
    pub border_box: RenderRect,
    pub padding_box: RenderRect,
    pub content_box: RenderRect,
    pub clip: Option<RenderRect>,
    pub scroll_node_id: Option<RenderScrollNodeId>,
    pub paint_order: u32,
}

impl RenderGeometryEntry {
    fn validate(self) -> Result<(), RenderProtocolError> {
        self.border_box.validate("geometry border box")?;
        self.padding_box.validate("geometry padding box")?;
        self.content_box.validate("geometry content box")?;
        if let Some(clip) = self.clip {
            clip.validate("geometry clip")?;
        }
        Ok(())
    }
}

/// Mechanical scroll state produced by the formatter for one commit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderScrollState {
    pub scroll_node_id: RenderScrollNodeId,
    pub node_id: RenderNodeId,
    pub offset: RenderPoint,
    pub max_offset: RenderPoint,
    pub viewport: RenderRect,
    pub content_size: RenderSize,
}

impl RenderScrollState {
    fn validate(self) -> Result<(), RenderProtocolError> {
        self.offset.validate("scroll offset")?;
        self.max_offset.validate("scroll maximum offset")?;
        self.viewport.validate("scroll viewport")?;
        self.content_size.validate("scroll content size")?;
        if self.offset.x < 0.0
            || self.offset.y < 0.0
            || self.max_offset.x < 0.0
            || self.max_offset.y < 0.0
            || self.offset.x > self.max_offset.x
            || self.offset.y > self.max_offset.y
        {
            return Err(RenderProtocolError::new(
                render_error_codes::INVALID_GEOMETRY,
                "scroll offset must be within its non-negative maximum",
            ));
        }
        Ok(())
    }
}

/// Formatter-computed bounds for BrowserCore-authored semantic meaning.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderSemanticBounds {
    pub semantic_node_id: SemanticNodeId,
    pub node_id: RenderNodeId,
    pub rects: Vec<RenderRect>,
}

/// One scene-ready atomic renderer commit. Presentation is acknowledged
/// separately by [`RenderPresented`].
#[derive(Debug, Clone, PartialEq)]
pub struct RenderCommit {
    pub version: u16,
    /// Strictly increasing within one context/document, including same-revision
    /// mechanical scroll commits.
    pub commit_id: RenderCommitId,
    pub revision: RenderRevision,
    pub viewport: RenderViewport,
    pub geometry_index: Vec<RenderGeometryEntry>,
    pub hit_test_handle: RenderHitTestHandle,
    pub text_query_handle: RenderTextQueryHandle,
    pub scroll_snapshot: Vec<RenderScrollState>,
    pub semantic_bounds: Vec<RenderSemanticBounds>,
    pub truncations: Vec<RenderTruncationDiagnostic>,
}

impl RenderCommit {
    pub fn validate(&self, source: &RenderReplica) -> Result<(), RenderProtocolError> {
        validate_version(self.version)?;
        self.revision.validate()?;
        if source.revision() != Some(self.revision) {
            return Err(RenderProtocolError::new(
                render_error_codes::STALE,
                "render commit does not match the current source revision",
            ));
        }
        self.viewport.validate()?;
        if source.viewport() != Some(self.viewport) {
            return Err(RenderProtocolError::new(
                render_error_codes::STALE,
                "render commit viewport does not match BrowserCore's source viewport",
            ));
        }
        if self.geometry_index.len() > RENDER_MAX_GEOMETRY_ENTRIES {
            return Err(limit_error(
                "geometry entry",
                self.geometry_index.len(),
                RENDER_MAX_GEOMETRY_ENTRIES,
            ));
        }
        if self.scroll_snapshot.len() > RENDER_MAX_SCROLL_ENTRIES {
            return Err(limit_error(
                "scroll entry",
                self.scroll_snapshot.len(),
                RENDER_MAX_SCROLL_ENTRIES,
            ));
        }
        if self.semantic_bounds.len() > RENDER_MAX_SEMANTIC_BOUNDS {
            return Err(limit_error(
                "semantic bounds",
                self.semantic_bounds.len(),
                RENDER_MAX_SEMANTIC_BOUNDS,
            ));
        }
        validate_truncations(&self.truncations, false)?;

        let mut scroll_ids = BTreeSet::new();
        for scroll in &self.scroll_snapshot {
            scroll.validate()?;
            if !source.contains_node(scroll.node_id) {
                return Err(unknown_id_error("scroll render node", scroll.node_id));
            }
            if !scroll_ids.insert(scroll.scroll_node_id) {
                return Err(duplicate_id_error(
                    "render scroll node",
                    scroll.scroll_node_id,
                ));
            }
        }

        let mut fragment_ids = BTreeSet::new();
        for geometry in &self.geometry_index {
            geometry.validate()?;
            if !source.contains_node(geometry.node_id) {
                return Err(unknown_id_error("geometry render node", geometry.node_id));
            }
            if !fragment_ids.insert(geometry.fragment_id) {
                return Err(duplicate_id_error("render fragment", geometry.fragment_id));
            }
            if let Some(scroll_node_id) = geometry.scroll_node_id
                && !scroll_ids.contains(&scroll_node_id)
            {
                return Err(unknown_id_error("geometry scroll node", scroll_node_id));
            }
        }

        let mut semantic_ids = BTreeSet::new();
        let mut semantic_rects = 0usize;
        for bounds in &self.semantic_bounds {
            if !semantic_ids.insert(bounds.semantic_node_id) {
                return Err(duplicate_id_error(
                    "semantic bounds",
                    bounds.semantic_node_id,
                ));
            }
            if !source.node_has_semantic_node(bounds.node_id, bounds.semantic_node_id) {
                return Err(unknown_id_error(
                    "semantic bounds node",
                    bounds.semantic_node_id,
                ));
            }
            semantic_rects = semantic_rects
                .checked_add(bounds.rects.len())
                .ok_or_else(|| {
                    limit_error(
                        "semantic rectangle",
                        usize::MAX,
                        RENDER_MAX_GEOMETRY_ENTRIES,
                    )
                })?;
            if semantic_rects > RENDER_MAX_GEOMETRY_ENTRIES {
                return Err(limit_error(
                    "semantic rectangle",
                    semantic_rects,
                    RENDER_MAX_GEOMETRY_ENTRIES,
                ));
            }
            for rect in &bounds.rects {
                rect.validate("semantic bounds")?;
            }
        }
        Ok(())
    }
}

/// Acknowledgement that an accepted commit is now actually visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderPresented {
    pub version: u16,
    pub context_id: BrowsingContextId,
    pub document_id: DocumentId,
    pub commit_id: RenderCommitId,
    pub revision: RenderRevision,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderHitTestQuery {
    pub version: u16,
    pub query_id: RenderQueryId,
    pub context_id: BrowsingContextId,
    pub document_id: DocumentId,
    pub displayed_commit_id: RenderCommitId,
    pub revision: RenderRevision,
    pub handle: RenderHitTestHandle,
    pub point: RenderPoint,
}

/// Commit-bound target returned by Flutter hit testing and supplied to input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderInputTarget {
    pub version: u16,
    pub query_id: RenderQueryId,
    pub context_id: BrowsingContextId,
    pub document_id: DocumentId,
    pub displayed_commit_id: RenderCommitId,
    pub revision: RenderRevision,
    pub handle: RenderHitTestHandle,
    pub node_id: RenderNodeId,
    pub fragment_id: RenderFragmentId,
    pub viewport_point: RenderPoint,
    pub local_point: RenderPoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderTextAffinity {
    Upstream,
    Downstream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderTextDirection {
    LeftToRight,
    RightToLeft,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RenderTextQueryKind {
    OffsetForPoint {
        point: RenderPoint,
    },
    CaretForOffset {
        utf16_offset: u32,
        affinity: RenderTextAffinity,
    },
    RangeBoxes {
        utf16_start: u32,
        utf16_end: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderTextQuery {
    pub query_id: RenderQueryId,
    pub node_id: RenderNodeId,
    pub kind: RenderTextQueryKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RenderTextQueryBatch {
    pub version: u16,
    pub context_id: BrowsingContextId,
    pub document_id: DocumentId,
    pub commit_id: RenderCommitId,
    pub revision: RenderRevision,
    pub handle: RenderTextQueryHandle,
    /// BrowserCore-owned policy for callers that can consume partial output.
    pub allow_truncation: bool,
    pub queries: Vec<RenderTextQuery>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderTextBox {
    pub rect: RenderRect,
    pub direction: RenderTextDirection,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RenderTextQueryValue {
    Offset {
        utf16_offset: u32,
        affinity: RenderTextAffinity,
    },
    Caret {
        rect: RenderRect,
        affinity: RenderTextAffinity,
    },
    RangeBoxes(Vec<RenderTextBox>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct RenderTextQueryResult {
    pub query_id: RenderQueryId,
    pub value: RenderTextQueryValue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RenderTextQueryBatchResult {
    pub version: u16,
    pub context_id: BrowsingContextId,
    pub document_id: DocumentId,
    pub commit_id: RenderCommitId,
    pub revision: RenderRevision,
    pub results: Vec<RenderTextQueryResult>,
    pub truncations: Vec<RenderTruncationDiagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RenderScrollCommandKind {
    By(RenderPoint),
    To(RenderPoint),
    Restore(RenderPoint),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderScrollCommand {
    pub version: u16,
    pub command_id: RenderScrollCommandId,
    pub context_id: BrowsingContextId,
    pub document_id: DocumentId,
    pub displayed_commit_id: RenderCommitId,
    pub revision: RenderRevision,
    pub scroll_node_id: RenderScrollNodeId,
    pub kind: RenderScrollCommandKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderSemanticAction {
    Activate,
    Focus,
    SetValue(String),
    SetSelection {
        base_offset: u32,
        extent_offset: u32,
    },
    Increase,
    Decrease,
    ScrollIntoView,
}

impl RenderSemanticAction {
    pub const fn kind(&self) -> RenderSemanticActionKind {
        match self {
            Self::Activate => RenderSemanticActionKind::Activate,
            Self::Focus => RenderSemanticActionKind::Focus,
            Self::SetValue(_) => RenderSemanticActionKind::SetValue,
            Self::SetSelection { .. } => RenderSemanticActionKind::SetSelection,
            Self::Increase => RenderSemanticActionKind::Increase,
            Self::Decrease => RenderSemanticActionKind::Decrease,
            Self::ScrollIntoView => RenderSemanticActionKind::ScrollIntoView,
        }
    }

    fn validate(&self) -> Result<(), RenderProtocolError> {
        if let Self::SetValue(value) = self
            && value.len() > RENDER_MAX_SEMANTIC_VALUE_BYTES
        {
            return Err(limit_error(
                "semantic action value byte",
                value.len(),
                RENDER_MAX_SEMANTIC_VALUE_BYTES,
            ));
        }
        Ok(())
    }
}

/// Native Semantics action bound to the exact displayed commit and advertised
/// BrowserCore action generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderSemanticActionTarget {
    pub version: u16,
    pub request_id: SemanticActionRequestId,
    pub context_id: BrowsingContextId,
    pub document_id: DocumentId,
    pub displayed_commit_id: RenderCommitId,
    pub revision: RenderRevision,
    pub semantic_node_id: SemanticNodeId,
    pub action_generation: u64,
    pub action: RenderSemanticAction,
}

/// Opaque Flutter commit handles that the bridge must release exactly once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderHandleRelease {
    pub version: u16,
    pub commit_id: RenderCommitId,
    pub hit_test_handle: RenderHitTestHandle,
    pub text_query_handle: RenderTextQueryHandle,
}

/// BrowserCore-side acceptance state for completed and displayed commits.
///
/// Contains no scene/browser truth; only exact identity and bounded replay state.
#[derive(Debug, Clone, Default)]
pub struct RenderCommitState {
    accepted: Option<RenderCommit>,
    presented: Option<RenderCommit>,
    seen_scroll_commands: BTreeSet<RenderScrollCommandId>,
    seen_semantic_actions: BTreeSet<SemanticActionRequestId>,
}

impl RenderCommitState {
    pub fn accepted_commit_id(&self) -> Option<RenderCommitId> {
        self.accepted.as_ref().map(|commit| commit.commit_id)
    }

    pub fn presented_commit_id(&self) -> Option<RenderCommitId> {
        self.presented.as_ref().map(|commit| commit.commit_id)
    }

    pub fn accepted_commit(&self) -> Option<&RenderCommit> {
        self.accepted.as_ref()
    }

    pub fn presented_commit(&self) -> Option<&RenderCommit> {
        self.presented.as_ref()
    }

    pub fn accept_commit(
        &mut self,
        source: &RenderReplica,
        commit: RenderCommit,
    ) -> Result<Vec<RenderHandleRelease>, RenderProtocolError> {
        commit.validate(source)?;
        let replacing_document = self.accepted.as_ref().is_some_and(|accepted| {
            accepted.revision.context_id != commit.revision.context_id
                || accepted.revision.document_id != commit.revision.document_id
        });
        if !replacing_document
            && self
                .accepted
                .as_ref()
                .is_some_and(|accepted| commit.commit_id.get() <= accepted.commit_id.get())
        {
            return Err(RenderProtocolError::new(
                render_error_codes::STALE,
                format!(
                    "render commit {} is replayed or superseded",
                    commit.commit_id
                ),
            ));
        }

        let mut releases = Vec::new();
        if replacing_document {
            push_unique_release(&mut releases, self.accepted.as_ref());
            push_unique_release(&mut releases, self.presented.as_ref());
            self.presented = None;
            self.seen_scroll_commands.clear();
            self.seen_semantic_actions.clear();
        } else if self.accepted.as_ref().is_some_and(|accepted| {
            self.presented
                .as_ref()
                .is_none_or(|presented| presented.commit_id != accepted.commit_id)
        }) {
            push_unique_release(&mut releases, self.accepted.as_ref());
        }
        self.accepted = Some(commit);
        Ok(releases)
    }

    pub fn accept_presented(
        &mut self,
        source: &RenderReplica,
        presented: RenderPresented,
    ) -> Result<Vec<RenderHandleRelease>, RenderProtocolError> {
        validate_version(presented.version)?;
        let accepted = self.accepted.as_ref().ok_or_else(|| {
            RenderProtocolError::new(
                render_error_codes::STALE,
                "presentation arrived before an accepted render commit",
            )
        })?;
        validate_identity(
            accepted,
            presented.context_id,
            presented.document_id,
            presented.commit_id,
            presented.revision,
            "presented acknowledgement",
        )?;
        validate_current_source(source, accepted, "presented acknowledgement")?;
        if self
            .presented
            .as_ref()
            .is_some_and(|commit| commit.commit_id == accepted.commit_id)
        {
            return Ok(Vec::new());
        }
        let mut releases = Vec::new();
        push_unique_release(&mut releases, self.presented.as_ref());
        self.presented = Some(accepted.clone());
        self.seen_scroll_commands.clear();
        self.seen_semantic_actions.clear();
        Ok(releases)
    }

    pub fn validate_hit_test_query(
        &self,
        source: &RenderReplica,
        query: RenderHitTestQuery,
    ) -> Result<(), RenderProtocolError> {
        validate_version(query.version)?;
        let commit = self.require_presented()?;
        validate_identity(
            commit,
            query.context_id,
            query.document_id,
            query.displayed_commit_id,
            query.revision,
            "hit-test query",
        )?;
        validate_current_source(source, commit, "hit-test query")?;
        if query.handle != commit.hit_test_handle {
            return Err(RenderProtocolError::new(
                render_error_codes::STALE,
                "hit-test query used the wrong opaque handle",
            ));
        }
        query.point.validate("hit-test point")
    }

    pub fn validate_input_target(
        &self,
        source: &RenderReplica,
        query: &RenderHitTestQuery,
        target: RenderInputTarget,
    ) -> Result<(), RenderProtocolError> {
        self.validate_hit_test_query(source, *query)?;
        validate_version(target.version)?;
        let commit = self.require_presented()?;
        validate_identity(
            commit,
            target.context_id,
            target.document_id,
            target.displayed_commit_id,
            target.revision,
            "input target",
        )?;
        validate_current_source(source, commit, "input target")?;
        if target.query_id != query.query_id || target.viewport_point != query.point {
            return Err(RenderProtocolError::new(
                render_error_codes::STALE,
                "input target does not match its hit-test query",
            ));
        }
        if target.handle != commit.hit_test_handle {
            return Err(RenderProtocolError::new(
                render_error_codes::STALE,
                "input target used the wrong opaque hit-test handle",
            ));
        }
        target.viewport_point.validate("input viewport point")?;
        target.local_point.validate("input local point")?;
        if !commit.geometry_index.iter().any(|geometry| {
            geometry.node_id == target.node_id && geometry.fragment_id == target.fragment_id
        }) {
            return Err(unknown_id_error("input node/fragment", target.fragment_id));
        }
        Ok(())
    }

    pub fn validate_text_query(
        &self,
        source: &RenderReplica,
        batch: &RenderTextQueryBatch,
    ) -> Result<(), RenderProtocolError> {
        validate_version(batch.version)?;
        let commit = self.require_accepted()?;
        validate_identity(
            commit,
            batch.context_id,
            batch.document_id,
            batch.commit_id,
            batch.revision,
            "text query batch",
        )?;
        validate_current_source(source, commit, "text query batch")?;
        if batch.handle != commit.text_query_handle {
            return Err(RenderProtocolError::new(
                render_error_codes::STALE,
                "text query used the wrong opaque handle",
            ));
        }
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
                return Err(duplicate_id_error("text query", query.query_id));
            }
            let text_len = source
                .node_text_utf16_len(query.node_id)
                .ok_or_else(|| unknown_id_error("text query text node", query.node_id))?;
            match query.kind {
                RenderTextQueryKind::OffsetForPoint { point } => {
                    point.validate("text query point")?;
                }
                RenderTextQueryKind::CaretForOffset { utf16_offset, .. }
                    if utf16_offset as usize > text_len =>
                {
                    return Err(RenderProtocolError::new(
                        render_error_codes::INVALID_GEOMETRY,
                        "text caret offset exceeds the source text",
                    ));
                }
                RenderTextQueryKind::CaretForOffset { .. } => {}
                RenderTextQueryKind::RangeBoxes {
                    utf16_start,
                    utf16_end,
                } if utf16_start > utf16_end || utf16_end as usize > text_len => {
                    return Err(RenderProtocolError::new(
                        render_error_codes::INVALID_GEOMETRY,
                        "text query range is reversed or exceeds the source text",
                    ));
                }
                RenderTextQueryKind::RangeBoxes { .. } => {}
            }
        }
        Ok(())
    }

    pub fn validate_text_query_result(
        &self,
        source: &RenderReplica,
        request: &RenderTextQueryBatch,
        response: &RenderTextQueryBatchResult,
    ) -> Result<(), RenderProtocolError> {
        validate_version(response.version)?;
        self.validate_text_query(source, request)?;
        self.validate_text_query_identity(request, response)?;
        validate_truncations(&response.truncations, request.allow_truncation)?;
        if response.results.len() != request.queries.len() {
            return Err(RenderProtocolError::new(
                render_error_codes::INVALID_GRAPH,
                "text query result count does not match the request",
            ));
        }
        let expected = request
            .queries
            .iter()
            .map(|query| (query.query_id, (query.node_id, query.kind)))
            .collect::<BTreeMap<_, _>>();
        let mut seen = BTreeSet::new();
        let mut text_boxes = 0usize;
        for result in &response.results {
            if !seen.insert(result.query_id) {
                return Err(duplicate_id_error("text query result", result.query_id));
            }
            let (node_id, request_kind) = expected
                .get(&result.query_id)
                .ok_or_else(|| unknown_id_error("text query result", result.query_id))?;
            match (request_kind, &result.value) {
                (
                    RenderTextQueryKind::OffsetForPoint { .. },
                    RenderTextQueryValue::Offset { utf16_offset, .. },
                ) if *utf16_offset as usize
                    <= source.node_text_utf16_len(*node_id).unwrap_or_default() => {}
                (
                    RenderTextQueryKind::OffsetForPoint { .. },
                    RenderTextQueryValue::Offset { .. },
                ) => {
                    return Err(RenderProtocolError::new(
                        render_error_codes::INVALID_GEOMETRY,
                        "text offset result exceeds the source text",
                    ));
                }
                (
                    RenderTextQueryKind::CaretForOffset { .. },
                    RenderTextQueryValue::Caret { rect, .. },
                ) => {
                    rect.validate("text caret rectangle")?;
                }
                (
                    RenderTextQueryKind::RangeBoxes { .. },
                    RenderTextQueryValue::RangeBoxes(boxes),
                ) => {
                    text_boxes = text_boxes.checked_add(boxes.len()).ok_or_else(|| {
                        limit_error("text box", usize::MAX, RENDER_MAX_TEXT_BOXES)
                    })?;
                    if text_boxes > RENDER_MAX_TEXT_BOXES {
                        return Err(limit_error("text box", text_boxes, RENDER_MAX_TEXT_BOXES));
                    }
                    for text_box in boxes {
                        text_box.rect.validate("text range box")?;
                    }
                }
                _ => {
                    return Err(RenderProtocolError::new(
                        render_error_codes::INVALID_GRAPH,
                        format!(
                            "text query result {} has the wrong value kind",
                            result.query_id
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn consume_scroll_command(
        &mut self,
        source: &RenderReplica,
        command: RenderScrollCommand,
    ) -> Result<(), RenderProtocolError> {
        validate_version(command.version)?;
        let commit = self.require_presented()?;
        validate_identity(
            commit,
            command.context_id,
            command.document_id,
            command.displayed_commit_id,
            command.revision,
            "scroll command",
        )?;
        validate_current_source(source, commit, "scroll command")?;
        if !commit
            .scroll_snapshot
            .iter()
            .any(|scroll| scroll.scroll_node_id == command.scroll_node_id)
        {
            return Err(unknown_id_error(
                "scroll command node",
                command.scroll_node_id,
            ));
        }
        match command.kind {
            RenderScrollCommandKind::By(delta) => delta.validate("scroll delta"),
            RenderScrollCommandKind::To(offset) | RenderScrollCommandKind::Restore(offset) => {
                offset.validate("scroll target")?;
                if offset.x < 0.0 || offset.y < 0.0 {
                    return Err(RenderProtocolError::new(
                        render_error_codes::INVALID_GEOMETRY,
                        "absolute scroll target must be non-negative",
                    ));
                }
                Ok(())
            }
        }?;
        if self.seen_scroll_commands.contains(&command.command_id) {
            return Err(RenderProtocolError::new(
                render_error_codes::REPLAYED_COMMAND,
                format!("scroll command {} was replayed", command.command_id),
            ));
        }
        if self.seen_scroll_commands.len() >= RENDER_MAX_SEEN_SCROLL_COMMANDS {
            return Err(limit_error(
                "scroll command replay ledger",
                self.seen_scroll_commands.len() + 1,
                RENDER_MAX_SEEN_SCROLL_COMMANDS,
            ));
        }
        self.seen_scroll_commands.insert(command.command_id);
        Ok(())
    }

    pub fn consume_semantic_action(
        &mut self,
        source: &RenderReplica,
        target: &RenderSemanticActionTarget,
    ) -> Result<(), RenderProtocolError> {
        validate_version(target.version)?;
        let commit = self.require_presented()?;
        validate_identity(
            commit,
            target.context_id,
            target.document_id,
            target.displayed_commit_id,
            target.revision,
            "semantic action",
        )?;
        validate_current_source(source, commit, "semantic action")?;
        target.action.validate()?;
        let semantic = source
            .semantic_node(target.semantic_node_id)
            .ok_or_else(|| unknown_id_error("semantic action node", target.semantic_node_id))?;
        if !commit
            .semantic_bounds
            .iter()
            .any(|bounds| bounds.semantic_node_id == target.semantic_node_id)
        {
            return Err(unknown_id_error(
                "displayed semantic node",
                target.semantic_node_id,
            ));
        }
        if target.action_generation != semantic.action_generation {
            return Err(RenderProtocolError::new(
                render_error_codes::STALE,
                "semantic action generation is stale or forged",
            ));
        }
        if !semantic.actions.contains(&target.action.kind()) {
            return Err(RenderProtocolError::new(
                render_error_codes::UNADVERTISED_ACTION,
                "semantic action was not advertised by BrowserCore",
            ));
        }
        if let RenderSemanticAction::SetSelection {
            base_offset,
            extent_offset,
        } = &target.action
        {
            let value_len = source
                .semantic_value_utf16_len(target.semantic_node_id)
                .unwrap_or_default();
            if *base_offset as usize > value_len || *extent_offset as usize > value_len {
                return Err(RenderProtocolError::new(
                    render_error_codes::INVALID_GEOMETRY,
                    "semantic selection exceeds the advertised value",
                ));
            }
        }
        if self.seen_semantic_actions.contains(&target.request_id) {
            return Err(RenderProtocolError::new(
                render_error_codes::REPLAYED_ACTION,
                format!("semantic action request {} was replayed", target.request_id),
            ));
        }
        if self.seen_semantic_actions.len() >= RENDER_MAX_SEEN_SEMANTIC_ACTIONS {
            return Err(limit_error(
                "semantic action replay ledger",
                self.seen_semantic_actions.len() + 1,
                RENDER_MAX_SEEN_SEMANTIC_ACTIONS,
            ));
        }
        self.seen_semantic_actions.insert(target.request_id);
        Ok(())
    }

    fn validate_text_query_identity(
        &self,
        request: &RenderTextQueryBatch,
        response: &RenderTextQueryBatchResult,
    ) -> Result<(), RenderProtocolError> {
        if response.context_id != request.context_id
            || response.document_id != request.document_id
            || response.commit_id != request.commit_id
            || response.revision != request.revision
        {
            return Err(RenderProtocolError::new(
                render_error_codes::STALE,
                "text query response identity does not match its request",
            ));
        }
        Ok(())
    }

    fn require_accepted(&self) -> Result<&RenderCommit, RenderProtocolError> {
        self.accepted.as_ref().ok_or_else(|| {
            RenderProtocolError::new(render_error_codes::STALE, "no render commit is accepted")
        })
    }

    fn require_presented(&self) -> Result<&RenderCommit, RenderProtocolError> {
        self.presented.as_ref().ok_or_else(|| {
            RenderProtocolError::new(render_error_codes::STALE, "no render commit is presented")
        })
    }
}

fn validate_identity(
    commit: &RenderCommit,
    context_id: BrowsingContextId,
    document_id: DocumentId,
    commit_id: RenderCommitId,
    revision: RenderRevision,
    exchange: &str,
) -> Result<(), RenderProtocolError> {
    if context_id != commit.revision.context_id
        || document_id != commit.revision.document_id
        || commit_id != commit.commit_id
        || revision != commit.revision
    {
        return Err(RenderProtocolError::new(
            render_error_codes::STALE,
            format!("{exchange} does not match the required render commit"),
        ));
    }
    Ok(())
}

fn validate_current_source(
    source: &RenderReplica,
    commit: &RenderCommit,
    exchange: &str,
) -> Result<(), RenderProtocolError> {
    if source.revision() != Some(commit.revision) {
        return Err(RenderProtocolError::new(
            render_error_codes::STALE,
            format!("{exchange} no longer matches BrowserCore's source revision"),
        ));
    }
    Ok(())
}

fn push_unique_release(releases: &mut Vec<RenderHandleRelease>, commit: Option<&RenderCommit>) {
    let Some(commit) = commit else {
        return;
    };
    if releases
        .iter()
        .any(|release| release.commit_id == commit.commit_id)
    {
        return;
    }
    releases.push(RenderHandleRelease {
        version: RENDER_PROTOCOL_VERSION,
        commit_id: commit.commit_id,
        hit_test_handle: commit.hit_test_handle,
        text_query_handle: commit.text_query_handle,
    });
}

fn validate_truncations(
    truncations: &[RenderTruncationDiagnostic],
    allow_truncation: bool,
) -> Result<(), RenderProtocolError> {
    if truncations.len() > RENDER_MAX_TRUNCATION_DIAGNOSTICS {
        return Err(limit_error(
            "truncation diagnostic",
            truncations.len(),
            RENDER_MAX_TRUNCATION_DIAGNOSTICS,
        ));
    }
    if let Some(truncation) = truncations
        .iter()
        .find(|truncation| truncation.required || !allow_truncation)
    {
        return Err(RenderProtocolError::new(
            render_error_codes::TRUNCATED_REQUIRED,
            format!(
                "disallowed {:?} truncation omitted {} entries at limit {}",
                truncation.domain, truncation.omitted, truncation.limit
            ),
        ));
    }
    Ok(())
}

fn limit_error(field: &str, observed: usize, limit: usize) -> RenderProtocolError {
    RenderProtocolError::new(
        render_error_codes::LIMIT,
        format!("{field} count/size {observed} exceeds limit {limit}"),
    )
}

fn duplicate_id_error(kind: &str, id: impl std::fmt::Display) -> RenderProtocolError {
    RenderProtocolError::new(
        render_error_codes::DUPLICATE_ID,
        format!("duplicate {kind} id {id}"),
    )
}

fn unknown_id_error(kind: &str, id: impl std::fmt::Display) -> RenderProtocolError {
    RenderProtocolError::new(
        render_error_codes::UNKNOWN_ID,
        format!("unknown {kind} id {id}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        FullRenderSnapshot, RenderLimitDomain, RenderMutationBatch, RenderNode, RenderNodeKind,
        RenderSemanticNode, RenderStyleProperty,
    };

    fn id<T: TryFrom<u64>>(raw: u64) -> T
    where
        T::Error: std::fmt::Debug,
    {
        T::try_from(raw).unwrap()
    }

    fn revision(generation: u64) -> RenderRevision {
        RenderRevision {
            context_id: id(1),
            document_id: id(2),
            source_generation: generation,
            style_generation: generation,
            viewport_generation: 1,
            resource_generation: generation,
        }
    }

    fn viewport() -> RenderViewport {
        RenderViewport {
            width: 800,
            height: 600,
            device_scale: 1.0,
            page_zoom: 1.0,
        }
    }

    fn source() -> RenderReplica {
        let mut source = RenderReplica::default();
        let mut snapshot = FullRenderSnapshot::new(revision(1), viewport());
        snapshot.nodes.push(RenderNode {
            id: id(1),
            parent_id: None,
            sibling_index: 0,
            depth: 0,
            kind: RenderNodeKind::Element {
                local_name: "button".to_owned(),
            },
            styles: vec![RenderStyleProperty {
                name: "display".to_owned(),
                value: "block".to_owned(),
            }],
            resource_ids: Vec::new(),
            semantic: Some(RenderSemanticNode {
                id: id(7),
                role: "button".to_owned(),
                name: "Run".to_owned(),
                value: Some("Run".to_owned()),
                action_generation: 5,
                actions: vec![
                    RenderSemanticActionKind::Activate,
                    RenderSemanticActionKind::SetValue,
                    RenderSemanticActionKind::SetSelection,
                ],
            }),
        });
        snapshot.nodes.push(RenderNode {
            id: id(2),
            parent_id: Some(id(1)),
            sibling_index: 0,
            depth: 1,
            kind: RenderNodeKind::Text {
                text: "Run".to_owned(),
            },
            styles: Vec::new(),
            resource_ids: Vec::new(),
            semantic: None,
        });
        source.accept_full_snapshot(snapshot).unwrap();
        source
    }

    fn rect() -> RenderRect {
        RenderRect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 40.0,
        }
    }

    fn commit() -> RenderCommit {
        RenderCommit {
            version: RENDER_PROTOCOL_VERSION,
            commit_id: id(11),
            revision: revision(1),
            viewport: viewport(),
            geometry_index: vec![RenderGeometryEntry {
                node_id: id(1),
                fragment_id: id(3),
                border_box: rect(),
                padding_box: rect(),
                content_box: rect(),
                clip: Some(rect()),
                scroll_node_id: Some(id(4)),
                paint_order: 1,
            }],
            hit_test_handle: RenderHitTestHandle::new(8).unwrap(),
            text_query_handle: RenderTextQueryHandle::new(9).unwrap(),
            scroll_snapshot: vec![RenderScrollState {
                scroll_node_id: id(4),
                node_id: id(1),
                offset: RenderPoint { x: 0.0, y: 2.0 },
                max_offset: RenderPoint { x: 0.0, y: 80.0 },
                viewport: rect(),
                content_size: RenderSize {
                    width: 100.0,
                    height: 120.0,
                },
            }],
            semantic_bounds: vec![RenderSemanticBounds {
                semantic_node_id: id(7),
                node_id: id(1),
                rects: vec![rect()],
            }],
            truncations: Vec::new(),
        }
    }

    fn presented() -> RenderPresented {
        RenderPresented {
            version: RENDER_PROTOCOL_VERSION,
            context_id: id(1),
            document_id: id(2),
            commit_id: id(11),
            revision: revision(1),
        }
    }

    fn state() -> (RenderReplica, RenderCommitState) {
        let source = source();
        let mut state = RenderCommitState::default();
        state.accept_commit(&source, commit()).unwrap();
        state.accept_presented(&source, presented()).unwrap();
        (source, state)
    }

    #[test]
    fn commits_reject_stale_unknown_non_finite_and_required_truncation() {
        let source = source();

        assert_eq!(RenderHitTestHandle::new(0), None);
        assert_eq!(RenderTextQueryHandle::new(0), None);

        let mut stale = commit();
        stale.revision = revision(2);
        assert_eq!(
            stale.validate(&source).unwrap_err().code,
            render_error_codes::STALE
        );

        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut non_finite = commit();
            non_finite.geometry_index[0].border_box.x = invalid;
            assert_eq!(
                non_finite.validate(&source).unwrap_err().code,
                render_error_codes::NON_FINITE
            );
        }

        let mut unknown = commit();
        unknown.geometry_index[0].node_id = id(99);
        assert_eq!(
            unknown.validate(&source).unwrap_err().code,
            render_error_codes::UNKNOWN_ID
        );

        let mut truncated = commit();
        truncated.truncations.push(RenderTruncationDiagnostic {
            domain: RenderLimitDomain::Geometry,
            limit: 10,
            omitted: 1,
            required: true,
        });
        assert_eq!(
            truncated.validate(&source).unwrap_err().code,
            render_error_codes::TRUNCATED_REQUIRED
        );
        truncated.truncations[0].required = false;
        assert_eq!(
            truncated.validate(&source).unwrap_err().code,
            render_error_codes::TRUNCATED_REQUIRED
        );

        let mut wrong_viewport = commit();
        wrong_viewport.viewport.width = 801;
        assert_eq!(
            wrong_viewport.validate(&source).unwrap_err().code,
            render_error_codes::STALE
        );
    }

    #[test]
    fn presentation_is_separate_and_rejects_an_old_commit() {
        let source = source();
        let mut state = RenderCommitState::default();
        state.accept_commit(&source, commit()).unwrap();
        assert_eq!(state.presented_commit_id(), None);

        let mut stale = presented();
        stale.commit_id = id(10);
        assert_eq!(
            state.accept_presented(&source, stale).unwrap_err().code,
            render_error_codes::STALE
        );
        state.accept_presented(&source, presented()).unwrap();
        assert_eq!(state.presented_commit_id(), Some(id(11)));
    }

    #[test]
    fn superseded_commits_are_inert_and_retired_handles_are_explicit() {
        let (source, mut state) = state();
        let mut replacement = commit();
        replacement.commit_id = id(12);
        replacement.hit_test_handle = RenderHitTestHandle::new(18).unwrap();
        replacement.text_query_handle = RenderTextQueryHandle::new(19).unwrap();
        assert!(
            state
                .accept_commit(&source, replacement)
                .unwrap()
                .is_empty()
        );

        let releases = state
            .accept_presented(
                &source,
                RenderPresented {
                    commit_id: id(12),
                    ..presented()
                },
            )
            .unwrap();
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].commit_id, id(11));
        assert_eq!(releases[0].hit_test_handle.get(), 8);
        assert_eq!(state.presented_commit().unwrap().commit_id, id(12));

        assert_eq!(
            state.accept_commit(&source, commit()).unwrap_err().code,
            render_error_codes::STALE
        );
        let mut unseen_late = commit();
        unseen_late.commit_id = id(10);
        assert_eq!(
            state.accept_commit(&source, unseen_late).unwrap_err().code,
            render_error_codes::STALE
        );
    }

    #[test]
    fn presentation_and_queries_reject_an_advanced_source_revision() {
        let mut source = source();
        let mut state = RenderCommitState::default();
        state.accept_commit(&source, commit()).unwrap();
        source
            .apply_batch(RenderMutationBatch {
                version: RENDER_PROTOCOL_VERSION,
                base_revision: revision(1),
                target_revision: revision(2),
                mutations: Vec::new(),
            })
            .unwrap();
        assert_eq!(
            state
                .accept_presented(&source, presented())
                .unwrap_err()
                .code,
            render_error_codes::STALE
        );
    }

    #[test]
    fn hit_testing_and_input_require_the_displayed_commit_and_exact_fragment() {
        let (source, state) = state();
        let query = RenderHitTestQuery {
            version: RENDER_PROTOCOL_VERSION,
            query_id: id(20),
            context_id: id(1),
            document_id: id(2),
            displayed_commit_id: id(11),
            revision: revision(1),
            handle: RenderHitTestHandle::new(8).unwrap(),
            point: RenderPoint { x: 4.0, y: 5.0 },
        };
        state.validate_hit_test_query(&source, query).unwrap();
        let mut stale_query = query;
        stale_query.displayed_commit_id = id(10);
        assert_eq!(
            state
                .validate_hit_test_query(&source, stale_query)
                .unwrap_err()
                .code,
            render_error_codes::STALE
        );

        let target = RenderInputTarget {
            version: RENDER_PROTOCOL_VERSION,
            query_id: id(20),
            context_id: id(1),
            document_id: id(2),
            displayed_commit_id: id(11),
            revision: revision(1),
            handle: RenderHitTestHandle::new(8).unwrap(),
            node_id: id(1),
            fragment_id: id(3),
            viewport_point: RenderPoint { x: 4.0, y: 5.0 },
            local_point: RenderPoint { x: 4.0, y: 5.0 },
        };
        state
            .validate_input_target(&source, &query, target)
            .unwrap();
        let mut forged = target;
        forged.fragment_id = id(99);
        assert_eq!(
            state
                .validate_input_target(&source, &query, forged)
                .unwrap_err()
                .code,
            render_error_codes::UNKNOWN_ID
        );
        let mut wrong_query = target;
        wrong_query.query_id = id(21);
        assert_eq!(
            state
                .validate_input_target(&source, &query, wrong_query)
                .unwrap_err()
                .code,
            render_error_codes::STALE
        );
    }

    #[test]
    fn text_query_batches_and_results_are_exact_and_bounded() {
        let (source, state) = state();
        let request = RenderTextQueryBatch {
            version: RENDER_PROTOCOL_VERSION,
            context_id: id(1),
            document_id: id(2),
            commit_id: id(11),
            revision: revision(1),
            handle: RenderTextQueryHandle::new(9).unwrap(),
            allow_truncation: false,
            queries: vec![
                RenderTextQuery {
                    query_id: id(1),
                    node_id: id(2),
                    kind: RenderTextQueryKind::CaretForOffset {
                        utf16_offset: 2,
                        affinity: RenderTextAffinity::Downstream,
                    },
                },
                RenderTextQuery {
                    query_id: id(2),
                    node_id: id(2),
                    kind: RenderTextQueryKind::RangeBoxes {
                        utf16_start: 0,
                        utf16_end: 3,
                    },
                },
            ],
        };
        state.validate_text_query(&source, &request).unwrap();
        let response = RenderTextQueryBatchResult {
            version: RENDER_PROTOCOL_VERSION,
            context_id: id(1),
            document_id: id(2),
            commit_id: id(11),
            revision: revision(1),
            results: vec![
                RenderTextQueryResult {
                    query_id: id(1),
                    value: RenderTextQueryValue::Caret {
                        rect: rect(),
                        affinity: RenderTextAffinity::Downstream,
                    },
                },
                RenderTextQueryResult {
                    query_id: id(2),
                    value: RenderTextQueryValue::RangeBoxes(vec![RenderTextBox {
                        rect: rect(),
                        direction: RenderTextDirection::LeftToRight,
                    }]),
                },
            ],
            truncations: Vec::new(),
        };
        state
            .validate_text_query_result(&source, &request, &response)
            .unwrap();

        let mut out_of_bounds = request.clone();
        out_of_bounds.queries[0].kind = RenderTextQueryKind::CaretForOffset {
            utf16_offset: 4,
            affinity: RenderTextAffinity::Downstream,
        };
        assert_eq!(
            state
                .validate_text_query(&source, &out_of_bounds)
                .unwrap_err()
                .code,
            render_error_codes::INVALID_GEOMETRY
        );

        let mut truncated_response = response.clone();
        truncated_response
            .truncations
            .push(RenderTruncationDiagnostic {
                domain: RenderLimitDomain::TextBoxes,
                limit: 1,
                omitted: 1,
                required: false,
            });
        assert_eq!(
            state
                .validate_text_query_result(&source, &request, &truncated_response)
                .unwrap_err()
                .code,
            render_error_codes::TRUNCATED_REQUIRED
        );
        let mut partial_request = request.clone();
        partial_request.allow_truncation = true;
        state
            .validate_text_query_result(&source, &partial_request, &truncated_response)
            .unwrap();

        let mut wrong_kind = response;
        wrong_kind.results[0].value = RenderTextQueryValue::Offset {
            utf16_offset: 2,
            affinity: RenderTextAffinity::Downstream,
        };
        assert_eq!(
            state
                .validate_text_query_result(&source, &request, &wrong_kind)
                .unwrap_err()
                .code,
            render_error_codes::INVALID_GRAPH
        );
    }

    #[test]
    fn scroll_commands_name_a_presented_scroll_node() {
        let (source, mut state) = state();
        let command = RenderScrollCommand {
            version: RENDER_PROTOCOL_VERSION,
            command_id: id(1),
            context_id: id(1),
            document_id: id(2),
            displayed_commit_id: id(11),
            revision: revision(1),
            scroll_node_id: id(4),
            kind: RenderScrollCommandKind::By(RenderPoint { x: 0.0, y: -10.0 }),
        };
        state.consume_scroll_command(&source, command).unwrap();
        assert_eq!(
            state
                .consume_scroll_command(&source, command)
                .unwrap_err()
                .code,
            render_error_codes::REPLAYED_COMMAND
        );
        let mut unknown = command;
        unknown.command_id = id(2);
        unknown.scroll_node_id = id(99);
        assert_eq!(
            state
                .consume_scroll_command(&source, unknown)
                .unwrap_err()
                .code,
            render_error_codes::UNKNOWN_ID
        );
    }

    #[test]
    fn semantic_actions_reject_forgery_staleness_and_replay() {
        let (source, mut state) = state();
        let target = RenderSemanticActionTarget {
            version: RENDER_PROTOCOL_VERSION,
            request_id: id(1),
            context_id: id(1),
            document_id: id(2),
            displayed_commit_id: id(11),
            revision: revision(1),
            semantic_node_id: id(7),
            action_generation: 5,
            action: RenderSemanticAction::Activate,
        };
        state.consume_semantic_action(&source, &target).unwrap();
        state.accept_presented(&source, presented()).unwrap();
        assert_eq!(
            state
                .consume_semantic_action(&source, &target)
                .unwrap_err()
                .code,
            render_error_codes::REPLAYED_ACTION
        );

        let mut stale = target.clone();
        stale.request_id = id(2);
        stale.action_generation = 4;
        assert_eq!(
            state
                .consume_semantic_action(&source, &stale)
                .unwrap_err()
                .code,
            render_error_codes::STALE
        );

        let mut forged = target.clone();
        forged.request_id = id(3);
        forged.semantic_node_id = id(99);
        assert_eq!(
            state
                .consume_semantic_action(&source, &forged)
                .unwrap_err()
                .code,
            render_error_codes::UNKNOWN_ID
        );

        let mut unadvertised = target;
        unadvertised.request_id = id(4);
        unadvertised.action = RenderSemanticAction::Increase;
        assert_eq!(
            state
                .consume_semantic_action(&source, &unadvertised)
                .unwrap_err()
                .code,
            render_error_codes::UNADVERTISED_ACTION
        );

        let mut invalid_selection = unadvertised;
        invalid_selection.request_id = id(5);
        invalid_selection.action = RenderSemanticAction::SetSelection {
            base_offset: 0,
            extent_offset: 4,
        };
        assert_eq!(
            state
                .consume_semantic_action(&source, &invalid_selection)
                .unwrap_err()
                .code,
            render_error_codes::INVALID_GEOMETRY
        );
    }
}
