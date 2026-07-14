//! Stable JSON projection for the dedicated renderer broker.

use serde_json::{Map, Value, json};
use vixen_api::{
    BrowsingContextId, DocumentId, RENDER_MAX_GEOMETRY_ENTRIES, RENDER_MAX_SCROLL_ENTRIES,
    RENDER_MAX_SEMANTIC_BOUNDS, RENDER_MAX_TEXT_BOXES, RENDER_MAX_TRUNCATION_DIAGNOSTICS,
    RENDER_PROTOCOL_VERSION, RenderBrokerCancellation, RenderBrokerRequest,
    RenderBrokerRequestKind, RenderBrokerResponse, RenderBrokerResponseKind, RenderCommit,
    RenderCommitId, RenderFragmentId, RenderGeometryEntry, RenderHitTestHandle, RenderInputTarget,
    RenderLimitDomain, RenderNodeId, RenderPoint, RenderProtocolError, RenderQueryId, RenderRect,
    RenderRequestId, RenderRevision, RenderScrollNodeId, RenderScrollState, RenderSemanticBounds,
    RenderSize, RenderTextAffinity, RenderTextBox, RenderTextDirection, RenderTextQueryBatchResult,
    RenderTextQueryHandle, RenderTextQueryKind, RenderTextQueryResult, RenderTextQueryValue,
    RenderTruncationDiagnostic, RenderViewport, SemanticNodeId,
};

use crate::c_abi::AbiError;

pub(crate) fn request_json(request: &RenderBrokerRequest) -> Value {
    let body = match &request.kind {
        RenderBrokerRequestKind::EnsureLayout { required_revision } => json!({
            "type": "ensure_layout",
            "required_revision": revision_json(*required_revision),
        }),
        RenderBrokerRequestKind::HitTest(query) => json!({
            "type": "hit_test",
            "context_id": query.context_id.get(),
            "document_id": query.document_id.get(),
            "displayed_commit_id": query.displayed_commit_id.get(),
            "revision": revision_json(query.revision),
            "handle": query.handle.get(),
            "query_id": query.query_id.get(),
            "point": point_json(query.point),
        }),
        RenderBrokerRequestKind::TextQuery(batch) => json!({
            "type": "text_query",
            "context_id": batch.context_id.get(),
            "document_id": batch.document_id.get(),
            "commit_id": batch.commit_id.get(),
            "revision": revision_json(batch.revision),
            "handle": batch.handle.get(),
            "allow_truncation": batch.allow_truncation,
            "queries": batch.queries.iter().map(|query| {
                let kind = match query.kind {
                    RenderTextQueryKind::OffsetForPoint { point } => json!({
                        "type": "offset_for_point", "point": point_json(point),
                    }),
                    RenderTextQueryKind::CaretForOffset { utf16_offset, affinity } => json!({
                        "type": "caret_for_offset", "utf16_offset": utf16_offset,
                        "affinity": affinity_name(affinity),
                    }),
                    RenderTextQueryKind::RangeBoxes { utf16_start, utf16_end } => json!({
                        "type": "range_boxes", "utf16_start": utf16_start,
                        "utf16_end": utf16_end,
                    }),
                };
                json!({"query_id": query.query_id.get(), "node_id": query.node_id.get(), "kind": kind})
            }).collect::<Vec<_>>(),
        }),
    };
    json!({
        "v": RENDER_PROTOCOL_VERSION,
        "type": "renderer_request",
        "request_id": request.request_id.get(),
        "request": body,
    })
}

pub(crate) fn parse_response(input: &str) -> Result<RenderBrokerResponse, AbiError> {
    let value: Value = serde_json::from_str(input)
        .map_err(|error| invalid(format!("invalid renderer JSON: {error}")))?;
    let envelope = object(&value, "renderer response")?;
    keys(envelope, &["request_id", "response", "type", "v"])?;
    if u64_field(envelope, "v")? != u64::from(RENDER_PROTOCOL_VERSION)
        || string(envelope, "type")? != "renderer_response"
    {
        return Err(invalid("unsupported renderer response envelope"));
    }
    let request_id = RenderRequestId::new(u64_field(envelope, "request_id")?)
        .ok_or_else(|| invalid("renderer request_id must be nonzero"))?;
    let response = object_field(envelope, "response")?;
    let kind = match string(response, "type")? {
        "cancelled" => {
            keys(response, &["reason", "type"])?;
            RenderBrokerResponseKind::Cancelled(match string(response, "reason")? {
                "navigation" => RenderBrokerCancellation::Navigation,
                "stop" => RenderBrokerCancellation::Stop,
                "context_closed" => RenderBrokerCancellation::ContextClosed,
                "shutdown" => RenderBrokerCancellation::Shutdown,
                "deadline" => RenderBrokerCancellation::Deadline,
                _ => return Err(invalid("unknown renderer cancellation reason")),
            })
        }
        "failed" => {
            keys(response, &["code", "message", "type"])?;
            RenderBrokerResponseKind::Failed {
                code: bounded_string(response, "code", 256)?,
                message: bounded_string(response, "message", 4096)?,
            }
        }
        "hit_test" => {
            keys(response, &["target", "type"])?;
            let target = response
                .get("target")
                .ok_or_else(|| invalid("hit-test target is required"))?;
            RenderBrokerResponseKind::HitTest(if target.is_null() {
                None
            } else {
                Some(parse_input_target(object(target, "hit-test target")?)?)
            })
        }
        "text_query" => RenderBrokerResponseKind::TextQuery(parse_text_result(response)?),
        "commit" => RenderBrokerResponseKind::Commit(parse_commit(response)?),
        _ => return Err(invalid("unknown renderer response type")),
    };
    Ok(RenderBrokerResponse {
        version: RENDER_PROTOCOL_VERSION,
        request_id,
        kind,
    })
}

fn parse_commit(value: &Map<String, Value>) -> Result<RenderCommit, AbiError> {
    keys(
        value,
        &[
            "commit_id",
            "geometry_index",
            "hit_test_handle",
            "revision",
            "scroll_snapshot",
            "semantic_bounds",
            "text_query_handle",
            "truncations",
            "type",
            "viewport",
        ],
    )?;
    let geometry = array(value, "geometry_index")?;
    let scroll = array(value, "scroll_snapshot")?;
    let semantics = array(value, "semantic_bounds")?;
    let truncations = array(value, "truncations")?;
    if geometry.len() > RENDER_MAX_GEOMETRY_ENTRIES
        || scroll.len() > RENDER_MAX_SCROLL_ENTRIES
        || semantics.len() > RENDER_MAX_SEMANTIC_BOUNDS
        || truncations.len() > RENDER_MAX_TRUNCATION_DIAGNOSTICS
    {
        return Err(invalid("renderer commit exceeds protocol limits"));
    }
    Ok(RenderCommit {
        version: RENDER_PROTOCOL_VERSION,
        commit_id: RenderCommitId::new(u64_field(value, "commit_id")?)
            .ok_or_else(|| invalid("commit_id must be nonzero"))?,
        revision: parse_revision(object_field(value, "revision")?)?,
        viewport: parse_viewport(object_field(value, "viewport")?)?,
        geometry_index: geometry
            .iter()
            .map(|value| parse_geometry(object(value, "geometry entry")?))
            .collect::<Result<_, _>>()?,
        hit_test_handle: RenderHitTestHandle::new(u64_field(value, "hit_test_handle")?)
            .ok_or_else(|| invalid("hit_test_handle must be nonzero"))?,
        text_query_handle: RenderTextQueryHandle::new(u64_field(value, "text_query_handle")?)
            .ok_or_else(|| invalid("text_query_handle must be nonzero"))?,
        scroll_snapshot: scroll
            .iter()
            .map(|value| parse_scroll(object(value, "scroll entry")?))
            .collect::<Result<_, _>>()?,
        semantic_bounds: semantics
            .iter()
            .map(|value| parse_semantic_bounds(object(value, "semantic bounds")?))
            .collect::<Result<_, _>>()?,
        truncations: truncations
            .iter()
            .map(|value| parse_truncation(object(value, "truncation")?))
            .collect::<Result<_, _>>()?,
    })
}

fn parse_geometry(value: &Map<String, Value>) -> Result<RenderGeometryEntry, AbiError> {
    keys(
        value,
        &[
            "border_box",
            "clip",
            "content_box",
            "fragment_id",
            "node_id",
            "padding_box",
            "paint_order",
            "scroll_node_id",
        ],
    )?;
    Ok(RenderGeometryEntry {
        node_id: render_node_id(value, "node_id")?,
        fragment_id: RenderFragmentId::new(u64_field(value, "fragment_id")?)
            .ok_or_else(|| invalid("fragment_id must be nonzero"))?,
        border_box: parse_rect(object_field(value, "border_box")?)?,
        padding_box: parse_rect(object_field(value, "padding_box")?)?,
        content_box: parse_rect(object_field(value, "content_box")?)?,
        clip: optional_object(value, "clip")?
            .map(parse_rect)
            .transpose()?,
        scroll_node_id: optional_id(value, "scroll_node_id", RenderScrollNodeId::new)?,
        paint_order: u32_value(value, "paint_order")?,
    })
}

fn parse_scroll(value: &Map<String, Value>) -> Result<RenderScrollState, AbiError> {
    keys(
        value,
        &[
            "content_size",
            "max_offset",
            "node_id",
            "offset",
            "scroll_node_id",
            "viewport",
        ],
    )?;
    Ok(RenderScrollState {
        scroll_node_id: RenderScrollNodeId::new(u64_field(value, "scroll_node_id")?)
            .ok_or_else(|| invalid("scroll_node_id must be nonzero"))?,
        node_id: render_node_id(value, "node_id")?,
        offset: parse_point(object_field(value, "offset")?)?,
        max_offset: parse_point(object_field(value, "max_offset")?)?,
        viewport: parse_rect(object_field(value, "viewport")?)?,
        content_size: parse_size(object_field(value, "content_size")?)?,
    })
}

fn parse_semantic_bounds(value: &Map<String, Value>) -> Result<RenderSemanticBounds, AbiError> {
    keys(value, &["node_id", "rects", "semantic_node_id"])?;
    Ok(RenderSemanticBounds {
        semantic_node_id: SemanticNodeId::new(u64_field(value, "semantic_node_id")?)
            .ok_or_else(|| invalid("semantic_node_id must be nonzero"))?,
        node_id: render_node_id(value, "node_id")?,
        rects: array(value, "rects")?
            .iter()
            .map(|value| parse_rect(object(value, "semantic rect")?))
            .collect::<Result<_, _>>()?,
    })
}

fn parse_truncation(value: &Map<String, Value>) -> Result<RenderTruncationDiagnostic, AbiError> {
    keys(value, &["domain", "limit", "omitted", "required"])?;
    let domain = match string(value, "domain")? {
        "nodes" => RenderLimitDomain::Nodes,
        "tree_depth" => RenderLimitDomain::TreeDepth,
        "mutations" => RenderLimitDomain::Mutations,
        "resources" => RenderLimitDomain::Resources,
        "resource_bytes" => RenderLimitDomain::ResourceBytes,
        "string_bytes" => RenderLimitDomain::StringBytes,
        "geometry" => RenderLimitDomain::Geometry,
        "scroll_entries" => RenderLimitDomain::ScrollEntries,
        "semantic_bounds" => RenderLimitDomain::SemanticBounds,
        "text_queries" => RenderLimitDomain::TextQueries,
        "text_boxes" => RenderLimitDomain::TextBoxes,
        _ => return Err(invalid("unknown truncation domain")),
    };
    Ok(RenderTruncationDiagnostic {
        domain,
        limit: u64_field(value, "limit")?,
        omitted: u64_field(value, "omitted")?,
        required: bool_field(value, "required")?,
    })
}

fn parse_input_target(value: &Map<String, Value>) -> Result<RenderInputTarget, AbiError> {
    keys(
        value,
        &[
            "context_id",
            "displayed_commit_id",
            "document_id",
            "fragment_id",
            "handle",
            "local_point",
            "node_id",
            "query_id",
            "revision",
            "v",
            "viewport_point",
        ],
    )?;
    version(value)?;
    Ok(RenderInputTarget {
        version: RENDER_PROTOCOL_VERSION,
        query_id: RenderQueryId::new(u64_field(value, "query_id")?)
            .ok_or_else(|| invalid("query_id must be nonzero"))?,
        context_id: context_id(value)?,
        document_id: document_id(value)?,
        displayed_commit_id: RenderCommitId::new(u64_field(value, "displayed_commit_id")?)
            .ok_or_else(|| invalid("displayed_commit_id must be nonzero"))?,
        revision: parse_revision(object_field(value, "revision")?)?,
        handle: RenderHitTestHandle::new(u64_field(value, "handle")?)
            .ok_or_else(|| invalid("hit-test handle must be nonzero"))?,
        node_id: render_node_id(value, "node_id")?,
        fragment_id: RenderFragmentId::new(u64_field(value, "fragment_id")?)
            .ok_or_else(|| invalid("fragment_id must be nonzero"))?,
        viewport_point: parse_point(object_field(value, "viewport_point")?)?,
        local_point: parse_point(object_field(value, "local_point")?)?,
    })
}

fn parse_text_result(value: &Map<String, Value>) -> Result<RenderTextQueryBatchResult, AbiError> {
    keys(
        value,
        &[
            "commit_id",
            "context_id",
            "document_id",
            "results",
            "revision",
            "truncations",
            "type",
        ],
    )?;
    let mut box_count = 0usize;
    let results = array(value, "results")?
        .iter()
        .map(|entry| {
            let entry = object(entry, "text query result")?;
            keys(entry, &["query_id", "value"])?;
            let result_value = object_field(entry, "value")?;
            let parsed = match string(result_value, "type")? {
                "offset" => {
                    keys(result_value, &["affinity", "type", "utf16_offset"])?;
                    RenderTextQueryValue::Offset {
                        utf16_offset: u32_value(result_value, "utf16_offset")?,
                        affinity: parse_affinity(string(result_value, "affinity")?)?,
                    }
                }
                "caret" => {
                    keys(result_value, &["affinity", "rect", "type"])?;
                    RenderTextQueryValue::Caret {
                        rect: parse_rect(object_field(result_value, "rect")?)?,
                        affinity: parse_affinity(string(result_value, "affinity")?)?,
                    }
                }
                "range_boxes" => {
                    keys(result_value, &["boxes", "type"])?;
                    let boxes = array(result_value, "boxes")?;
                    box_count = box_count
                        .checked_add(boxes.len())
                        .ok_or_else(|| invalid("text box count overflows"))?;
                    if box_count > RENDER_MAX_TEXT_BOXES {
                        return Err(invalid("text query result exceeds the text box limit"));
                    }
                    RenderTextQueryValue::RangeBoxes(
                        boxes
                            .iter()
                            .map(|value| {
                                let value = object(value, "text box")?;
                                keys(value, &["direction", "rect"])?;
                                Ok(RenderTextBox {
                                    rect: parse_rect(object_field(value, "rect")?)?,
                                    direction: match string(value, "direction")? {
                                        "ltr" => RenderTextDirection::LeftToRight,
                                        "rtl" => RenderTextDirection::RightToLeft,
                                        _ => return Err(invalid("unknown text direction")),
                                    },
                                })
                            })
                            .collect::<Result<_, AbiError>>()?,
                    )
                }
                _ => return Err(invalid("unknown text query result type")),
            };
            Ok(RenderTextQueryResult {
                query_id: RenderQueryId::new(u64_field(entry, "query_id")?)
                    .ok_or_else(|| invalid("query_id must be nonzero"))?,
                value: parsed,
            })
        })
        .collect::<Result<Vec<_>, AbiError>>()?;
    Ok(RenderTextQueryBatchResult {
        version: RENDER_PROTOCOL_VERSION,
        context_id: context_id(value)?,
        document_id: document_id(value)?,
        commit_id: RenderCommitId::new(u64_field(value, "commit_id")?)
            .ok_or_else(|| invalid("commit_id must be nonzero"))?,
        revision: parse_revision(object_field(value, "revision")?)?,
        results,
        truncations: array(value, "truncations")?
            .iter()
            .map(|value| parse_truncation(object(value, "truncation")?))
            .collect::<Result<_, _>>()?,
    })
}

fn revision_json(revision: RenderRevision) -> Value {
    json!({
        "context_id": revision.context_id.get(),
        "document_id": revision.document_id.get(),
        "source_generation": revision.source_generation,
        "style_generation": revision.style_generation,
        "viewport_generation": revision.viewport_generation,
        "resource_generation": revision.resource_generation,
    })
}

fn parse_revision(value: &Map<String, Value>) -> Result<RenderRevision, AbiError> {
    keys(
        value,
        &[
            "context_id",
            "document_id",
            "resource_generation",
            "source_generation",
            "style_generation",
            "viewport_generation",
        ],
    )?;
    let revision = RenderRevision {
        context_id: context_id(value)?,
        document_id: document_id(value)?,
        source_generation: u64_field(value, "source_generation")?,
        style_generation: u64_field(value, "style_generation")?,
        viewport_generation: u64_field(value, "viewport_generation")?,
        resource_generation: u64_field(value, "resource_generation")?,
    };
    revision
        .validate()
        .map_err(|error| invalid(error.message))?;
    Ok(revision)
}

fn parse_viewport(value: &Map<String, Value>) -> Result<RenderViewport, AbiError> {
    keys(value, &["device_scale", "height", "page_zoom", "width"])?;
    let viewport = RenderViewport {
        width: u32_value(value, "width")?,
        height: u32_value(value, "height")?,
        device_scale: f64_field(value, "device_scale")?,
        page_zoom: f64_field(value, "page_zoom")?,
    };
    viewport
        .validate()
        .map_err(|error| invalid(error.message))?;
    Ok(viewport)
}

fn point_json(point: RenderPoint) -> Value {
    json!({"x": point.x, "y": point.y})
}

fn parse_point(value: &Map<String, Value>) -> Result<RenderPoint, AbiError> {
    keys(value, &["x", "y"])?;
    let point = RenderPoint {
        x: f64_field(value, "x")?,
        y: f64_field(value, "y")?,
    };
    point.validate("wire point").map_err(protocol_error)?;
    Ok(point)
}

fn parse_size(value: &Map<String, Value>) -> Result<RenderSize, AbiError> {
    keys(value, &["height", "width"])?;
    let size = RenderSize {
        width: f64_field(value, "width")?,
        height: f64_field(value, "height")?,
    };
    size.validate("wire size").map_err(protocol_error)?;
    Ok(size)
}

fn parse_rect(value: &Map<String, Value>) -> Result<RenderRect, AbiError> {
    keys(value, &["height", "width", "x", "y"])?;
    let rect = RenderRect {
        x: f64_field(value, "x")?,
        y: f64_field(value, "y")?,
        width: f64_field(value, "width")?,
        height: f64_field(value, "height")?,
    };
    rect.validate("wire rectangle").map_err(protocol_error)?;
    Ok(rect)
}

fn affinity_name(affinity: RenderTextAffinity) -> &'static str {
    match affinity {
        RenderTextAffinity::Upstream => "upstream",
        RenderTextAffinity::Downstream => "downstream",
    }
}

fn parse_affinity(value: &str) -> Result<RenderTextAffinity, AbiError> {
    match value {
        "upstream" => Ok(RenderTextAffinity::Upstream),
        "downstream" => Ok(RenderTextAffinity::Downstream),
        _ => Err(invalid("unknown text affinity")),
    }
}

fn version(value: &Map<String, Value>) -> Result<(), AbiError> {
    if u64_field(value, "v")? != u64::from(RENDER_PROTOCOL_VERSION) {
        return Err(invalid("renderer payload version is unsupported"));
    }
    Ok(())
}

fn context_id(value: &Map<String, Value>) -> Result<BrowsingContextId, AbiError> {
    BrowsingContextId::new(u64_field(value, "context_id")?)
        .ok_or_else(|| invalid("context_id must be nonzero"))
}

fn document_id(value: &Map<String, Value>) -> Result<DocumentId, AbiError> {
    DocumentId::new(u64_field(value, "document_id")?)
        .ok_or_else(|| invalid("document_id must be nonzero"))
}

fn render_node_id(value: &Map<String, Value>, field: &str) -> Result<RenderNodeId, AbiError> {
    RenderNodeId::new(u64_field(value, field)?)
        .ok_or_else(|| invalid(format!("{field} must be nonzero")))
}

fn optional_id<T>(
    value: &Map<String, Value>,
    field: &str,
    constructor: impl FnOnce(u64) -> Option<T>,
) -> Result<Option<T>, AbiError> {
    let value = value
        .get(field)
        .ok_or_else(|| invalid(format!("{field} is required")))?;
    if value.is_null() {
        return Ok(None);
    }
    constructor(
        value
            .as_u64()
            .ok_or_else(|| invalid(format!("{field} must be an integer or null")))?,
    )
    .map(Some)
    .ok_or_else(|| invalid(format!("{field} must be nonzero")))
}

fn object<'a>(value: &'a Value, name: &str) -> Result<&'a Map<String, Value>, AbiError> {
    value
        .as_object()
        .ok_or_else(|| invalid(format!("{name} must be an object")))
}

fn object_field<'a>(
    value: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a Map<String, Value>, AbiError> {
    object(
        value
            .get(field)
            .ok_or_else(|| invalid(format!("{field} is required")))?,
        field,
    )
}

fn optional_object<'a>(
    value: &'a Map<String, Value>,
    field: &str,
) -> Result<Option<&'a Map<String, Value>>, AbiError> {
    let value = value
        .get(field)
        .ok_or_else(|| invalid(format!("{field} is required")))?;
    if value.is_null() {
        Ok(None)
    } else {
        object(value, field).map(Some)
    }
}

fn array<'a>(value: &'a Map<String, Value>, field: &str) -> Result<&'a [Value], AbiError> {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| invalid(format!("{field} must be an array")))
}

fn string<'a>(value: &'a Map<String, Value>, field: &str) -> Result<&'a str, AbiError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("{field} must be a string")))
}

fn bounded_string(
    value: &Map<String, Value>,
    field: &str,
    limit: usize,
) -> Result<String, AbiError> {
    let value = string(value, field)?;
    if value.len() > limit {
        return Err(invalid(format!("{field} exceeds {limit} bytes")));
    }
    Ok(value.to_owned())
}

fn u64_field(value: &Map<String, Value>, field: &str) -> Result<u64, AbiError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid(format!("{field} must be an unsigned integer")))
}

fn u32_value(value: &Map<String, Value>, field: &str) -> Result<u32, AbiError> {
    u32::try_from(u64_field(value, field)?)
        .map_err(|_| invalid(format!("{field} must fit unsigned 32 bits")))
}

fn f64_field(value: &Map<String, Value>, field: &str) -> Result<f64, AbiError> {
    value
        .get(field)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .ok_or_else(|| invalid(format!("{field} must be a finite number")))
}

fn bool_field(value: &Map<String, Value>, field: &str) -> Result<bool, AbiError> {
    value
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| invalid(format!("{field} must be a boolean")))
}

fn keys(value: &Map<String, Value>, expected: &[&str]) -> Result<(), AbiError> {
    if value.len() != expected.len() || !expected.iter().all(|field| value.contains_key(*field)) {
        return Err(invalid("renderer object has missing or unknown fields"));
    }
    Ok(())
}

fn protocol_error(error: RenderProtocolError) -> AbiError {
    invalid(error.message)
}

fn invalid(message: impl Into<String>) -> AbiError {
    AbiError::invalid_command(message)
}

#[cfg(test)]
mod tests {
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
    fn ensure_layout_request_has_stable_golden_shape() {
        let request = RenderBrokerRequest {
            version: RENDER_PROTOCOL_VERSION,
            request_id: RenderRequestId::new(7).unwrap(),
            kind: RenderBrokerRequestKind::EnsureLayout {
                required_revision: revision(),
            },
        };
        assert_eq!(
            request_json(&request),
            json!({
                "v": 1,
                "type": "renderer_request",
                "request_id": 7,
                "request": {
                    "type": "ensure_layout",
                    "required_revision": {
                        "context_id": 1,
                        "document_id": 2,
                        "source_generation": 3,
                        "style_generation": 4,
                        "viewport_generation": 5,
                        "resource_generation": 6,
                    }
                }
            })
        );
    }

    #[test]
    fn response_parser_is_strict_and_correlated() {
        let parsed = parse_response(
            r#"{"v":1,"type":"renderer_response","request_id":7,"response":{"type":"cancelled","reason":"stop"}}"#,
        )
        .unwrap();
        assert_eq!(parsed.request_id, RenderRequestId::new(7).unwrap());
        assert!(matches!(
            parsed.kind,
            RenderBrokerResponseKind::Cancelled(RenderBrokerCancellation::Stop)
        ));
        assert!(parse_response(
            r#"{"v":1,"type":"renderer_response","request_id":7,"response":{"type":"cancelled","reason":"stop","extra":true}}"#
        )
        .is_err());
    }
}
