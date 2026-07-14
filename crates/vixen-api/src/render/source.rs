//! Renderer source revisions, immutable snapshots, and incremental mutations.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    BrowsingContextId, DocumentId, RenderNodeId, RenderResourceId, RenderScrollNodeId,
    SemanticNodeId,
};

use super::{
    RENDER_MAX_MUTATIONS, RENDER_MAX_NODES, RENDER_MAX_RESOURCE_BYTES, RENDER_MAX_RESOURCES,
    RENDER_MAX_RESOURCES_PER_NODE, RENDER_MAX_SCROLL_ENTRIES, RENDER_MAX_SEMANTIC_ACTIONS_PER_NODE,
    RENDER_MAX_STRING_BYTES, RENDER_MAX_STYLES_PER_NODE, RENDER_MAX_TOTAL_RESOURCE_BYTES,
    RENDER_MAX_TOTAL_STRING_BYTES, RENDER_MAX_TREE_DEPTH, RENDER_PROTOCOL_VERSION, RenderPoint,
    RenderProtocolError, RenderViewport, render_error_codes, validate_version,
};

/// Exact BrowserCore source generations consumed by one renderer document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderRevision {
    pub context_id: BrowsingContextId,
    pub document_id: DocumentId,
    pub source_generation: u64,
    pub style_generation: u64,
    pub viewport_generation: u64,
    pub resource_generation: u64,
}

impl RenderRevision {
    pub fn validate(self) -> Result<(), RenderProtocolError> {
        if [
            self.source_generation,
            self.style_generation,
            self.viewport_generation,
            self.resource_generation,
        ]
        .contains(&0)
        {
            return Err(RenderProtocolError::new(
                render_error_codes::REVISION,
                "render revision generations must be non-zero",
            ));
        }
        Ok(())
    }

    pub fn validate_successor_of(self, base: Self) -> Result<(), RenderProtocolError> {
        self.validate()?;
        base.validate()?;
        if self.context_id != base.context_id || self.document_id != base.document_id {
            return Err(RenderProtocolError::new(
                render_error_codes::REVISION,
                "render revision successor changed context or document",
            ));
        }
        let base_generations = [
            base.source_generation,
            base.style_generation,
            base.viewport_generation,
            base.resource_generation,
        ];
        let target_generations = [
            self.source_generation,
            self.style_generation,
            self.viewport_generation,
            self.resource_generation,
        ];
        if target_generations
            .iter()
            .zip(base_generations)
            .any(|(target, base)| *target < base)
            || target_generations == base_generations
        {
            return Err(RenderProtocolError::new(
                render_error_codes::REVISION,
                "render revision must advance without regressing a generation",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RenderSemanticActionKind {
    Activate,
    Focus,
    SetValue,
    SetSelection,
    Increase,
    Decrease,
    ScrollIntoView,
}

/// BrowserCore-authored semantic meaning attached to a stable render node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderSemanticNode {
    pub id: SemanticNodeId,
    pub role: String,
    pub name: String,
    pub value: Option<String>,
    pub action_generation: u64,
    pub actions: Vec<RenderSemanticActionKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderStyleProperty {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderNodeKind {
    Element { local_name: String },
    Text { text: String },
    PseudoBefore { text: String },
    PseudoAfter { text: String },
}

/// Immutable styled input for one DOM, text, or generated-content node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderNode {
    pub id: RenderNodeId,
    pub parent_id: Option<RenderNodeId>,
    pub sibling_index: u32,
    pub depth: u16,
    pub kind: RenderNodeKind,
    pub styles: Vec<RenderStyleProperty>,
    pub resource_ids: Vec<RenderResourceId>,
    pub semantic: Option<RenderSemanticNode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderResourceKind {
    Image,
    Font,
}

/// Policy-accepted immutable resource bytes exposed to the renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderResource {
    pub id: RenderResourceId,
    pub kind: RenderResourceKind,
    pub mime: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RenderScrollIntentKind {
    By(RenderPoint),
    To(RenderPoint),
    Restore(RenderPoint),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderScrollIntent {
    pub scroll_node_id: RenderScrollNodeId,
    pub node_id: RenderNodeId,
    pub kind: RenderScrollIntentKind,
}

/// Complete bounded renderer state used for first load and deterministic resync.
#[derive(Debug, Clone, PartialEq)]
pub struct FullRenderSnapshot {
    pub version: u16,
    pub revision: RenderRevision,
    pub viewport: RenderViewport,
    pub nodes: Vec<RenderNode>,
    pub resources: Vec<RenderResource>,
    pub scroll_intents: Vec<RenderScrollIntent>,
}

impl FullRenderSnapshot {
    pub fn new(revision: RenderRevision, viewport: RenderViewport) -> Self {
        Self {
            version: RENDER_PROTOCOL_VERSION,
            revision,
            viewport,
            nodes: Vec::new(),
            resources: Vec::new(),
            scroll_intents: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<(), RenderProtocolError> {
        validate_version(self.version)?;
        self.revision.validate()?;
        self.viewport.validate()?;
        validate_state(&self.nodes, &self.resources, &self.scroll_intents)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RenderMutation {
    SetViewport(RenderViewport),
    UpsertNode(RenderNode),
    RemoveNode { node_id: RenderNodeId },
    UpsertResource(RenderResource),
    RemoveResource { resource_id: RenderResourceId },
    SetScrollIntent(RenderScrollIntent),
    RemoveScrollIntent { scroll_node_id: RenderScrollNodeId },
}

/// Incremental renderer update that applies only to its exact base revision.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderMutationBatch {
    pub version: u16,
    pub base_revision: RenderRevision,
    pub target_revision: RenderRevision,
    pub mutations: Vec<RenderMutation>,
}

impl RenderMutationBatch {
    pub fn validate(&self) -> Result<(), RenderProtocolError> {
        validate_version(self.version)?;
        self.target_revision
            .validate_successor_of(self.base_revision)?;
        if self.mutations.len() > RENDER_MAX_MUTATIONS {
            return Err(limit_error(
                "mutation",
                self.mutations.len(),
                RENDER_MAX_MUTATIONS,
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderResyncReason {
    MissingState,
    MissedBaseRevision,
    RendererReset,
}

/// Exact request for BrowserCore to replace incremental state with a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderResyncRequest {
    pub version: u16,
    pub context_id: BrowsingContextId,
    pub document_id: DocumentId,
    pub current_revision: Option<RenderRevision>,
    pub rejected_base_revision: Option<RenderRevision>,
    pub reason: RenderResyncReason,
}

impl RenderResyncRequest {
    pub fn renderer_reset(context_id: BrowsingContextId, document_id: DocumentId) -> Self {
        Self {
            version: RENDER_PROTOCOL_VERSION,
            context_id,
            document_id,
            current_revision: None,
            rejected_base_revision: None,
            reason: RenderResyncReason::RendererReset,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyRenderBatchOutcome {
    Applied { revision: RenderRevision },
    ResyncRequired(RenderResyncRequest),
}

/// Small reference replica used to validate atomic snapshot/batch application.
///
/// It owns renderer input only, never BrowserCore state. Native and Dart bridge
/// implementations must preserve the same fail-closed behavior.
#[derive(Debug, Clone, Default)]
pub struct RenderReplica {
    revision: Option<RenderRevision>,
    viewport: Option<RenderViewport>,
    nodes: BTreeMap<RenderNodeId, RenderNode>,
    resources: BTreeMap<RenderResourceId, RenderResource>,
    scroll_intents: BTreeMap<RenderScrollNodeId, RenderScrollIntent>,
}

impl RenderReplica {
    pub fn revision(&self) -> Option<RenderRevision> {
        self.revision
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn viewport(&self) -> Option<RenderViewport> {
        self.viewport
    }

    pub fn resource_count(&self) -> usize {
        self.resources.len()
    }

    pub fn contains_node(&self, node_id: RenderNodeId) -> bool {
        self.nodes.contains_key(&node_id)
    }

    pub fn node_text_utf16_len(&self, node_id: RenderNodeId) -> Option<usize> {
        match &self.nodes.get(&node_id)?.kind {
            RenderNodeKind::Text { text }
            | RenderNodeKind::PseudoBefore { text }
            | RenderNodeKind::PseudoAfter { text } => Some(text.encode_utf16().count()),
            RenderNodeKind::Element { .. } => None,
        }
    }

    pub fn contains_semantic_node(&self, semantic_node_id: SemanticNodeId) -> bool {
        self.nodes.values().any(|node| {
            node.semantic
                .as_ref()
                .is_some_and(|semantic| semantic.id == semantic_node_id)
        })
    }

    pub fn semantic_node(&self, semantic_node_id: SemanticNodeId) -> Option<&RenderSemanticNode> {
        self.nodes.values().find_map(|node| {
            node.semantic
                .as_ref()
                .filter(|semantic| semantic.id == semantic_node_id)
        })
    }

    pub fn semantic_value_utf16_len(&self, semantic_node_id: SemanticNodeId) -> Option<usize> {
        self.semantic_node(semantic_node_id).map(|semantic| {
            semantic
                .value
                .as_deref()
                .unwrap_or_default()
                .encode_utf16()
                .count()
        })
    }

    pub fn node_has_semantic_node(
        &self,
        node_id: RenderNodeId,
        semantic_node_id: SemanticNodeId,
    ) -> bool {
        self.nodes.get(&node_id).is_some_and(|node| {
            node.semantic
                .as_ref()
                .is_some_and(|semantic| semantic.id == semantic_node_id)
        })
    }

    pub fn accept_full_snapshot(
        &mut self,
        snapshot: FullRenderSnapshot,
    ) -> Result<(), RenderProtocolError> {
        snapshot.validate()?;
        let next_nodes = snapshot
            .nodes
            .into_iter()
            .map(|node| (node.id, node))
            .collect::<BTreeMap<_, _>>();
        let next_resources = snapshot
            .resources
            .into_iter()
            .map(|resource| (resource.id, resource))
            .collect::<BTreeMap<_, _>>();
        let next_scroll_intents = snapshot
            .scroll_intents
            .into_iter()
            .map(|intent| (intent.scroll_node_id, intent))
            .collect::<BTreeMap<_, _>>();
        if let Some(current) = self.revision
            && current.context_id == snapshot.revision.context_id
            && current.document_id == snapshot.revision.document_id
        {
            let current_generations = [
                current.source_generation,
                current.style_generation,
                current.viewport_generation,
                current.resource_generation,
            ];
            let next_generations = [
                snapshot.revision.source_generation,
                snapshot.revision.style_generation,
                snapshot.revision.viewport_generation,
                snapshot.revision.resource_generation,
            ];
            if next_generations
                .iter()
                .zip(current_generations)
                .any(|(next, current)| *next < current)
            {
                return Err(RenderProtocolError::new(
                    render_error_codes::STALE,
                    "full render snapshot regressed the current revision",
                ));
            }
            if current == snapshot.revision {
                if self.viewport == Some(snapshot.viewport)
                    && self.nodes == next_nodes
                    && self.resources == next_resources
                    && self.scroll_intents == next_scroll_intents
                {
                    return Ok(());
                }
                return Err(RenderProtocolError::new(
                    render_error_codes::REVISION,
                    "equal render revision carried different snapshot state",
                ));
            }
            if current.viewport_generation == snapshot.revision.viewport_generation
                && self.viewport != Some(snapshot.viewport)
            {
                return Err(RenderProtocolError::new(
                    render_error_codes::REVISION,
                    "full render snapshot changed viewport without advancing its generation",
                ));
            }
        }

        self.revision = Some(snapshot.revision);
        self.viewport = Some(snapshot.viewport);
        self.nodes = next_nodes;
        self.resources = next_resources;
        self.scroll_intents = next_scroll_intents;
        Ok(())
    }

    pub fn apply_batch(
        &mut self,
        batch: RenderMutationBatch,
    ) -> Result<ApplyRenderBatchOutcome, RenderProtocolError> {
        batch.validate()?;
        if self.revision != Some(batch.base_revision) {
            let reason = if self.revision.is_some() {
                RenderResyncReason::MissedBaseRevision
            } else {
                RenderResyncReason::MissingState
            };
            return Ok(ApplyRenderBatchOutcome::ResyncRequired(
                RenderResyncRequest {
                    version: RENDER_PROTOCOL_VERSION,
                    context_id: batch.target_revision.context_id,
                    document_id: batch.target_revision.document_id,
                    current_revision: self.revision,
                    rejected_base_revision: Some(batch.base_revision),
                    reason,
                },
            ));
        }

        let mut nodes = self.nodes.clone();
        let mut resources = self.resources.clone();
        let mut scroll_intents = self.scroll_intents.clone();
        let mut viewport = self.viewport;
        let mut viewport_mutations = 0usize;
        for mutation in batch.mutations {
            match mutation {
                RenderMutation::SetViewport(next_viewport) => {
                    next_viewport.validate()?;
                    viewport = Some(next_viewport);
                    viewport_mutations += 1;
                    if viewport_mutations > 1 {
                        return Err(RenderProtocolError::new(
                            render_error_codes::INVALID_GRAPH,
                            "render mutation batch repeats the viewport",
                        ));
                    }
                }
                RenderMutation::UpsertNode(node) => {
                    nodes.insert(node.id, node);
                }
                RenderMutation::RemoveNode { node_id } => {
                    if nodes.remove(&node_id).is_none() {
                        return Err(unknown_id_error("render node", node_id));
                    }
                }
                RenderMutation::UpsertResource(resource) => {
                    resources.insert(resource.id, resource);
                }
                RenderMutation::RemoveResource { resource_id } => {
                    if resources.remove(&resource_id).is_none() {
                        return Err(unknown_id_error("render resource", resource_id));
                    }
                }
                RenderMutation::SetScrollIntent(intent) => {
                    scroll_intents.insert(intent.scroll_node_id, intent);
                }
                RenderMutation::RemoveScrollIntent { scroll_node_id } => {
                    if scroll_intents.remove(&scroll_node_id).is_none() {
                        return Err(unknown_id_error("render scroll node", scroll_node_id));
                    }
                }
            }
        }

        let viewport_advanced =
            batch.target_revision.viewport_generation != batch.base_revision.viewport_generation;
        if viewport_advanced != (viewport_mutations == 1) {
            return Err(RenderProtocolError::new(
                render_error_codes::REVISION,
                "viewport generation and viewport mutation must advance together",
            ));
        }

        let node_values = nodes.values().cloned().collect::<Vec<_>>();
        let resource_values = resources.values().cloned().collect::<Vec<_>>();
        let scroll_values = scroll_intents.values().copied().collect::<Vec<_>>();
        validate_state(&node_values, &resource_values, &scroll_values)?;

        self.revision = Some(batch.target_revision);
        self.viewport = viewport;
        self.nodes = nodes;
        self.resources = resources;
        self.scroll_intents = scroll_intents;
        Ok(ApplyRenderBatchOutcome::Applied {
            revision: batch.target_revision,
        })
    }
}

fn validate_state(
    nodes: &[RenderNode],
    resources: &[RenderResource],
    scroll_intents: &[RenderScrollIntent],
) -> Result<(), RenderProtocolError> {
    if nodes.len() > RENDER_MAX_NODES {
        return Err(limit_error("node", nodes.len(), RENDER_MAX_NODES));
    }
    if resources.len() > RENDER_MAX_RESOURCES {
        return Err(limit_error(
            "resource",
            resources.len(),
            RENDER_MAX_RESOURCES,
        ));
    }
    if scroll_intents.len() > RENDER_MAX_SCROLL_ENTRIES {
        return Err(limit_error(
            "scroll intent",
            scroll_intents.len(),
            RENDER_MAX_SCROLL_ENTRIES,
        ));
    }

    let mut total_strings = 0usize;
    let mut total_resource_bytes = 0usize;
    let mut resource_ids = BTreeSet::new();
    for resource in resources {
        if !resource_ids.insert(resource.id) {
            return Err(duplicate_id_error("render resource", resource.id));
        }
        add_string_bytes(&mut total_strings, &resource.mime, "resource MIME")?;
        if resource.bytes.len() > RENDER_MAX_RESOURCE_BYTES {
            return Err(limit_error(
                "resource byte",
                resource.bytes.len(),
                RENDER_MAX_RESOURCE_BYTES,
            ));
        }
        total_resource_bytes = total_resource_bytes
            .checked_add(resource.bytes.len())
            .ok_or_else(|| {
                limit_error(
                    "total resource byte",
                    usize::MAX,
                    RENDER_MAX_TOTAL_RESOURCE_BYTES,
                )
            })?;
        if total_resource_bytes > RENDER_MAX_TOTAL_RESOURCE_BYTES {
            return Err(limit_error(
                "total resource byte",
                total_resource_bytes,
                RENDER_MAX_TOTAL_RESOURCE_BYTES,
            ));
        }
    }

    let mut node_by_id = BTreeMap::new();
    let mut semantic_ids = BTreeSet::new();
    for node in nodes {
        if node.depth > RENDER_MAX_TREE_DEPTH {
            return Err(limit_error(
                "render tree depth",
                usize::from(node.depth),
                usize::from(RENDER_MAX_TREE_DEPTH),
            ));
        }
        if node.styles.len() > RENDER_MAX_STYLES_PER_NODE {
            return Err(limit_error(
                "style property",
                node.styles.len(),
                RENDER_MAX_STYLES_PER_NODE,
            ));
        }
        if node.resource_ids.len() > RENDER_MAX_RESOURCES_PER_NODE {
            return Err(limit_error(
                "node resource reference",
                node.resource_ids.len(),
                RENDER_MAX_RESOURCES_PER_NODE,
            ));
        }
        if node_by_id.insert(node.id, node).is_some() {
            return Err(duplicate_id_error("render node", node.id));
        }
        match &node.kind {
            RenderNodeKind::Element { local_name } => {
                add_string_bytes(&mut total_strings, local_name, "element local name")?;
            }
            RenderNodeKind::Text { text }
            | RenderNodeKind::PseudoBefore { text }
            | RenderNodeKind::PseudoAfter { text } => {
                add_string_bytes(&mut total_strings, text, "render text")?;
            }
        }
        let mut style_names = BTreeSet::new();
        for style in &node.styles {
            add_string_bytes(&mut total_strings, &style.name, "style name")?;
            add_string_bytes(&mut total_strings, &style.value, "style value")?;
            if !style_names.insert(style.name.as_str()) {
                return Err(RenderProtocolError::new(
                    render_error_codes::INVALID_GRAPH,
                    format!("render node {} repeats style {}", node.id, style.name),
                ));
            }
        }
        let mut node_resources = BTreeSet::new();
        for resource_id in &node.resource_ids {
            if !node_resources.insert(*resource_id) {
                return Err(duplicate_id_error("node resource reference", resource_id));
            }
            if !resource_ids.contains(resource_id) {
                return Err(unknown_id_error("render resource", resource_id));
            }
        }
        if let Some(semantic) = &node.semantic {
            if semantic.action_generation == 0 {
                return Err(RenderProtocolError::new(
                    render_error_codes::REVISION,
                    format!(
                        "semantic node {} action generation must be non-zero",
                        semantic.id
                    ),
                ));
            }
            if semantic.actions.len() > RENDER_MAX_SEMANTIC_ACTIONS_PER_NODE {
                return Err(limit_error(
                    "semantic action",
                    semantic.actions.len(),
                    RENDER_MAX_SEMANTIC_ACTIONS_PER_NODE,
                ));
            }
            if !semantic_ids.insert(semantic.id) {
                return Err(duplicate_id_error("semantic node", semantic.id));
            }
            add_string_bytes(&mut total_strings, &semantic.role, "semantic role")?;
            add_string_bytes(&mut total_strings, &semantic.name, "semantic name")?;
            if let Some(value) = &semantic.value {
                add_string_bytes(&mut total_strings, value, "semantic value")?;
            }
            let unique_actions = semantic.actions.iter().copied().collect::<BTreeSet<_>>();
            if unique_actions.len() != semantic.actions.len() {
                return Err(RenderProtocolError::new(
                    render_error_codes::INVALID_GRAPH,
                    format!("semantic node {} repeats an advertised action", semantic.id),
                ));
            }
        }
    }

    for node in nodes {
        match node.parent_id {
            Some(parent_id) => {
                let parent = node_by_id
                    .get(&parent_id)
                    .ok_or_else(|| unknown_id_error("parent render node", parent_id))?;
                if node.depth != parent.depth.saturating_add(1) {
                    return Err(RenderProtocolError::new(
                        render_error_codes::INVALID_GRAPH,
                        format!(
                            "render node {} depth {} does not follow parent {} depth {}",
                            node.id, node.depth, parent_id, parent.depth
                        ),
                    ));
                }
            }
            None if node.depth != 0 => {
                return Err(RenderProtocolError::new(
                    render_error_codes::INVALID_GRAPH,
                    format!("root render node {} must have depth zero", node.id),
                ));
            }
            None => {}
        }
    }

    let mut scroll_ids = BTreeSet::new();
    for intent in scroll_intents {
        if !scroll_ids.insert(intent.scroll_node_id) {
            return Err(duplicate_id_error(
                "render scroll node",
                intent.scroll_node_id,
            ));
        }
        if !node_by_id.contains_key(&intent.node_id) {
            return Err(unknown_id_error("scroll render node", intent.node_id));
        }
        match intent.kind {
            RenderScrollIntentKind::By(point)
            | RenderScrollIntentKind::To(point)
            | RenderScrollIntentKind::Restore(point) => {
                point.validate("scroll intent")?;
            }
        }
    }

    Ok(())
}

fn add_string_bytes(
    total: &mut usize,
    value: &str,
    field: &str,
) -> Result<(), RenderProtocolError> {
    if value.len() > RENDER_MAX_STRING_BYTES {
        return Err(limit_error(field, value.len(), RENDER_MAX_STRING_BYTES));
    }
    *total = total.checked_add(value.len()).ok_or_else(|| {
        limit_error(
            "total string byte",
            usize::MAX,
            RENDER_MAX_TOTAL_STRING_BYTES,
        )
    })?;
    if *total > RENDER_MAX_TOTAL_STRING_BYTES {
        return Err(limit_error(
            "total string byte",
            *total,
            RENDER_MAX_TOTAL_STRING_BYTES,
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

    fn root_node(node_id: u64) -> RenderNode {
        RenderNode {
            id: id(node_id),
            parent_id: None,
            sibling_index: 0,
            depth: 0,
            kind: RenderNodeKind::Element {
                local_name: "main".to_owned(),
            },
            styles: vec![RenderStyleProperty {
                name: "display".to_owned(),
                value: "block".to_owned(),
            }],
            resource_ids: Vec::new(),
            semantic: Some(RenderSemanticNode {
                id: id(node_id),
                role: "main".to_owned(),
                name: "Content".to_owned(),
                value: None,
                action_generation: 1,
                actions: vec![RenderSemanticActionKind::Focus],
            }),
        }
    }

    fn snapshot(generation: u64) -> FullRenderSnapshot {
        let mut snapshot = FullRenderSnapshot::new(revision(generation), viewport());
        snapshot.nodes.push(root_node(1));
        snapshot
    }

    #[test]
    fn renderer_ids_reject_malformed_zero_and_round_trip() {
        assert_eq!(RenderNodeId::new(0), None);
        assert_eq!(
            RenderResourceId::try_from(0).unwrap_err().kind(),
            "RenderResourceId"
        );
        let id = RenderNodeId::try_from(41).unwrap();
        assert_eq!(RenderNodeId::try_from(id.get()).unwrap(), id);
    }

    #[test]
    fn revisions_require_nonzero_monotonic_exact_document_generations() {
        let mut invalid = revision(1);
        invalid.style_generation = 0;
        assert_eq!(
            invalid.validate().unwrap_err().code,
            render_error_codes::REVISION
        );

        let base = revision(2);
        let mut target = revision(3);
        target.resource_generation = 1;
        assert_eq!(
            target.validate_successor_of(base).unwrap_err().code,
            render_error_codes::REVISION
        );
        assert_eq!(
            base.validate_successor_of(base).unwrap_err().code,
            render_error_codes::REVISION
        );
        assert!(revision(3).validate_successor_of(base).is_ok());
    }

    #[test]
    fn full_snapshots_reject_unknown_resources_and_excess_depth() {
        let mut unknown_resource = snapshot(1);
        unknown_resource.nodes[0].resource_ids.push(id(9));
        assert_eq!(
            unknown_resource.validate().unwrap_err().code,
            render_error_codes::UNKNOWN_ID
        );

        let mut deep = snapshot(1);
        deep.nodes[0].depth = RENDER_MAX_TREE_DEPTH + 1;
        assert_eq!(deep.validate().unwrap_err().code, render_error_codes::LIMIT);
    }

    #[test]
    fn full_snapshots_reject_duplicate_ids_and_oversized_strings() {
        let mut duplicate = snapshot(1);
        duplicate.nodes.push(root_node(1));
        assert_eq!(
            duplicate.validate().unwrap_err().code,
            render_error_codes::DUPLICATE_ID
        );

        let mut oversized = snapshot(1);
        oversized.nodes[0].kind = RenderNodeKind::Text {
            text: "x".repeat(RENDER_MAX_STRING_BYTES + 1),
        };
        assert_eq!(
            oversized.validate().unwrap_err().code,
            render_error_codes::LIMIT
        );
    }

    #[test]
    fn equal_revision_snapshot_must_be_idempotent() {
        let mut replica = RenderReplica::default();
        let original = snapshot(1);
        replica.accept_full_snapshot(original.clone()).unwrap();
        replica.accept_full_snapshot(original).unwrap();

        let mut changed = snapshot(1);
        changed.nodes[0].kind = RenderNodeKind::Element {
            local_name: "article".to_owned(),
        };
        assert_eq!(
            replica.accept_full_snapshot(changed).unwrap_err().code,
            render_error_codes::REVISION
        );
        assert_eq!(replica.node_count(), 1);
    }

    #[test]
    fn missed_batch_base_requests_deterministic_full_resync() {
        let mut replica = RenderReplica::default();
        replica.accept_full_snapshot(snapshot(1)).unwrap();
        let batch = RenderMutationBatch {
            version: RENDER_PROTOCOL_VERSION,
            base_revision: revision(2),
            target_revision: revision(3),
            mutations: Vec::new(),
        };

        let first = replica.apply_batch(batch.clone()).unwrap();
        let second = replica.apply_batch(batch).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            first,
            ApplyRenderBatchOutcome::ResyncRequired(RenderResyncRequest {
                version: RENDER_PROTOCOL_VERSION,
                context_id: id(1),
                document_id: id(2),
                current_revision: Some(revision(1)),
                rejected_base_revision: Some(revision(2)),
                reason: RenderResyncReason::MissedBaseRevision,
            })
        );
        assert_eq!(replica.revision(), Some(revision(1)));
    }

    #[test]
    fn full_resync_recovers_then_exact_batches_apply() {
        let mut replica = RenderReplica::default();
        let missing = RenderMutationBatch {
            version: RENDER_PROTOCOL_VERSION,
            base_revision: revision(1),
            target_revision: revision(2),
            mutations: Vec::new(),
        };
        assert!(matches!(
            replica.apply_batch(missing).unwrap(),
            ApplyRenderBatchOutcome::ResyncRequired(RenderResyncRequest {
                reason: RenderResyncReason::MissingState,
                ..
            })
        ));

        replica.accept_full_snapshot(snapshot(2)).unwrap();
        let outcome = replica
            .apply_batch(RenderMutationBatch {
                version: RENDER_PROTOCOL_VERSION,
                base_revision: revision(2),
                target_revision: revision(3),
                mutations: vec![RenderMutation::UpsertNode(root_node(3))],
            })
            .unwrap();
        assert_eq!(
            outcome,
            ApplyRenderBatchOutcome::Applied {
                revision: revision(3)
            }
        );
        assert_eq!(replica.node_count(), 2);
    }

    #[test]
    fn viewport_value_and_generation_advance_atomically() {
        let mut replica = RenderReplica::default();
        replica.accept_full_snapshot(snapshot(1)).unwrap();
        let mut target = revision(2);
        target.viewport_generation = 2;

        let missing_value = replica
            .apply_batch(RenderMutationBatch {
                version: RENDER_PROTOCOL_VERSION,
                base_revision: revision(1),
                target_revision: target,
                mutations: Vec::new(),
            })
            .unwrap_err();
        assert_eq!(missing_value.code, render_error_codes::REVISION);
        assert_eq!(replica.revision(), Some(revision(1)));

        let next_viewport = RenderViewport {
            width: 1024,
            height: 768,
            device_scale: 2.0,
            page_zoom: 1.25,
        };
        replica
            .apply_batch(RenderMutationBatch {
                version: RENDER_PROTOCOL_VERSION,
                base_revision: revision(1),
                target_revision: target,
                mutations: vec![RenderMutation::SetViewport(next_viewport)],
            })
            .unwrap();
        assert_eq!(replica.viewport(), Some(next_viewport));
    }

    #[test]
    fn invalid_incremental_state_is_rejected_atomically() {
        let mut replica = RenderReplica::default();
        replica.accept_full_snapshot(snapshot(1)).unwrap();
        let mut references_unknown_resource = root_node(3);
        references_unknown_resource.resource_ids.push(id(99));
        let error = replica
            .apply_batch(RenderMutationBatch {
                version: RENDER_PROTOCOL_VERSION,
                base_revision: revision(1),
                target_revision: revision(2),
                mutations: vec![RenderMutation::UpsertNode(references_unknown_resource)],
            })
            .unwrap_err();
        assert_eq!(error.code, render_error_codes::UNKNOWN_ID);
        assert_eq!(replica.revision(), Some(revision(1)));
        assert_eq!(replica.node_count(), 1);
    }

    #[test]
    fn mutation_and_resource_byte_limits_fail_closed() {
        let batch = RenderMutationBatch {
            version: RENDER_PROTOCOL_VERSION,
            base_revision: revision(1),
            target_revision: revision(2),
            mutations: vec![
                RenderMutation::RemoveNode { node_id: id(1) };
                RENDER_MAX_MUTATIONS + 1
            ],
        };
        assert_eq!(
            batch.validate().unwrap_err().code,
            render_error_codes::LIMIT
        );

        let mut oversized = snapshot(1);
        oversized.resources.push(RenderResource {
            id: id(1),
            kind: RenderResourceKind::Image,
            mime: "image/png".to_owned(),
            bytes: vec![0; RENDER_MAX_RESOURCE_BYTES + 1],
        });
        assert_eq!(
            oversized.validate().unwrap_err().code,
            render_error_codes::LIMIT
        );
    }
}
