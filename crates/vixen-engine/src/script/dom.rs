//! DOM snapshot host extension for the JS runtime.
//!
//! This module is the Phase 6 bridge from [`crate::page::Page`] snapshots into
//! a JS global. It deliberately exposes a small, fail-closed subset while the
//! full DOM/WebIDL binding layer is still landing; `Element.textContent` is the
//! first mutating slice and is committed back to the authoritative [`Page`].

#![forbid(unsafe_code)]

use std::borrow::Cow;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use deno_core::serde_json::json;
use deno_core::{Extension, ExtensionFileSource, OpState};

use vixen_api::{
    ElementInfo, RENDER_PROTOCOL_VERSION, RenderQueryId, RenderTextAffinity, RenderTextQuery,
    RenderTextQueryBatch, RenderTextQueryKind, RenderTextQueryValue,
};

use crate::class_list::DomTokenList;
use crate::dataset::collect_dataset;
use crate::engine_error::{EngineError, codes};
use crate::form_submission::{FormEntry, FormEntryValue};
use crate::media_query::Viewport;
use crate::page::{Page, PageSelection};
use crate::responsive_select::select_from as select_responsive_image_source;
use crate::style_dom::ElementRelation;

struct DomHost(Rc<DomHostState>);

struct DomHostState {
    snapshot: deno_core::serde_json::Value,
    elements: Vec<DomElementRecord>,
    text_overrides: Mutex<HashMap<usize, String>>,
    mutations: DomMutationSink,
    synchronous_layout: Option<SynchronousLayoutHost>,
}

#[derive(Clone)]
pub(super) struct SynchronousLayoutHost {
    pub(super) config: super::SynchronousLayoutConfig,
    pub(super) mutations: DomMutationSink,
    pub(super) cancellation: super::RenderLayoutCancellation,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum DomMutation {
    SetDocumentTitle {
        value: String,
    },
    SetTextContent {
        node_id: usize,
        value: String,
    },
    SetAttribute {
        node_id: usize,
        name: String,
        value: String,
    },
    RemoveAttribute {
        node_id: usize,
        name: String,
    },
    SetInnerHtml {
        node_id: usize,
        html: String,
    },
    SetControlValue {
        node_id: usize,
        element_id: Option<String>,
        name: Option<String>,
        tag: String,
        value: String,
    },
    SetControlSelection {
        node_id: usize,
        element_id: Option<String>,
        name: Option<String>,
        tag: String,
        base_offset: u32,
        extent_offset: u32,
    },
    SetContenteditableState {
        node_id: usize,
        value: String,
        base_offset: u32,
        extent_offset: u32,
    },
    SetFocusedElement {
        node_id: Option<usize>,
    },
    SetSelection {
        selection: Option<PageSelection>,
    },
    SetRootScroll {
        x: f64,
        y: f64,
    },
    SetElementScroll {
        node_id: usize,
        element_id: Option<String>,
        tag: String,
        x: f64,
        y: f64,
    },
}

#[derive(Clone, Default)]
pub(super) struct DomMutationSink(Arc<Mutex<Vec<DomMutation>>>);

impl DomMutationSink {
    pub(super) fn take(&self) -> Vec<DomMutation> {
        self.0
            .lock()
            .expect("DOM mutation sink poisoned")
            .drain(..)
            .collect()
    }

    fn push(&self, mutation: DomMutation) {
        let mut pending = self.0.lock().expect("DOM mutation sink poisoned");
        if let DomMutation::SetElementScroll { node_id, .. } = &mutation {
            for existing in pending.iter_mut().rev() {
                match existing {
                    DomMutation::SetElementScroll {
                        node_id: existing_node_id,
                        ..
                    } if existing_node_id == node_id => {
                        *existing = mutation;
                        return;
                    }
                    DomMutation::SetTextContent { .. } | DomMutation::SetInnerHtml { .. } => break,
                    _ => {}
                }
            }
        }
        pending.push(mutation);
    }
}

#[derive(Clone)]
struct DomElementRecord {
    node_id: usize,
    tag: String,
    id: Option<String>,
    classes: Vec<String>,
    attributes: Vec<(String, String)>,
    text_content: String,
    inner_html: String,
    outer_html: String,
    bbox: Option<(f64, f64, f64, f64)>,
    scroll_position: (f32, f32),
    scroll_max: (f32, f32),
    user_scrollable: bool,
    overflow_clips: bool,
    fixed_position: bool,
    form_entries: Option<Vec<FormEntry>>,
    class_tokens: Vec<String>,
    rel_tokens: Vec<String>,
    sandbox_tokens: Vec<String>,
    dataset: Vec<(String, String)>,
    parent_node_id: Option<usize>,
    first_element_child_node_id: Option<usize>,
    last_element_child_node_id: Option<usize>,
    previous_element_sibling_node_id: Option<usize>,
    next_element_sibling_node_id: Option<usize>,
    child_element_node_ids: Vec<usize>,
}

#[derive(Debug)]
struct SimpleSelector {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
    attributes: Vec<SimpleAttributeSelector>,
}

#[derive(Debug)]
enum SimpleAttributeSelector {
    Exists(String),
    Equals(String, String),
}

deno_core::extension!(
    vixen_dom,
    ops = [
        op_vixen_dom_snapshot,
        op_vixen_dom_element_snapshot,
        op_vixen_dom_query_selector_all,
        op_vixen_dom_get_element_by_id,
        op_vixen_dom_element_matches,
        op_vixen_dom_element_text,
        op_vixen_dom_element_attribute,
        op_vixen_dom_element_tokens,
        op_vixen_dom_element_dataset,
        op_vixen_dom_element_rect,
        op_vixen_dom_range_rect,
        op_vixen_dom_image_current_src,
        op_vixen_dom_form_entries,
        op_vixen_dom_set_document_title,
        op_vixen_dom_set_element_text,
        op_vixen_dom_set_element_attr,
        op_vixen_dom_remove_element_attr,
        op_vixen_dom_set_element_inner_html,
        op_vixen_dom_set_control_value,
        op_vixen_dom_set_control_selection,
        op_vixen_dom_set_contenteditable_state,
        op_vixen_dom_set_focused_element,
        op_vixen_dom_set_selection,
        op_vixen_dom_scroll_state,
        op_vixen_dom_set_root_scroll,
        op_vixen_dom_set_element_scroll,
    ],
    options = {
        host: Rc<DomHostState>,
    },
    state = |state, options| {
        state.put(DomHost(options.host))
    },
);

pub(super) fn extension(
    page: &Page,
    mutations: DomMutationSink,
    synchronous_layout: Option<SynchronousLayoutHost>,
) -> Result<Extension, EngineError> {
    let host = dom_host_state(page, mutations, synchronous_layout).map_err(|err| {
        EngineError::script(
            codes::SCRIPT_EVAL,
            format!("failed to build DOM host snapshot: {err}"),
        )
    })?;
    let mut extension = vixen_dom::init(Rc::new(host));
    extension.js_files = Cow::Owned(vec![ExtensionFileSource::new_computed(
        "ext:vixen_dom/bootstrap.js",
        Arc::<str>::from(DOM_API_BOOTSTRAP),
    )]);
    Ok(extension)
}

pub(super) fn refresh(
    runtime: &mut deno_core::JsRuntime,
    page: &Page,
    mutations: DomMutationSink,
) -> Result<(), EngineError> {
    let synchronous_layout = runtime
        .op_state()
        .borrow()
        .try_borrow::<DomHost>()
        .and_then(|host| host.0.synchronous_layout.clone());
    let host = dom_host_state(page, mutations, synchronous_layout).map_err(|err| {
        EngineError::script(
            codes::SCRIPT_EVAL,
            format!("failed to refresh DOM host snapshot: {err}"),
        )
    })?;
    runtime.op_state().borrow_mut().put(DomHost(Rc::new(host)));
    Ok(())
}

pub(super) fn element_scroll_state_source(page: &Page, emit_scroll: bool) -> String {
    let states = page
        .element_scroll_state_snapshot()
        .into_iter()
        .map(|state| {
            json!({
                "nodeId": state.node_id,
                "id": state.element_id,
                "tag": state.tag,
                "left": state.position.0,
                "top": state.position.1,
                "maxX": state.max.0,
                "maxY": state.max.1,
                "userScrollable": state.user_scrollable,
            })
        })
        .collect::<Vec<_>>();
    format!(
        "globalThis.__vixenSyncElementScrollState ? globalThis.__vixenSyncElementScrollState({}, {emit_scroll}) : false",
        deno_core::serde_json::Value::Array(states),
    )
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_snapshot(state: &mut OpState) -> deno_core::serde_json::Value {
    state.borrow::<DomHost>().0.snapshot.clone()
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_element_snapshot(
    state: &mut OpState,
    node_id: u32,
) -> deno_core::serde_json::Value {
    let host = state.borrow::<DomHost>();
    element_record_by_node_id(&host.0, node_id as usize)
        .map(element_record_value)
        .unwrap_or(deno_core::serde_json::Value::Null)
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_query_selector_all(
    state: &mut OpState,
    #[string] selector: String,
) -> deno_core::serde_json::Value {
    let host = state.borrow::<DomHost>().0.clone();
    match query_selector_node_ids(&host, &selector) {
        Ok(node_ids) => json!({ "ok": true, "nodeIds": node_ids }),
        Err(message) => json!({ "ok": false, "message": message }),
    }
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_get_element_by_id(
    state: &mut OpState,
    #[string] id: String,
) -> deno_core::serde_json::Value {
    let host = state.borrow::<DomHost>();
    host.0
        .elements
        .iter()
        .find(|record| record.id.as_deref() == Some(id.as_str()))
        .map(|record| json!(record.node_id))
        .unwrap_or(deno_core::serde_json::Value::Null)
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_element_matches(
    state: &mut OpState,
    node_id: u32,
    #[string] selector: String,
) -> deno_core::serde_json::Value {
    let host = state.borrow::<DomHost>().0.clone();
    let parsed = match parse_simple_selector(&selector) {
        Ok(parsed) => parsed,
        Err(message) => return json!({ "ok": false, "message": message }),
    };
    let matches = host
        .elements
        .iter()
        .find(|record| record.node_id == node_id as usize)
        .is_some_and(|record| record_matches(record, &parsed));
    json!({ "ok": true, "matches": matches })
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_element_text(state: &mut OpState, node_id: u32) -> deno_core::serde_json::Value {
    let host = state.borrow::<DomHost>();
    let Some(record) = element_record_by_node_id(&host.0, node_id as usize) else {
        return missing_element_result(node_id);
    };
    json!({ "ok": true, "value": record_text_content(&host.0, record) })
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_set_element_text(
    state: &mut OpState,
    node_id: u32,
    #[string] value: String,
) -> deno_core::serde_json::Value {
    let host = state.borrow::<DomHost>().0.clone();
    let node_id = node_id as usize;
    if element_record_by_node_id(&host, node_id).is_none() {
        return missing_element_result(node_id as u32);
    }

    host.text_overrides
        .lock()
        .expect("DOM text override map poisoned")
        .insert(node_id, value.clone());
    host.mutations
        .push(DomMutation::SetTextContent { node_id, value });
    json!({ "ok": true })
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_set_document_title(
    state: &mut OpState,
    #[string] value: String,
) -> deno_core::serde_json::Value {
    let host = state.borrow::<DomHost>().0.clone();
    host.mutations.push(DomMutation::SetDocumentTitle { value });
    json!({ "ok": true })
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_set_element_attr(
    state: &mut OpState,
    node_id: u32,
    #[string] name: String,
    #[string] value: String,
) -> deno_core::serde_json::Value {
    let host = state.borrow::<DomHost>().0.clone();
    let node_id = node_id as usize;
    if element_record_by_node_id(&host, node_id).is_none() {
        return missing_element_result(node_id as u32);
    }

    host.mutations.push(DomMutation::SetAttribute {
        node_id,
        name,
        value,
    });
    json!({ "ok": true })
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_remove_element_attr(
    state: &mut OpState,
    node_id: u32,
    #[string] name: String,
) -> deno_core::serde_json::Value {
    let host = state.borrow::<DomHost>().0.clone();
    let node_id = node_id as usize;
    if element_record_by_node_id(&host, node_id).is_none() {
        return missing_element_result(node_id as u32);
    }

    host.mutations
        .push(DomMutation::RemoveAttribute { node_id, name });
    json!({ "ok": true })
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_set_element_inner_html(
    state: &mut OpState,
    node_id: u32,
    #[string] html: String,
) -> deno_core::serde_json::Value {
    let host = state.borrow::<DomHost>().0.clone();
    let node_id = node_id as usize;
    if element_record_by_node_id(&host, node_id).is_none() {
        return missing_element_result(node_id as u32);
    }

    host.mutations
        .push(DomMutation::SetInnerHtml { node_id, html });
    json!({ "ok": true })
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_set_control_value(
    state: &mut OpState,
    node_id: u32,
    #[string] element_id: String,
    #[string] name: String,
    #[string] tag: String,
    #[string] value: String,
) -> deno_core::serde_json::Value {
    let host = state.borrow::<DomHost>().0.clone();
    let node_id = node_id as usize;
    if element_record_by_node_id(&host, node_id).is_none() {
        return missing_element_result(node_id as u32);
    }

    host.mutations.push(DomMutation::SetControlValue {
        node_id,
        element_id: (!element_id.is_empty()).then_some(element_id),
        name: (!name.is_empty()).then_some(name),
        tag,
        value,
    });
    json!({ "ok": true })
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_set_control_selection(
    state: &mut OpState,
    node_id: u32,
    #[string] element_id: String,
    #[string] name: String,
    #[string] tag: String,
    base_offset: u32,
    extent_offset: u32,
) -> deno_core::serde_json::Value {
    let host = state.borrow::<DomHost>().0.clone();
    let node_id = node_id as usize;
    if element_record_by_node_id(&host, node_id).is_none() {
        return missing_element_result(node_id as u32);
    }
    host.mutations.push(DomMutation::SetControlSelection {
        node_id,
        element_id: (!element_id.is_empty()).then_some(element_id),
        name: (!name.is_empty()).then_some(name),
        tag,
        base_offset,
        extent_offset,
    });
    json!({ "ok": true })
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_set_contenteditable_state(
    state: &mut OpState,
    node_id: u32,
    #[string] value: String,
    base_offset: u32,
    extent_offset: u32,
) -> deno_core::serde_json::Value {
    let host = state.borrow::<DomHost>().0.clone();
    let node_id = node_id as usize;
    let Some(record) = element_record_by_node_id(&host, node_id) else {
        return missing_element_result(node_id as u32);
    };
    let editable = record.attributes.iter().any(|(name, value)| {
        name == "contenteditable"
            && !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "false" | "inherit"
            )
    });
    if !editable {
        return json!({ "ok": false, "message": "text input target is not a contenteditable host" });
    }
    let utf16_len = value.encode_utf16().count();
    if base_offset as usize > utf16_len || extent_offset as usize > utf16_len {
        return json!({ "ok": false, "message": "contenteditable selection exceeds the UTF-16 text length" });
    }

    host.text_overrides
        .lock()
        .expect("DOM text override map poisoned")
        .insert(node_id, value.clone());
    host.mutations.push(DomMutation::SetContenteditableState {
        node_id,
        value,
        base_offset,
        extent_offset,
    });
    json!({ "ok": true })
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_set_focused_element(
    state: &mut OpState,
    node_id: u32,
) -> deno_core::serde_json::Value {
    let host = state.borrow::<DomHost>().0.clone();
    let node_id = (node_id != 0).then_some(node_id as usize);
    if node_id.is_some_and(|node_id| element_record_by_node_id(&host, node_id).is_none()) {
        return json!({ "ok": false, "message": "focused element is not page-backed" });
    }
    host.mutations
        .push(DomMutation::SetFocusedElement { node_id });
    json!({ "ok": true })
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_set_selection(
    state: &mut OpState,
    #[serde] value: deno_core::serde_json::Value,
) -> deno_core::serde_json::Value {
    let host = state.borrow::<DomHost>().0.clone();
    let node_id = |name: &str| {
        value
            .get(name)
            .and_then(deno_core::serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
    };
    let offset = |name: &str| {
        value
            .get(name)
            .and_then(deno_core::serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
    };
    let selection = match (
        node_id("anchorNodeId"),
        offset("anchorOffset"),
        node_id("focusNodeId"),
        offset("focusOffset"),
    ) {
        (Some(anchor_node_id), Some(anchor_offset), Some(focus_node_id), Some(focus_offset))
            if [anchor_node_id, focus_node_id].into_iter().all(|node_id| {
                node_id == 0 || element_record_by_node_id(&host, node_id).is_some()
            }) =>
        {
            Some(PageSelection {
                anchor_node_id,
                anchor_offset,
                focus_node_id,
                focus_offset,
            })
        }
        _ if value.is_null() => None,
        _ => {
            return json!({
                "ok": false,
                "message": "selection contains a non-page-backed boundary",
            });
        }
    };
    host.mutations.push(DomMutation::SetSelection { selection });
    json!({ "ok": true })
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_set_root_scroll(
    state: &mut OpState,
    x: f64,
    y: f64,
) -> deno_core::serde_json::Value {
    if !x.is_finite() || !y.is_finite() {
        return json!({ "ok": false, "message": "root scroll coordinates must be finite" });
    }
    let host = state.borrow::<DomHost>().0.clone();
    host.mutations.push(DomMutation::SetRootScroll { x, y });
    json!({ "ok": true })
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_scroll_state(state: &mut OpState) -> deno_core::serde_json::Value {
    let host = state.borrow::<DomHost>().0.clone();
    let Some(layout) = &host.synchronous_layout else {
        return json!({ "ok": true, "rendererState": false });
    };
    let (_, commit) = match synchronous_layout(layout) {
        Ok(result) => result,
        Err(message) => return json!({ "ok": false, "message": message }),
    };
    let output_scale = commit.viewport.device_scale * commit.viewport.page_zoom;
    let root = commit
        .scroll_snapshot
        .iter()
        .find(|scroll| scroll.scroll_node_id.get() == 1)
        .map(|scroll| {
            json!({
                "left": scroll.offset.x / output_scale,
                "top": scroll.offset.y / output_scale,
                "maxX": scroll.max_offset.x / output_scale,
                "maxY": scroll.max_offset.y / output_scale,
            })
        });
    let page = layout.config.page.borrow();
    let states = page
        .element_scroll_state_snapshot()
        .into_iter()
        .map(|scroll| {
            json!({
                "nodeId": scroll.node_id,
                "id": scroll.element_id,
                "tag": scroll.tag,
                "left": scroll.position.0,
                "top": scroll.position.1,
                "maxX": scroll.max.0,
                "maxY": scroll.max.1,
                "userScrollable": scroll.user_scrollable,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "ok": true,
        "rendererState": true,
        "root": root,
        "states": states,
    })
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_set_element_scroll(
    state: &mut OpState,
    node_id: u32,
    x: f64,
    y: f64,
) -> deno_core::serde_json::Value {
    if !x.is_finite() || !y.is_finite() {
        return json!({ "ok": false, "message": "element scroll coordinates must be finite" });
    }
    let host = state.borrow::<DomHost>().0.clone();
    let node_id = node_id as usize;
    if element_record_by_node_id(&host, node_id).is_none() {
        return missing_element_result(node_id as u32);
    }
    let record = element_record_by_node_id(&host, node_id).expect("element checked");
    host.mutations.push(DomMutation::SetElementScroll {
        node_id,
        element_id: record.id.clone(),
        tag: record.tag.clone(),
        x,
        y,
    });
    json!({ "ok": true })
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_element_attribute(
    state: &mut OpState,
    node_id: u32,
    #[string] name: String,
) -> deno_core::serde_json::Value {
    let host = state.borrow::<DomHost>();
    let Some(record) = element_record_by_node_id(&host.0, node_id as usize) else {
        return missing_element_result(node_id);
    };
    json!({ "ok": true, "value": record_attr(record, &name) })
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_element_tokens(
    state: &mut OpState,
    node_id: u32,
    #[string] attribute: String,
) -> deno_core::serde_json::Value {
    let host = state.borrow::<DomHost>();
    let Some(record) = element_record_by_node_id(&host.0, node_id as usize) else {
        return missing_element_result(node_id);
    };
    match record_tokens(record, &attribute) {
        Ok(tokens) => json!({ "ok": true, "tokens": tokens }),
        Err(message) => json!({ "ok": false, "message": message }),
    }
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_element_dataset(state: &mut OpState, node_id: u32) -> deno_core::serde_json::Value {
    let host = state.borrow::<DomHost>();
    let Some(record) = element_record_by_node_id(&host.0, node_id as usize) else {
        return missing_element_result(node_id);
    };
    json!({ "ok": true, "pairs": &record.dataset })
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_element_rect(state: &mut OpState, node_id: u32) -> deno_core::serde_json::Value {
    let host = state.borrow::<DomHost>().0.clone();
    if let Some(layout) = &host.synchronous_layout {
        return synchronous_layout_rect(layout, node_id as usize);
    }
    let Some(record) = element_record_by_node_id(&host, node_id as usize) else {
        return missing_element_result(node_id);
    };
    json!({ "ok": true, "rect": record.bbox.map(rect_value) })
}

fn synchronous_layout_rect(
    layout: &SynchronousLayoutHost,
    node_id: usize,
) -> deno_core::serde_json::Value {
    let (_, commit) = match synchronous_layout(layout) {
        Ok(result) => result,
        Err(message) => return json!({ "ok": false, "message": message }),
    };
    let Ok(raw_node_id) = u64::try_from(node_id) else {
        return json!({ "ok": false, "message": "DOM node id exceeds renderer range" });
    };
    let rect = commit
        .geometry_index
        .iter()
        .filter(|geometry| geometry.node_id.get() == raw_node_id)
        .map(|geometry| geometry.border_box)
        .reduce(union_render_rect);
    renderer_rect_result(rect, renderer_output_scale(&commit))
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_range_rect(
    state: &mut OpState,
    parent_node_id: u32,
    start_offset: u32,
    end_offset: u32,
    collapsed: bool,
    whole_text: bool,
) -> deno_core::serde_json::Value {
    let host = state.borrow::<DomHost>().0.clone();
    let Some(layout) = &host.synchronous_layout else {
        return json!({ "ok": true, "rendererGeometry": false, "rect": null });
    };
    let (snapshot, commit) = match synchronous_layout(layout) {
        Ok(result) => result,
        Err(message) => return json!({ "ok": false, "message": message }),
    };
    let Some(parent_id) = vixen_api::RenderNodeId::new(u64::from(parent_node_id)) else {
        return json!({ "ok": false, "message": "range parent node id is invalid" });
    };
    let text_nodes = snapshot
        .nodes
        .iter()
        .filter(|node| matches!(node.kind, vixen_api::RenderNodeKind::Text { .. }))
        .filter(|node| render_node_is_descendant(&snapshot, node.id, parent_id))
        .collect::<Vec<_>>();
    if text_nodes.is_empty() {
        return renderer_rect_result(None, renderer_output_scale(&commit));
    }

    let selected = if whole_text {
        text_nodes.as_slice()
    } else {
        &text_nodes[..1]
    };
    let queries = selected
        .iter()
        .enumerate()
        .map(|(index, node)| {
            let text_len = match &node.kind {
                vixen_api::RenderNodeKind::Text { text } => {
                    u32::try_from(text.encode_utf16().count()).unwrap_or(u32::MAX)
                }
                _ => unreachable!("text nodes were filtered"),
            };
            let kind = if collapsed && index == 0 {
                RenderTextQueryKind::CaretForOffset {
                    utf16_offset: start_offset.min(text_len),
                    affinity: RenderTextAffinity::Downstream,
                }
            } else {
                RenderTextQueryKind::RangeBoxes {
                    utf16_start: if whole_text {
                        0
                    } else {
                        start_offset.min(text_len)
                    },
                    utf16_end: if whole_text {
                        text_len
                    } else {
                        end_offset.min(text_len)
                    },
                }
            };
            RenderTextQuery {
                query_id: RenderQueryId::new(index as u64 + 1).expect("bounded query id"),
                node_id: node.id,
                kind,
            }
        })
        .collect::<Vec<_>>();
    let batch = RenderTextQueryBatch {
        version: RENDER_PROTOCOL_VERSION,
        context_id: snapshot.revision.context_id,
        document_id: snapshot.revision.document_id,
        commit_id: commit.commit_id,
        revision: commit.revision,
        handle: commit.text_query_handle,
        allow_truncation: false,
        queries,
    };
    let result = match layout
        .config
        .renderer
        .query_text(batch, &layout.cancellation)
    {
        Ok(result) => result,
        Err(error) => return json!({ "ok": false, "message": error.to_string() }),
    };
    let rect = result
        .results
        .iter()
        .flat_map(|result| match &result.value {
            RenderTextQueryValue::Caret { rect, .. } => vec![*rect],
            RenderTextQueryValue::RangeBoxes(boxes) => {
                boxes.iter().map(|text_box| text_box.rect).collect()
            }
            RenderTextQueryValue::Offset { .. } => Vec::new(),
        })
        .reduce(union_render_rect);
    renderer_rect_result(rect, renderer_output_scale(&commit))
}

fn synchronous_layout(
    layout: &SynchronousLayoutHost,
) -> Result<(vixen_api::FullRenderSnapshot, vixen_api::RenderCommit), String> {
    let mutations = layout.mutations.take();
    if !mutations.is_empty()
        && let Err(error) =
            super::apply_dom_mutation_list(&mut layout.config.page.borrow_mut(), mutations)
    {
        return Err(error.to_string());
    }
    let view = layout.config.view.get();
    let snapshot = layout
        .config
        .page
        .borrow()
        .render_snapshot(
            layout.config.context_id,
            layout.config.document_id,
            view.viewport,
            view.viewport_generation.max(1),
            view.device_scale,
            view.page_zoom,
        )
        .map_err(|message| message.to_string())?;
    let commit = layout
        .config
        .renderer
        .ensure_layout(snapshot.clone(), &layout.cancellation)
        .map_err(|error| error.to_string())?;
    layout
        .config
        .page
        .borrow_mut()
        .apply_renderer_scroll(&commit);
    Ok((snapshot, commit))
}

fn render_node_is_descendant(
    snapshot: &vixen_api::FullRenderSnapshot,
    node_id: vixen_api::RenderNodeId,
    ancestor_id: vixen_api::RenderNodeId,
) -> bool {
    let mut current = snapshot
        .nodes
        .iter()
        .find(|node| node.id == node_id)
        .and_then(|node| node.parent_id);
    while let Some(node_id) = current {
        if node_id == ancestor_id {
            return true;
        }
        current = snapshot
            .nodes
            .iter()
            .find(|node| node.id == node_id)
            .and_then(|node| node.parent_id);
    }
    false
}

fn union_render_rect(
    left: vixen_api::RenderRect,
    right: vixen_api::RenderRect,
) -> vixen_api::RenderRect {
    let x = left.x.min(right.x);
    let y = left.y.min(right.y);
    let right_edge = (left.x + left.width).max(right.x + right.width);
    let bottom = (left.y + left.height).max(right.y + right.height);
    vixen_api::RenderRect {
        x,
        y,
        width: right_edge - x,
        height: bottom - y,
    }
}

fn renderer_output_scale(commit: &vixen_api::RenderCommit) -> f64 {
    commit.viewport.device_scale * commit.viewport.page_zoom
}

fn renderer_rect_result(
    rect: Option<vixen_api::RenderRect>,
    output_scale: f64,
) -> deno_core::serde_json::Value {
    json!({ "ok": true, "rendererGeometry": true, "rect": rect.map(|rect| json!({
        "x": rect.x / output_scale,
        "y": rect.y / output_scale,
        "width": rect.width / output_scale,
        "height": rect.height / output_scale,
    })) })
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_image_current_src(
    #[string] src: String,
    #[string] srcset: String,
    #[string] sizes: String,
) -> deno_core::serde_json::Value {
    let viewport = Viewport::new(800.0, 600.0, 1.0);
    json!(select_responsive_image_source(&srcset, &sizes, &viewport).unwrap_or(src))
}

#[deno_core::op2]
#[serde]
fn op_vixen_dom_form_entries(state: &mut OpState, node_id: u32) -> deno_core::serde_json::Value {
    let host = state.borrow::<DomHost>();
    let Some(record) = element_record_by_node_id(&host.0, node_id as usize) else {
        return missing_element_result(node_id);
    };
    let Some(entries) = &record.form_entries else {
        return json!({
            "ok": false,
            "message": format!("Vixen DOM host element is not a form: {node_id}"),
        });
    };
    json!({
        "ok": true,
        "entries": entries.iter().map(form_entry_value).collect::<Vec<_>>(),
    })
}

fn dom_host_state(
    page: &Page,
    mutations: DomMutationSink,
    synchronous_layout: Option<SynchronousLayoutHost>,
) -> Result<DomHostState, String> {
    let elements = page.query_selector_all_in_viewport("*", page.layout_viewport())?;
    let records = elements
        .iter()
        .map(|info| element_record(page, info))
        .collect::<Vec<_>>();
    let document_element_node_id = records
        .iter()
        .find(|record| record.tag.eq_ignore_ascii_case("html"))
        .map(|record| record.node_id);
    let head_node_id = records
        .iter()
        .find(|record| record.tag.eq_ignore_ascii_case("head"))
        .map(|record| record.node_id);
    let body_node_id = records
        .iter()
        .find(|record| record.tag.eq_ignore_ascii_case("body"))
        .map(|record| record.node_id);
    let forms = collection_node_ids(page, "form")?;
    let images = collection_node_ids(page, "img")?;
    let links = collection_node_ids(page, "a[href], area[href]")?;
    let scripts = collection_node_ids(page, "script")?;
    let (scroll_x, scroll_y) = page.root_scroll();
    let (scroll_max_x, scroll_max_y) = page.root_scroll_max();
    let (viewport_width, viewport_height) = page.layout_viewport();

    Ok(DomHostState {
        snapshot: json!({
            "title": page.document().title().unwrap_or_default(),
            "url": page.url(),
            "baseURI": document_base_uri(page)?,
            "bodyTextContent": page.document().body_text_content(),
            "documentElementNodeId": document_element_node_id,
            "headNodeId": head_node_id,
            "bodyNodeId": body_node_id,
            "activeElementNodeId": page.focused_element_node_id().or(body_node_id),
            "selection": page.selection().map(|selection| json!({
                "anchorNodeId": selection.anchor_node_id,
                "anchorOffset": selection.anchor_offset,
                "focusNodeId": selection.focus_node_id,
                "focusOffset": selection.focus_offset,
            })),
            "scrollingElementNodeId": document_element_node_id,
            "scrollX": scroll_x,
            "scrollY": scroll_y,
            "scrollMaxX": scroll_max_x,
            "scrollMaxY": scroll_max_y,
            "viewportWidth": viewport_width,
            "viewportHeight": viewport_height,
            "historyLength": page.session_history().length(),
            "historyIndex": page.session_history().index(),
            "historyStateJson": page.history_state_json(),
            "historyScrollRestoration": page.session_history().scroll_restoration().to_keyword(),
            "collections": {
                "forms": forms,
                "images": images,
                "links": links,
                "scripts": scripts,
            },
        }),
        elements: records,
        text_overrides: Mutex::new(HashMap::new()),
        mutations,
        synchronous_layout,
    })
}

fn element_record(page: &Page, info: &ElementInfo) -> DomElementRecord {
    let text_content = page
        .document()
        .element_text_content(info.node_id)
        .unwrap_or_else(|| info.text.clone());

    let styles = page.computed_style_for_viewport(info.node_id, page.layout_viewport());
    let overflow = styles
        .iter()
        .find(|(name, _)| name == "overflow" || name == "overflow-y")
        .map(|(_, value)| value.to_ascii_lowercase());
    let overflow_clips = overflow
        .as_deref()
        .is_some_and(|value| matches!(value, "auto" | "scroll" | "hidden" | "clip"));
    let user_scrollable = overflow
        .as_deref()
        .is_some_and(|value| matches!(value, "auto" | "scroll"));
    let fixed_position = styles
        .iter()
        .any(|(name, value)| name == "position" && value.eq_ignore_ascii_case("fixed"));
    DomElementRecord {
        node_id: info.node_id,
        tag: info.tag.clone(),
        id: info.id.clone(),
        classes: info.classes.clone(),
        attributes: info.attributes.clone(),
        text_content,
        inner_html: page
            .document()
            .element_inner_html(info.node_id)
            .unwrap_or_default(),
        outer_html: page
            .document()
            .element_outer_html(info.node_id)
            .unwrap_or_default(),
        bbox: info.bbox,
        scroll_position: (0.0, 0.0),
        scroll_max: (0.0, 0.0),
        user_scrollable,
        overflow_clips,
        fixed_position,
        form_entries: form_entries_for_element(page, info),
        class_tokens: dom_token_list(info, "class"),
        rel_tokens: dom_token_list(info, "rel"),
        sandbox_tokens: dom_token_list(info, "sandbox"),
        dataset: dataset_pairs(info),
        parent_node_id: related_node_id(page, info.node_id, ElementRelation::Parent),
        first_element_child_node_id: related_node_id(
            page,
            info.node_id,
            ElementRelation::FirstChild,
        ),
        last_element_child_node_id: related_node_id(page, info.node_id, ElementRelation::LastChild),
        previous_element_sibling_node_id: related_node_id(
            page,
            info.node_id,
            ElementRelation::PreviousSibling,
        ),
        next_element_sibling_node_id: related_node_id(
            page,
            info.node_id,
            ElementRelation::NextSibling,
        ),
        child_element_node_ids: child_element_node_ids(page, info.node_id),
    }
}

fn form_entries_for_element(page: &Page, info: &ElementInfo) -> Option<Vec<FormEntry>> {
    if !info.tag.eq_ignore_ascii_case("form") {
        return None;
    }
    page.form_submission_by_node_id(info.node_id, None)
        .ok()
        .map(|submission| submission.entries)
}

fn form_entry_value(entry: &FormEntry) -> deno_core::serde_json::Value {
    match &entry.value {
        FormEntryValue::Text(value) => json!({
            "name": &entry.name,
            "kind": "text",
            "value": value,
        }),
        FormEntryValue::File {
            filename,
            content_type,
            body,
        } => json!({
            "name": &entry.name,
            "kind": "file",
            "filename": filename,
            "type": content_type,
            "size": body.len(),
        }),
    }
}

fn element_record_value(record: &DomElementRecord) -> deno_core::serde_json::Value {
    json!({
        "nodeId": record.node_id,
        "tag": &record.tag,
        "id": &record.id,
        "className": record.classes.join(" "),
        "attributes": &record.attributes,
        "textContent": &record.text_content,
        "innerHTML": &record.inner_html,
        "outerHTML": &record.outer_html,
        "bbox": record.bbox.map(rect_value),
        "scrollLeft": record.scroll_position.0,
        "scrollTop": record.scroll_position.1,
        "scrollOriginLeft": record.scroll_position.0,
        "scrollOriginTop": record.scroll_position.1,
        "scrollMaxX": record.scroll_max.0,
        "scrollMaxY": record.scroll_max.1,
        "userScrollable": record.user_scrollable,
        "overflowClips": record.overflow_clips,
        "fixedPosition": record.fixed_position,
        "parentNodeId": record.parent_node_id,
        "childNodeIds": &record.child_element_node_ids,
        "previousSiblingNodeId": record.previous_element_sibling_node_id,
        "nextSiblingNodeId": record.next_element_sibling_node_id,
        "firstElementChildNodeId": record.first_element_child_node_id,
        "lastElementChildNodeId": record.last_element_child_node_id,
        "previousElementSiblingNodeId": record.previous_element_sibling_node_id,
        "nextElementSiblingNodeId": record.next_element_sibling_node_id,
        "childElementNodeIds": &record.child_element_node_ids,
    })
}

fn related_node_id(page: &Page, node_id: usize, relation: ElementRelation) -> Option<usize> {
    page.document()
        .related_element_by_node_id(node_id, relation)
        .map(|element| element.into_element_info().node_id)
}

fn child_element_node_ids(page: &Page, node_id: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut child = related_node_id(page, node_id, ElementRelation::FirstChild);
    while let Some(child_node_id) = child {
        out.push(child_node_id);
        child = related_node_id(page, child_node_id, ElementRelation::NextSibling);
    }
    out
}

fn collection_node_ids(page: &Page, selector: &str) -> Result<Vec<usize>, String> {
    Ok(page
        .query_selector_all(selector)?
        .into_iter()
        .map(|info| info.node_id)
        .collect())
}

fn document_base_uri(page: &Page) -> Result<String, String> {
    let Some(base) = page
        .query_selector_all("base[href]")?
        .into_iter()
        .next()
        .and_then(|info| element_attr(&info, "href").map(ToOwned::to_owned))
    else {
        return Ok(page.url().to_owned());
    };
    Ok(resolve_url_string(&base, page.url()).unwrap_or(base))
}

fn resolve_url_string(input: &str, base: &str) -> Option<String> {
    let base = url::Url::parse(base).ok()?;
    base.join(input).ok().map(|url| url.to_string())
}

fn rect_value((x, y, width, height): (f64, f64, f64, f64)) -> deno_core::serde_json::Value {
    json!({
        "x": x,
        "y": y,
        "width": width,
        "height": height,
    })
}

fn missing_element_result(node_id: u32) -> deno_core::serde_json::Value {
    json!({
        "ok": false,
        "message": format!("Vixen DOM host element is unavailable: {node_id}"),
    })
}

fn record_attr(record: &DomElementRecord, name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    record
        .attributes
        .iter()
        .find(|(attr, _)| attr == name || attr == &lower)
        .map(|(_, value)| value.clone())
}

fn record_text_content(host: &DomHostState, record: &DomElementRecord) -> String {
    host.text_overrides
        .lock()
        .expect("DOM text override map poisoned")
        .get(&record.node_id)
        .cloned()
        .unwrap_or_else(|| record.text_content.clone())
}

fn record_tokens(record: &DomElementRecord, attribute: &str) -> Result<Vec<String>, String> {
    match attribute {
        "class" => Ok(record.class_tokens.clone()),
        "rel" => Ok(record.rel_tokens.clone()),
        "sandbox" => Ok(record.sandbox_tokens.clone()),
        _ => Err(format!("unsupported DOMTokenList attribute: {attribute}")),
    }
}

fn element_record_by_node_id(host: &DomHostState, node_id: usize) -> Option<&DomElementRecord> {
    host.elements
        .iter()
        .find(|record| record.node_id == node_id)
}

fn query_selector_node_ids(host: &DomHostState, selector: &str) -> Result<Vec<usize>, String> {
    let parsed = parse_simple_selector_list(selector)?;
    Ok(host
        .elements
        .iter()
        .filter(|record| record_matches_any(record, &parsed))
        .map(|record| record.node_id)
        .collect())
}

fn parse_simple_selector_list(selector: &str) -> Result<Vec<SimpleSelector>, String> {
    let mut selectors = Vec::new();
    for raw in selector.split(',') {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("Vixen DOM host selector list contains an empty selector".to_owned());
        }
        selectors.push(parse_simple_selector(raw)?);
    }
    Ok(selectors)
}

fn parse_simple_selector(raw: &str) -> Result<SimpleSelector, String> {
    if raw.contains(char::is_whitespace)
        || raw.contains('>')
        || raw.contains('+')
        || raw.contains('~')
        || raw.contains(':')
    {
        return Err(unsupported_selector(raw));
    }

    let mut rest = raw;
    let mut tag = None;
    let mut id = None;
    let mut classes = Vec::new();
    let mut attributes = Vec::new();

    if let Some(stripped) = rest.strip_prefix('*') {
        rest = stripped;
    } else if rest
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic())
    {
        let (name, stripped) = take_simple_name(rest).ok_or_else(|| unsupported_selector(raw))?;
        tag = Some(name.to_ascii_lowercase());
        rest = stripped;
    }

    while !rest.is_empty() {
        if let Some(stripped) = rest.strip_prefix('#') {
            let (name, next) =
                take_simple_name(stripped).ok_or_else(|| unsupported_selector(raw))?;
            id = Some(name.to_owned());
            rest = next;
        } else if let Some(stripped) = rest.strip_prefix('.') {
            let (name, next) =
                take_simple_name(stripped).ok_or_else(|| unsupported_selector(raw))?;
            classes.push(name.to_owned());
            rest = next;
        } else if let Some(stripped) = rest.strip_prefix('[') {
            let Some((body, next)) = stripped.split_once(']') else {
                return Err(unsupported_selector(raw));
            };
            attributes.push(
                parse_attribute_selector(body.trim()).ok_or_else(|| unsupported_selector(raw))?,
            );
            rest = next;
        } else {
            return Err(unsupported_selector(raw));
        }
    }

    if tag.is_none() && id.is_none() && classes.is_empty() && attributes.is_empty() && raw != "*" {
        return Err(unsupported_selector(raw));
    }

    Ok(SimpleSelector {
        tag,
        id,
        classes,
        attributes,
    })
}

fn unsupported_selector(raw: &str) -> String {
    format!(
        "Vixen DOM host currently supports selector lists of tag, #id, .class, [attr], and [attr='value'] compounds: {raw}"
    )
}

fn take_simple_name(input: &str) -> Option<(&str, &str)> {
    let mut end = 0;
    for (index, byte) in input.bytes().enumerate() {
        let valid = if index == 0 {
            byte.is_ascii_alphabetic() || byte == b'_'
        } else {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
        };
        if !valid {
            break;
        }
        end = index + 1;
    }
    if end == 0 {
        return None;
    }
    Some((&input[..end], &input[end..]))
}

fn parse_attribute_selector(input: &str) -> Option<SimpleAttributeSelector> {
    if let Some((name, value)) = input.split_once('=') {
        let name = name.trim();
        if !is_simple_attr_name(name) {
            return None;
        }
        let value = unquote_attr_value(value.trim())?;
        return Some(SimpleAttributeSelector::Equals(
            name.to_ascii_lowercase(),
            value.to_owned(),
        ));
    }
    if !is_simple_attr_name(input) {
        return None;
    }
    Some(SimpleAttributeSelector::Exists(input.to_ascii_lowercase()))
}

fn is_simple_attr_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':'))
}

fn unquote_attr_value(value: &str) -> Option<&str> {
    if let Some(value) = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        return Some(value);
    }
    if let Some(value) = value
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
    {
        return Some(value);
    }
    if value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.' | b'/')
    }) {
        return Some(value);
    }
    None
}

fn record_matches_any(record: &DomElementRecord, selectors: &[SimpleSelector]) -> bool {
    selectors
        .iter()
        .any(|selector| record_matches(record, selector))
}

fn record_matches(record: &DomElementRecord, selector: &SimpleSelector) -> bool {
    if let Some(tag) = &selector.tag
        && !record.tag.eq_ignore_ascii_case(tag)
    {
        return false;
    }
    if let Some(id) = &selector.id
        && record.id.as_deref() != Some(id.as_str())
    {
        return false;
    }
    if !selector
        .classes
        .iter()
        .all(|class| record.classes.iter().any(|name| name == class))
    {
        return false;
    }
    selector.attributes.iter().all(|attribute| match attribute {
        SimpleAttributeSelector::Exists(name) => record_attr(record, name).is_some(),
        SimpleAttributeSelector::Equals(name, value) => {
            record_attr(record, name).as_deref() == Some(value.as_str())
        }
    })
}

fn dom_token_list(info: &ElementInfo, attribute: &str) -> Vec<String> {
    let list = DomTokenList::parse(element_attr(info, attribute).unwrap_or_default());
    list.as_vec().to_vec()
}

fn dataset_pairs(info: &ElementInfo) -> Vec<(String, String)> {
    collect_dataset(
        info.attributes
            .iter()
            .map(|(name, value)| (name.as_str(), value.clone())),
    )
}

fn element_attr<'a>(info: &'a ElementInfo, name: &str) -> Option<&'a str> {
    info.attributes
        .iter()
        .find(|(attr_name, _)| attr_name == name)
        .map(|(_, value)| value.as_str())
}

const DOM_API_BOOTSTRAP: &str = r#"
(() => {
  const {
    op_vixen_dom_snapshot,
    op_vixen_dom_element_snapshot,
    op_vixen_dom_query_selector_all,
    op_vixen_dom_get_element_by_id,
    op_vixen_dom_element_matches,
    op_vixen_dom_element_text,
    op_vixen_dom_element_attribute,
    op_vixen_dom_element_tokens,
    op_vixen_dom_element_dataset,
    op_vixen_dom_element_rect,
    op_vixen_dom_range_rect,
    op_vixen_dom_image_current_src,
    op_vixen_dom_form_entries,
    op_vixen_dom_set_document_title,
    op_vixen_dom_set_element_text,
    op_vixen_dom_set_element_attr,
    op_vixen_dom_remove_element_attr,
    op_vixen_dom_set_element_inner_html,
    op_vixen_dom_set_control_value,
    op_vixen_dom_set_control_selection,
    op_vixen_dom_set_contenteditable_state,
    op_vixen_dom_set_focused_element,
    op_vixen_dom_set_selection,
    op_vixen_dom_scroll_state,
    op_vixen_dom_set_root_scroll,
    op_vixen_dom_set_element_scroll,
    op_vixen_document_cookie_get,
    op_vixen_document_cookie_set,
  } = Deno.core.ops;
  const webidl = globalThis.__vixenWebidl;
  const data = op_vixen_dom_snapshot();
  const elementObjects = new Map();
  const nodeObjects = new Map();
  let knownElementNodeIds = null;
  let nextLocalNodeId = -1;
  const htmlElementInterfaceByTag = new Map([
    ['html', 'HTMLHtmlElement'], ['head', 'HTMLHeadElement'], ['body', 'HTMLBodyElement'],
    ['title', 'HTMLTitleElement'], ['meta', 'HTMLMetaElement'], ['base', 'HTMLBaseElement'],
    ['link', 'HTMLLinkElement'], ['style', 'HTMLStyleElement'], ['script', 'HTMLScriptElement'],
    ['template', 'HTMLTemplateElement'], ['slot', 'HTMLSlotElement'], ['div', 'HTMLDivElement'],
    ['span', 'HTMLSpanElement'], ['p', 'HTMLParagraphElement'], ['h1', 'HTMLHeadingElement'],
    ['h2', 'HTMLHeadingElement'], ['h3', 'HTMLHeadingElement'], ['h4', 'HTMLHeadingElement'],
    ['h5', 'HTMLHeadingElement'], ['h6', 'HTMLHeadingElement'], ['pre', 'HTMLPreElement'],
    ['blockquote', 'HTMLQuoteElement'], ['q', 'HTMLQuoteElement'], ['ol', 'HTMLOListElement'],
    ['ul', 'HTMLUListElement'], ['li', 'HTMLLIElement'], ['a', 'HTMLAnchorElement'],
    ['area', 'HTMLAreaElement'], ['br', 'HTMLBRElement'], ['hr', 'HTMLHRElement'],
    ['dl', 'HTMLDListElement'], ['data', 'HTMLDataElement'], ['time', 'HTMLTimeElement'],
    ['ins', 'HTMLModElement'], ['del', 'HTMLModElement'], ['img', 'HTMLImageElement'],
    ['picture', 'HTMLPictureElement'], ['source', 'HTMLSourceElement'], ['audio', 'HTMLAudioElement'],
    ['video', 'HTMLVideoElement'], ['track', 'HTMLTrackElement'], ['iframe', 'HTMLIFrameElement'],
    ['embed', 'HTMLEmbedElement'], ['object', 'HTMLObjectElement'], ['param', 'HTMLParamElement'],
    ['canvas', 'HTMLCanvasElement'], ['table', 'HTMLTableElement'], ['caption', 'HTMLTableCaptionElement'],
    ['col', 'HTMLTableColElement'], ['colgroup', 'HTMLTableColElement'], ['tbody', 'HTMLTableSectionElement'],
    ['thead', 'HTMLTableSectionElement'], ['tfoot', 'HTMLTableSectionElement'], ['tr', 'HTMLTableRowElement'],
    ['td', 'HTMLTableCellElement'], ['th', 'HTMLTableCellElement'], ['form', 'HTMLFormElement'],
    ['label', 'HTMLLabelElement'], ['input', 'HTMLInputElement'], ['button', 'HTMLButtonElement'],
    ['select', 'HTMLSelectElement'], ['datalist', 'HTMLDataListElement'], ['optgroup', 'HTMLOptGroupElement'],
    ['option', 'HTMLOptionElement'], ['textarea', 'HTMLTextAreaElement'], ['progress', 'HTMLProgressElement'],
    ['meter', 'HTMLMeterElement'], ['fieldset', 'HTMLFieldSetElement'], ['legend', 'HTMLLegendElement'],
    ['output', 'HTMLOutputElement'], ['details', 'HTMLDetailsElement'], ['dialog', 'HTMLDialogElement'],
    ['menu', 'HTMLMenuElement'],
  ]);
  let currentUrl = String(data.url || 'about:blank');
  let historyLength = Math.max(1, Number(data.historyLength) || 1);
  let historyIndex = Math.min(historyLength - 1, Math.max(0, Number(data.historyIndex) || 0));
  let historyState = parseHistoryState(data.historyStateJson);
  let historyScrollRestoration = data.historyScrollRestoration === 'manual' ? 'manual' : 'auto';
  let topLevelScrollX = Math.max(0, Number(data.scrollX) || 0);
  let topLevelScrollY = Math.max(0, Number(data.scrollY) || 0);
  const initialTopLevelScrollX = topLevelScrollX;
  const initialTopLevelScrollY = topLevelScrollY;
  let topLevelScrollMaxX = Math.max(0, Number(data.scrollMaxX) || 0);
  let topLevelScrollMaxY = Math.max(0, Number(data.scrollMaxY) || 0);
  let rootScrollEventQueued = false;
  const elementScrollEventsQueued = new Set();
  const navigationActions = [];
  const maxNavigationActions = 64;

  function parseHistoryState(value) {
    if (value === null || value === undefined) return null;
    try { return JSON.parse(String(value)); } catch (_) { return null; }
  }

  function cloneHistoryState(value) {
    if (value === undefined) return null;
    if (typeof structuredClone === 'function') return structuredClone(value);
    return parseHistoryState(JSON.stringify(value));
  }

  function historyStateJson(value) {
    const json = JSON.stringify(value === undefined ? null : value);
    return json === undefined ? 'null' : json;
  }

  function resolveNavigationUrl(input, base = currentUrl) {
    return new URL(String(input), base || data.baseURI || currentUrl).href;
  }

  function assertSameOriginNavigation(url) {
    const current = new URL(currentUrl);
    const next = new URL(url);
    if (current.protocol === 'file:' && next.protocol === 'file:') return;
    if (current.origin !== next.origin) throw new TypeError('history state URL must be same-origin');
  }

  function queueNavigationAction(action) {
    if (navigationActions.length < maxNavigationActions) {
      navigationActions.push(action);
    } else if (navigationActions.length === maxNavigationActions) {
      navigationActions.push({ type: 'overflow' });
    }
  }

  function unwrapDomOp(result) {
    if (!result.ok) throw new TypeError(result.message);
    return result;
  }

  function documentCookie() {
    return unwrapDomOp(op_vixen_document_cookie_get(currentUrl)).value || '';
  }

  function setDocumentCookie(value) {
    unwrapDomOp(op_vixen_document_cookie_set(currentUrl, String(value)));
  }

  function knownElementIds() {
    if (knownElementNodeIds === null) {
      knownElementNodeIds = unwrapDomOp(op_vixen_dom_query_selector_all('*')).nodeIds.slice();
    }
    return knownElementNodeIds;
  }

  function rememberElementId(nodeId) {
    if (nodeId <= 0) return;
    const ids = knownElementIds();
    if (!ids.includes(nodeId)) ids.push(nodeId);
  }

  function nodeRecord(node) {
    return node && Object.prototype.hasOwnProperty.call(node, '__vixenRecord')
      ? node.__vixenRecord
      : null;
  }

  function recordForElementNodeId(nodeId) {
    const element = wrapElementByNodeId(nodeId);
    return element === null ? null : elementRecord(element);
  }

  function findAllNodeIds(selector) {
    const raw = String(selector);
    let parsed;
    try {
      parsed = parseSimpleSelectorList(raw);
    } catch (_) {
      return unwrapDomOp(op_vixen_dom_query_selector_all(raw)).nodeIds;
    }
    const ids = knownElementIds()
      .concat([...elementObjects.keys()].filter((nodeId) => nodeId < 0));
    const out = [];
    for (const nodeId of ids) {
      const record = recordForElementNodeId(nodeId);
      if (record && record.isConnected !== false && recordMatchesAny(record, parsed) && !out.includes(nodeId)) {
        out.push(nodeId);
      }
    }
    return out;
  }

  function elementMatches(nodeId, selector) {
    const raw = String(selector);
    const record = recordForElementNodeId(nodeId);
    if (!record) return false;
    try {
      return recordMatchesAny(record, parseSimpleSelectorList(raw));
    } catch (_) {
      return unwrapDomOp(op_vixen_dom_element_matches(nodeId, raw)).matches;
    }
  }

  function elementText(nodeId) {
    const record = recordForElementNodeId(nodeId);
    if (record) return record.textContent || '';
    return unwrapDomOp(op_vixen_dom_element_text(nodeId)).value;
  }

  function escapeTextForHtml(value) {
    return String(value)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;');
  }

  function decodeBasicHtmlText(value) {
    return String(value)
      .replace(/&lt;/g, '<')
      .replace(/&gt;/g, '>')
      .replace(/&quot;/g, '"')
      .replace(/&#39;/g, "'")
      .replace(/&nbsp;/g, '\u00a0')
      .replace(/&amp;/g, '&');
  }

  function topLevelTextFromHtml(html) {
    const input = String(html || '');
    let text = '';
    let depth = 0;
    for (let i = 0; i < input.length;) {
      if (input.startsWith('<!--', i)) {
        const end = input.indexOf('-->', i + 4);
        i = end === -1 ? input.length : end + 3;
        continue;
      }
      if (input[i] === '<') {
        const end = input.indexOf('>', i + 1);
        if (end === -1) break;
        const rawTag = input.slice(i + 1, end).trim();
        const closing = rawTag.startsWith('/');
        const nameStart = closing ? 1 : 0;
        const match = /^[a-zA-Z][a-zA-Z0-9-]*/.exec(rawTag.slice(nameStart).trimStart());
        const tag = match ? match[0].toLowerCase() : '';
        const selfClosing = rawTag.endsWith('/') || voidElements.has(tag);
        if (closing) depth = Math.max(0, depth - 1);
        else if (tag && !selfClosing) depth += 1;
        i = end + 1;
        continue;
      }
      if (depth === 0) text += input[i];
      i += 1;
    }
    return decodeBasicHtmlText(text);
  }

  function syntheticTextForRecord(record) {
    const text = String(record.textContent || '');
    if (String(record.tag || '').toLowerCase() !== 'label') return text;
    if (!record.childElementNodeIds || record.childElementNodeIds.length === 0) return text;
    return topLevelTextFromHtml(record.innerHTML || '');
  }

  function setElementText(nodeId, value) {
    const text = String(value);
    const record = recordForElementNodeId(nodeId);
    if (!record) throw new TypeError('Vixen DOM host element is unavailable: ' + nodeId);
    const oldSerialized = serializeElementRecord(record);
    const oldText = record.textContent || '';
    const removed = nodesFromIds((record.childNodeIds || record.childElementNodeIds || []).slice());
    record.textContent = text;
    record.innerHTML = escapeTextForHtml(text);
    record.childNodeIds = [];
    record.childElementNodeIds = [];
    record.firstElementChildNodeId = null;
    record.lastElementChildNodeId = null;
    const parent = parentElementRecord(record);
    if (parent) parent.textContent = replaceFirst(parent.textContent || '', oldText, text);
    queueChildListMutation(wrapElementByNodeId(nodeId), [], removed);
    propagateSerializedChange(record, oldSerialized);
    if (nodeId > 0) {
      unwrapDomOp(op_vixen_dom_set_element_text(nodeId, text));
    } else {
      commitNearestConnectedAncestor(record);
    }
    return text;
  }

  function elementAttribute(nodeId, name) {
    const record = recordForElementNodeId(nodeId);
    if (!record) return unwrapDomOp(op_vixen_dom_element_attribute(nodeId, String(name))).value;
    return recordAttr(record, name);
  }

  function imageCurrentSrc(element) {
    const nodeId = element.__vixenNodeId;
    const src = elementAttribute(nodeId, 'src') || '';
    if (elementTag(element) !== 'img') return src;
    const srcset = elementAttribute(nodeId, 'srcset') || '';
    if (!srcset) return src;
    return op_vixen_dom_image_current_src(src, srcset, elementAttribute(nodeId, 'sizes') || '');
  }

  function elementTokens(nodeId, attribute) {
    const value = elementAttribute(nodeId, attribute) || '';
    return parseTokenSet(value);
  }

  function elementDataset(nodeId) {
    const record = recordForElementNodeId(nodeId);
    if (!record) return unwrapDomOp(op_vixen_dom_element_dataset(nodeId)).pairs;
    const pairs = [];
    for (const [name, value] of record.attributes) {
      if (!name.startsWith('data-') || name.length <= 5) continue;
      const prop = name.slice(5).replace(/-([a-z])/g, (_, ch) => ch.toUpperCase());
      if (prop && !pairs.some(([existing]) => existing === prop)) pairs.push([prop, value]);
    }
    return pairs;
  }

  function elementRect(nodeId) {
    const record = recordForElementNodeId(nodeId);
    if (record && Number(nodeId) < 0) return normalizedElementRect(record, { x: 0, y: 0, width: 0, height: 0 });
    syncRendererScrollState(false);
    const result = unwrapDomOp(op_vixen_dom_element_rect(nodeId));
    if (result.rendererGeometry === true) return normalizedElementRect(record, result.rect);
    if (record && Object.prototype.hasOwnProperty.call(record, 'bbox')) return liveElementRect(record);
    return normalizedElementRect(record, result.rect);
  }

  function liveElementRect(record) {
    const raw = normalizedElementRect(record, record.bbox);
    if (!raw) return raw;
    let x = Number(raw.x) || 0;
    let y = Number(raw.y) || 0;
    let fixedSubtree = record.fixedPosition === true;
    let parentId = record.parentNodeId;
    while (Number.isInteger(parentId) && !fixedSubtree) {
      const parent = recordForElementNodeId(parentId);
      if (!parent) break;
      x -= (Number(parent.scrollLeft) || 0) - (Number(parent.scrollOriginLeft) || 0);
      y -= (Number(parent.scrollTop) || 0) - (Number(parent.scrollOriginTop) || 0);
      fixedSubtree = parent.fixedPosition === true;
      parentId = parent.parentNodeId;
    }
    if (!fixedSubtree) {
      x -= topLevelScrollX - initialTopLevelScrollX;
      y -= topLevelScrollY - initialTopLevelScrollY;
    }
    return { x, y, width: Number(raw.width) || 0, height: Number(raw.height) || 0 };
  }

  function normalizedElementRect(record, rect) {
    if (!rect) return rect;
    const width = Number(rect.width) || 0;
    const height = Number(rect.height) || 0;
    if (width > 0 && height > 0) return rect;
    if (!record) return rect;
    const tag = String(record.tag).toLowerCase();
    let fallback = null;
    if (tag === 'input') {
      const type = String(recordAttr(record, 'type') || 'text').toLowerCase();
      if (type === 'checkbox' || type === 'radio') fallback = { width: 13, height: 13 };
      else if (type === 'color') fallback = { width: 44, height: 23 };
      else fallback = { width: 150, height: 20 };
    }
    else if (tag === 'textarea') fallback = { width: 200, height: 60 };
    else if (tag === 'select') fallback = { width: 120, height: 20 };
    else if (tag === 'button') fallback = { width: 64, height: 24 };
    if (!fallback) return rect;
    return {
      x: Number(rect.x) || 0,
      y: Number(rect.y) || 0,
      width: width > 0 ? width : fallback.width,
      height: height > 0 ? height : fallback.height,
    };
  }

  function rectContainsPoint(rect, x, y) {
    return rect && Number.isFinite(rect.x) && Number.isFinite(rect.y)
      && Number.isFinite(rect.width) && Number.isFinite(rect.height)
      && rect.width > 0 && rect.height > 0
      && x >= rect.x && y >= rect.y && x < rect.x + rect.width && y < rect.y + rect.height;
  }

  function hitTestPriority(nodeId) {
    const record = recordForElementNodeId(nodeId);
    if (!record) return 0;
    const tag = String(record.tag || '').toLowerCase();
    if (tag === 'button' || tag === 'input' || tag === 'select' || tag === 'textarea') return 2;
    return 1;
  }

  function hitTestElementIds(x, y) {
    const px = Number(x);
    const py = Number(y);
    if (!Number.isFinite(px) || !Number.isFinite(py)) return [];
    if (px < 0 || py < 0 || px >= globalThis.innerWidth || py >= globalThis.innerHeight) return [];
    const hits = [];
    for (const nodeId of knownElementIds()) {
      const record = recordForElementNodeId(nodeId);
      if (!record || record.isConnected === false) continue;
      if (!rectContainsPoint(elementRect(nodeId), px, py)) continue;
      let parentId = record.parentNodeId;
      let clipped = false;
      while (Number.isInteger(parentId)) {
        const parent = recordForElementNodeId(parentId);
        if (!parent) break;
        if (parent.overflowClips && !rectContainsPoint(elementRect(parentId), px, py)) {
          clipped = true;
          break;
        }
        parentId = parent.parentNodeId;
      }
      if (!clipped) hits.push(nodeId);
    }
    return hits.reverse().sort((a, b) => hitTestPriority(b) - hitTestPriority(a));
  }

  function formEntries(nodeId) {
    const form = wrapElementByNodeId(Number(nodeId));
    if (!form || String(elementRecord(form).tag).toLowerCase() !== 'form') {
      return unwrapDomOp(op_vixen_dom_form_entries(nodeId)).entries;
    }
    const entries = [];
    collectLiveFormEntries(form, false, entries);
    return entries;
  }

  function normalizeAttributeName(name) {
    const value = String(name).trim();
    if (value === '' || /[\t\n\f\r\u0020\0"'>/=]/.test(value)) {
      throw new TypeError('Invalid attribute name: ' + value);
    }
    return value.toLowerCase();
  }

  function recordAttr(record, name) {
    const raw = String(name);
    const lower = raw.toLowerCase();
    const pair = record.attributes.find(([attr]) => attr === raw || attr === lower);
    return pair ? pair[1] : null;
  }

  function queueDomMutation(record) {
    const hook = globalThis.__vixenQueueMutationRecord;
    if (typeof hook === 'function') hook(record);
  }

  function nodesFromIds(ids) {
    return (ids || []).map(wrapNodeById).filter((node) => node !== null);
  }

  function queueAttributeMutation(element, name, oldValue) {
    queueDomMutation({
      type: 'attributes',
      target: element,
      addedNodes: [],
      removedNodes: [],
      previousSibling: null,
      nextSibling: null,
      attributeName: name,
      attributeNamespace: null,
      oldValue,
    });
  }

  function queueChildListMutation(parent, addedNodes, removedNodes, previousSibling = null, nextSibling = null) {
    queueDomMutation({
      type: 'childList',
      target: parent,
      addedNodes: addedNodes || [],
      removedNodes: removedNodes || [],
      previousSibling,
      nextSibling,
      attributeName: null,
      attributeNamespace: null,
      oldValue: null,
    });
  }

  function queueCharacterDataMutation(node, oldValue) {
    queueDomMutation({
      type: 'characterData',
      target: node,
      addedNodes: [],
      removedNodes: [],
      previousSibling: null,
      nextSibling: null,
      attributeName: null,
      attributeNamespace: null,
      oldValue,
    });
  }

  function setRecordAttr(record, name, value) {
    const attrName = normalizeAttributeName(name);
    const attrValue = String(value);
    const index = record.attributes.findIndex(([attr]) => attr === attrName);
    if (index === -1) record.attributes.push([attrName, attrValue]);
    else record.attributes[index] = [attrName, attrValue];
    if (attrName === 'id') record.id = attrValue || null;
    if (attrName === 'class') {
      record.classes = attrValue.split(/[\t\n\f\r ]+/).filter((token) => token !== '');
      record.className = attrValue;
    }
    if (attrName === 'value' && String(record.tag).toLowerCase() === 'input') {
      record.defaultValue = attrValue;
      if (!record.__vixenValueDirty) {
        record.value = attrValue;
        record.selectionStart = attrValue.length;
        record.selectionEnd = attrValue.length;
      }
    }
    invalidateElementCaches(record.nodeId);
    return attrName;
  }

  function removeRecordAttr(record, name) {
    const attrName = normalizeAttributeName(name);
    record.attributes = record.attributes.filter(([attr]) => attr !== attrName);
    if (attrName === 'id') record.id = null;
    if (attrName === 'class') {
      record.classes = [];
      record.className = '';
    }
    if (attrName === 'value' && String(record.tag).toLowerCase() === 'input') {
      record.defaultValue = '';
      if (!record.__vixenValueDirty) {
        record.value = '';
        record.selectionStart = 0;
        record.selectionEnd = 0;
      }
    }
    invalidateElementCaches(record.nodeId);
    return attrName;
  }

  function invalidateElementCaches(nodeId) {
    const element = elementObjects.get(nodeId);
    if (!element) return;
    for (const key of ['__vixenAttributes', '__vixenClassList', '__vixenRelList', '__vixenSandboxList', '__vixenDataset', '__vixenStyle']) {
      if (Object.prototype.hasOwnProperty.call(element, key)) delete element[key];
    }
  }

  function parseTokenSet(input) {
    const out = [];
    for (const token of String(input).split(/[\t\n\f\r ]+/)) {
      if (token !== '' && !out.includes(token)) out.push(token);
    }
    return out;
  }

  function serializeTokenSet(tokens) {
    return tokens.join(' ');
  }

  function escapeAttributeForHtml(value) {
    return String(value)
      .replace(/&/g, '&amp;')
      .replace(/"/g, '&quot;')
      .replace(/\u00a0/g, '\u00a0');
  }

  const voidElements = new Set(['area', 'base', 'br', 'col', 'embed', 'hr', 'img', 'input', 'link', 'meta', 'param', 'source', 'track', 'wbr']);

  function serializeElementRecord(record) {
    let html = '<' + record.tag;
    for (const [name, value] of record.attributes) {
      html += ' ' + name + '="' + escapeAttributeForHtml(value) + '"';
    }
    html += '>';
    if (voidElements.has(String(record.tag).toLowerCase())) return html;
    const htmlChildren = record.innerHTML || ((record.childNodeIds || []).length === 0 ? escapeTextForHtml(record.textContent || '') : '');
    return html + htmlChildren + '</' + record.tag + '>';
  }

  function serializeNodeObject(node) {
    if (node instanceof VixenText) return escapeTextForHtml(node.data);
    const record = nodeRecord(node);
    if (!record) throw new TypeError('Expected a Vixen Node');
    return serializeElementRecord(record);
  }

  function textContentOfNode(node) {
    if (node instanceof VixenText) return node.data;
    const record = nodeRecord(node);
    return record ? record.textContent || '' : '';
  }

  function replaceFirst(source, before, after) {
    const index = String(source).indexOf(String(before));
    if (index === -1) return String(source);
    return String(source).slice(0, index) + String(after) + String(source).slice(index + String(before).length);
  }

  function parentElementRecord(record) {
    if (record.parentNodeId === null || record.parentNodeId === undefined) return null;
    return recordForElementNodeId(record.parentNodeId);
  }

  function propagateSerializedChange(record, oldSerialized) {
    const parent = parentElementRecord(record);
    if (!parent) return;
    const parentOld = serializeElementRecord(parent);
    const currentNode = nodeObjects.get(record.nodeId) || elementObjects.get(record.nodeId);
    parent.innerHTML = replaceFirst(parent.innerHTML || '', oldSerialized, serializeNodeObject(currentNode));
    propagateSerializedChange(parent, parentOld);
  }

  function nearestConnectedPositiveAncestor(record) {
    let current = record;
    while (current) {
      if (current.nodeId > 0 && current.isConnected !== false) return current;
      current = parentElementRecord(current);
    }
    return null;
  }

  function commitNearestConnectedAncestor(record) {
    const ancestor = nearestConnectedPositiveAncestor(record);
    if (ancestor) {
      unwrapDomOp(op_vixen_dom_set_element_inner_html(ancestor.nodeId, ancestor.innerHTML || ''));
    }
  }

  function setElementAttribute(nodeId, name, value) {
    const record = recordForElementNodeId(nodeId);
    if (!record) throw new TypeError('Vixen DOM host element is unavailable: ' + nodeId);
    const oldSerialized = serializeElementRecord(record);
    const oldValue = recordAttr(record, name);
    const attrName = setRecordAttr(record, name, value);
    queueAttributeMutation(wrapElementByNodeId(nodeId), attrName, oldValue);
    propagateSerializedChange(record, oldSerialized);
    if (nodeId > 0) {
      unwrapDomOp(op_vixen_dom_set_element_attr(nodeId, attrName, String(value)));
    } else {
      commitNearestConnectedAncestor(record);
    }
  }

  function removeElementAttribute(nodeId, name) {
    const record = recordForElementNodeId(nodeId);
    if (!record) throw new TypeError('Vixen DOM host element is unavailable: ' + nodeId);
    const oldSerialized = serializeElementRecord(record);
    const oldValue = recordAttr(record, name);
    const attrName = removeRecordAttr(record, name);
    if (oldValue !== null) queueAttributeMutation(wrapElementByNodeId(nodeId), attrName, oldValue);
    propagateSerializedChange(record, oldSerialized);
    if (nodeId > 0) {
      unwrapDomOp(op_vixen_dom_remove_element_attr(nodeId, attrName));
    } else {
      commitNearestConnectedAncestor(record);
    }
  }

  function setElementInnerHTML(element, html) {
    const record = elementRecord(element);
    const oldSerialized = serializeElementRecord(record);
    const removed = nodesFromIds((record.childNodeIds || record.childElementNodeIds || []).slice());
    for (const childId of record.childNodeIds || record.childElementNodeIds || []) {
      const child = nodeObjects.get(childId) || elementObjects.get(childId);
      const childRecord = nodeRecord(child);
      if (childRecord) {
        childRecord.parentNodeId = null;
        childRecord.isConnected = false;
      }
    }
    record.innerHTML = String(html);
    record.textContent = String(html).replace(/<[^>]*>/g, '');
    record.childNodeIds = [];
    record.childElementNodeIds = [];
    record.firstElementChildNodeId = null;
    record.lastElementChildNodeId = null;
    if (removed.length > 0 || String(html) !== '') queueChildListMutation(element, [], removed);
    propagateSerializedChange(record, oldSerialized);
    if (record.nodeId > 0) {
      unwrapDomOp(op_vixen_dom_set_element_inner_html(record.nodeId, record.innerHTML));
    } else {
      commitNearestConnectedAncestor(record);
    }
  }

  function parseSimpleSelectorList(selector) {
    return splitSelectorList(selector).map(parseComplexSelector);
  }

  function splitSelectorList(selector) {
    const input = String(selector);
    const parts = [];
    let start = 0;
    let bracketDepth = 0;
    let parenDepth = 0;
    let quote = '';
    for (let i = 0; i < input.length; i++) {
      const ch = input[i];
      if (quote) {
        if (ch === '\\') i += 1;
        else if (ch === quote) quote = '';
        continue;
      }
      if (ch === '"' || ch === "'") {
        quote = ch;
      } else if (ch === '[') {
        bracketDepth += 1;
      } else if (ch === ']') {
        bracketDepth = Math.max(0, bracketDepth - 1);
      } else if (ch === '(') {
        parenDepth += 1;
      } else if (ch === ')') {
        parenDepth = Math.max(0, parenDepth - 1);
      } else if (ch === ',' && bracketDepth === 0 && parenDepth === 0) {
        const part = input.slice(start, i).trim();
        if (!part) throw new TypeError('unsupported selector');
        parts.push(part);
        start = i + 1;
      }
    }
    const tail = input.slice(start).trim();
    if (!tail) throw new TypeError('unsupported selector');
    parts.push(tail);
    return parts;
  }

  function parseComplexSelector(raw) {
    const input = String(raw).trim();
    if (!input) throw new TypeError('unsupported selector');
    const steps = [];
    let combinator = null;
    let i = 0;
    while (i < input.length) {
      const beforeWs = i;
      while (i < input.length && /[\t\n\f\r ]/.test(input[i])) i += 1;
      if (i >= input.length) break;
      if (input[i] === '>') {
        if (steps.length === 0 || combinator !== null) throw new TypeError('unsupported selector');
        combinator = 'child';
        i += 1;
        continue;
      }
      if (i > beforeWs && steps.length > 0 && combinator === null) combinator = 'descendant';
      if (input[i] === '+' || input[i] === '~') throw new TypeError('unsupported selector');
      const start = i;
      i = scanCompoundEnd(input, i);
      if (i === start) throw new TypeError('unsupported selector');
      steps.push({
        combinator: steps.length === 0 ? null : (combinator || 'descendant'),
        compound: parseCompoundSelector(input.slice(start, i)),
      });
      combinator = null;
    }
    if (steps.length === 0 || combinator !== null) throw new TypeError('unsupported selector');
    return { steps };
  }

  function scanCompoundEnd(input, start) {
    let bracketDepth = 0;
    let parenDepth = 0;
    let quote = '';
    for (let i = start; i < input.length; i++) {
      const ch = input[i];
      if (quote) {
        if (ch === '\\') i += 1;
        else if (ch === quote) quote = '';
        continue;
      }
      if (ch === '"' || ch === "'") quote = ch;
      else if (ch === '[') bracketDepth += 1;
      else if (ch === ']') bracketDepth = Math.max(0, bracketDepth - 1);
      else if (ch === '(') parenDepth += 1;
      else if (ch === ')') parenDepth = Math.max(0, parenDepth - 1);
      else if (bracketDepth === 0 && parenDepth === 0 && (/[\t\n\f\r ]/.test(ch) || ch === '>' || ch === '+' || ch === '~')) return i;
    }
    return input.length;
  }

  function parseCompoundSelector(raw) {
    let rest = String(raw);
    const selector = { tag: null, id: null, classes: [], attrs: [], pseudos: [] };
    if (rest.startsWith('*')) rest = rest.slice(1);
    else {
      const tag = /^[A-Za-z][A-Za-z0-9_-]*/.exec(rest);
      if (tag) {
        selector.tag = tag[0].toLowerCase();
        rest = rest.slice(tag[0].length);
      }
    }
    while (rest.length > 0) {
      if (rest.startsWith('#')) {
        const m = /^#([A-Za-z_][A-Za-z0-9_-]*)/.exec(rest);
        if (!m) throw new TypeError('unsupported selector');
        selector.id = m[1];
        rest = rest.slice(m[0].length);
      } else if (rest.startsWith('.')) {
        const m = /^\.([A-Za-z_][A-Za-z0-9_-]*)/.exec(rest);
        if (!m) throw new TypeError('unsupported selector');
        selector.classes.push(m[1]);
        rest = rest.slice(m[0].length);
      } else if (rest.startsWith('[')) {
        const end = findBalancedClose(rest, 0, '[', ']');
        if (end === -1) throw new TypeError('unsupported selector');
        selector.attrs.push(parseAttributeSelector(rest.slice(1, end)));
        rest = rest.slice(end + 1);
      } else if (rest.startsWith(':')) {
        const pseudo = parseFunctionalPseudo(rest);
        selector.pseudos.push(pseudo.value);
        rest = rest.slice(pseudo.length);
      } else {
        throw new TypeError('unsupported selector');
      }
    }
    if (!selector.tag && !selector.id && selector.classes.length === 0 && selector.attrs.length === 0 && selector.pseudos.length === 0 && raw !== '*') {
      throw new TypeError('unsupported selector');
    }
    return selector;
  }

  function parseAttributeSelector(body) {
    const eq = String(body).indexOf('=');
    if (eq === -1) return [String(body).trim().toLowerCase(), null];
    const name = String(body).slice(0, eq).trim().toLowerCase();
    let value = String(body).slice(eq + 1).trim();
    if ((value.startsWith('"') && value.endsWith('"')) || (value.startsWith("'") && value.endsWith("'"))) {
      value = value.slice(1, -1);
    }
    return [name, value];
  }

  function parseFunctionalPseudo(rest) {
    const nameMatch = /^:([A-Za-z-]+)/.exec(rest);
    if (!nameMatch) throw new TypeError('unsupported selector');
    const name = nameMatch[1].toLowerCase();
    const open = nameMatch[0].length;
    if (rest[open] !== '(') throw new TypeError('unsupported selector');
    const close = findBalancedClose(rest, open, '(', ')');
    if (close === -1) throw new TypeError('unsupported selector');
    const body = rest.slice(open + 1, close);
    if (name === 'is' || name === 'where' || name === 'not') {
      return { length: close + 1, value: { name, selectors: splitSelectorList(body).map(parseComplexSelector) } };
    }
    if (name === 'has') {
      return { length: close + 1, value: { name, selectors: splitSelectorList(body).map(parseRelativeSelector) } };
    }
    throw new TypeError('unsupported selector');
  }

  function parseRelativeSelector(raw) {
    const input = String(raw).trim();
    if (input.startsWith('>')) {
      return { combinator: 'child', selector: parseComplexSelector(input.slice(1).trim()) };
    }
    return { combinator: 'descendant', selector: parseComplexSelector(input) };
  }

  function findBalancedClose(input, openIndex, openChar, closeChar) {
    let depth = 0;
    let quote = '';
    for (let i = openIndex; i < input.length; i++) {
      const ch = input[i];
      if (quote) {
        if (ch === '\\') i += 1;
        else if (ch === quote) quote = '';
        continue;
      }
      if (ch === '"' || ch === "'") quote = ch;
      else if (ch === openChar) depth += 1;
      else if (ch === closeChar) {
        depth -= 1;
        if (depth === 0) return i;
      }
    }
    return -1;
  }

  function recordMatchesAny(record, selectors) {
    return selectors.some((selector) => complexSelectorMatches(record, selector));
  }

  function complexSelectorMatches(record, selector) {
    return selectorStepMatches(record, selector.steps, selector.steps.length - 1);
  }

  function selectorStepMatches(record, steps, index) {
    if (!record || record.isConnected === false || !recordMatches(record, steps[index].compound)) return false;
    if (index === 0) return true;
    const combinator = steps[index].combinator || 'descendant';
    if (combinator === 'child') {
      return selectorStepMatches(parentElementRecord(record), steps, index - 1);
    }
    let parent = parentElementRecord(record);
    while (parent) {
      if (selectorStepMatches(parent, steps, index - 1)) return true;
      parent = parentElementRecord(parent);
    }
    return false;
  }

  function recordMatches(record, selector) {
    if (selector.tag && String(record.tag).toLowerCase() !== selector.tag) return false;
    if (selector.id !== null && record.id !== selector.id) return false;
    for (const klass of selector.classes) {
      if (!record.classes.includes(klass)) return false;
    }
    for (const [name, value] of selector.attrs) {
      const attr = recordAttr(record, name);
      if (value === null) {
        if (attr === null) return false;
      } else if (attr !== value) return false;
    }
    for (const pseudo of selector.pseudos) {
      if (pseudo.name === 'is' || pseudo.name === 'where') {
        if (!recordMatchesAny(record, pseudo.selectors)) return false;
      } else if (pseudo.name === 'not') {
        if (recordMatchesAny(record, pseudo.selectors)) return false;
      } else if (pseudo.name === 'has') {
        if (!relativeSelectorMatchesAny(record, pseudo.selectors)) return false;
      } else {
        return false;
      }
    }
    return true;
  }

  function relativeSelectorMatchesAny(record, selectors) {
    return selectors.some((selector) => relativeSelectorMatches(record, selector));
  }

  function relativeSelectorMatches(record, relative) {
    const candidates = relative.combinator === 'child'
      ? childElementRecords(record)
      : descendantElementRecords(record);
    return candidates.some((candidate) => complexSelectorMatches(candidate, relative.selector));
  }

  function childElementRecords(record) {
    return (record && record.childElementNodeIds ? record.childElementNodeIds : [])
      .map(recordForElementNodeId)
      .filter((child) => child && child.isConnected !== false);
  }

  function descendantElementRecords(record) {
    const out = [];
    for (const child of childElementRecords(record)) {
      out.push(child);
      out.push(...descendantElementRecords(child));
    }
    return out;
  }

  function wrapElementByNodeId(nodeId) {
    if (nodeId === null || nodeId === undefined) return null;
    if (!elementObjects.has(nodeId)) {
      const record = op_vixen_dom_element_snapshot(nodeId);
      if (record === null) throw new TypeError('Vixen DOM host element is unavailable: ' + nodeId);
      elementObjects.set(nodeId, makeElementObject(record));
    }
    return elementObjects.get(nodeId);
  }

  function wrapNodeById(nodeId) {
    if (nodeId === null || nodeId === undefined) return null;
    if (nodeObjects.has(nodeId)) return nodeObjects.get(nodeId);
    return wrapElementByNodeId(nodeId);
  }

  function wrapPageElementByIdentity(nodeId, id, tag) {
    const expectedId = id === null || id === undefined || String(id) === '' ? null : String(id);
    const expectedTag = String(tag || '').toLowerCase();
    let target = null;
    try { target = wrapElementByNodeId(nodeId); } catch (_) {}
    const matches = (element) => {
      if (element === null) return false;
      const record = elementRecord(element);
      return (expectedTag === '' || String(record.tag || '').toLowerCase() === expectedTag)
        && (expectedId === null || record.id === expectedId);
    };
    if (matches(target)) return target;
    if (expectedId === null) return null;
    for (const candidateId of knownElementIds()) {
      const candidate = wrapElementByNodeId(candidateId);
      if (matches(candidate)) return candidate;
    }
    return null;
  }

  function interfaceNameForTag(tag) {
    return htmlElementInterfaceByTag.get(String(tag).toLowerCase()) || 'HTMLElement';
  }

  function makeElementObject(record) {
    record.nodeId = Number(record.nodeId);
    record.attributes = (record.attributes || []).map(([name, value]) => [String(name), String(value)]);
    record.className = recordAttr(record, 'class') || record.className || '';
    record.classes = record.className.split(/[\t\n\f\r ]+/).filter((token) => token !== '');
    record.textContent = record.nodeId === data.bodyNodeId && data.bodyTextContent !== undefined
      ? String(data.bodyTextContent)
      : record.textContent || '';
    record.innerHTML = record.innerHTML || '';
    record.childNodeIds = (record.childNodeIds || record.childElementNodeIds || []).slice();
    record.childElementNodeIds = (record.childElementNodeIds || []).slice();
    record.isConnected = record.isConnected !== false;
    rememberElementId(record.nodeId);
    const ctor = webidl.interfaceConstructor(interfaceNameForTag(record.tag));
    const element = Object.create(ctor.prototype);
    Object.defineProperties(element, {
      __vixenNodeId: { value: record.nodeId, enumerable: false },
      __vixenRecord: { value: record, enumerable: false },
    });
    elementObjects.set(record.nodeId, element);
    nodeObjects.set(record.nodeId, element);
    return element;
  }

  function elementRecord(element) {
    if (!Object.prototype.hasOwnProperty.call(element, '__vixenRecord')) {
      const record = op_vixen_dom_element_snapshot(element.__vixenNodeId);
      if (record === null) throw new TypeError('Vixen DOM host element is unavailable: ' + element.__vixenNodeId);
      Object.defineProperty(element, '__vixenRecord', {
        value: record,
        enumerable: false,
      });
    }
    return element.__vixenRecord;
  }

  function validateToken(token) {
    const value = String(token);
    if (value === '') throw new TypeError('DOMTokenList token must not be empty');
    if (/[\t\n\f\r ]/.test(value)) throw new TypeError('DOMTokenList token must not contain ASCII whitespace');
    return value;
  }

  class VixenDOMTokenList {
    constructor(ownerElement, attribute) {
      Object.defineProperties(this, {
        __vixenOwner: { value: ownerElement, enumerable: false },
        __vixenAttribute: { value: String(attribute), enumerable: false },
      });
    }
    get __vixenTokens() { return elementTokens(this.__vixenOwner.__vixenNodeId, this.__vixenAttribute); }
    get length() { return this.__vixenTokens.length; }
    get value() { return serializeTokenSet(this.__vixenTokens); }
    set value(value) { setElementAttribute(this.__vixenOwner.__vixenNodeId, this.__vixenAttribute, String(value)); }
    item(index) {
      const n = Number(index);
      const token = Number.isInteger(n) && n >= 0 ? this.__vixenTokens[n] : undefined;
      return token === undefined ? null : token;
    }
    contains(token) { return this.__vixenTokens.includes(validateToken(token)); }
    add(...tokens) {
      const validated = tokens.map(validateToken);
      const current = this.__vixenTokens;
      for (const token of validated) if (!current.includes(token)) current.push(token);
      this.value = serializeTokenSet(current);
    }
    remove(...tokens) {
      const validated = tokens.map(validateToken);
      this.value = serializeTokenSet(this.__vixenTokens.filter((token) => !validated.includes(token)));
    }
    toggle(token, force = undefined) {
      const validated = validateToken(token);
      const current = this.__vixenTokens;
      const present = current.includes(validated);
      const shouldHave = force === undefined ? !present : Boolean(force);
      if (shouldHave && !present) current.push(validated);
      if (!shouldHave && present) current.splice(current.indexOf(validated), 1);
      this.value = serializeTokenSet(current);
      return shouldHave;
    }
    replace(oldToken, newToken) {
      const oldValue = validateToken(oldToken);
      const newValue = validateToken(newToken);
      const current = this.__vixenTokens;
      const index = current.indexOf(oldValue);
      if (index === -1) return false;
      if (current.includes(newValue)) current.splice(index, 1);
      else current[index] = newValue;
      this.value = serializeTokenSet(current);
      return true;
    }
    supports() { return false; }
    toString() { return this.value; }
    [Symbol.iterator]() { return this.__vixenTokens[Symbol.iterator](); }
  }

  class VixenDOMStringMap {
    constructor(pairs) {
      Object.defineProperty(this, '__vixenPairs', {
        value: Object.freeze(pairs.map(([name, value]) => Object.freeze([name, value]))),
        enumerable: false,
      });
      for (const [name, value] of this.__vixenPairs) {
        if (!Object.prototype.hasOwnProperty.call(this, name)) {
          Object.defineProperty(this, name, {
            value,
            enumerable: true,
            configurable: true,
          });
        }
      }
    }
  }

  function defineIndexedValues(target, values) {
    for (let i = 0; i < values.length; i++) {
      Object.defineProperty(target, String(i), {
        value: values[i],
        enumerable: true,
        configurable: true,
      });
    }
  }

  function defineWritableValue(target, name, value, enumerable = true) {
    Object.defineProperty(target, name, {
      value,
      writable: true,
      enumerable,
      configurable: true,
    });
  }

  function defineReadonlyValue(target, name, value, enumerable = true) {
    Object.defineProperty(target, name, {
      value,
      enumerable,
      configurable: true,
    });
  }

  class VixenNodeList {
    constructor(nodeIds) {
      const nodes = nodeIds.map((node) => typeof node === 'number' ? wrapNodeById(node) : node).filter((node) => node !== null);
      Object.defineProperty(this, '__vixenNodes', {
        value: Object.freeze(nodes),
        enumerable: false,
      });
      defineIndexedValues(this, this.__vixenNodes);
    }
    get length() { return this.__vixenNodes.length; }
    item(index) {
      const n = Number(index);
      return Number.isInteger(n) && n >= 0 && n < this.__vixenNodes.length ? this.__vixenNodes[n] : null;
    }
    entries() { return this.__vixenNodes.entries(); }
    keys() { return this.__vixenNodes.keys(); }
    values() { return this.__vixenNodes.values(); }
    forEach(callback, thisArg = undefined) { return this.__vixenNodes.forEach(callback, thisArg); }
    [Symbol.iterator]() { return this.values(); }
  }

  class VixenHTMLCollection {
    constructor(nodeIds) {
      const nodes = nodeIds.map(wrapElementByNodeId).filter((node) => node !== null);
      Object.defineProperty(this, '__vixenNodes', {
        value: Object.freeze(nodes),
        enumerable: false,
      });
      defineIndexedValues(this, this.__vixenNodes);
    }
    get length() { return this.__vixenNodes.length; }
    item(index) {
      const n = Number(index);
      return Number.isInteger(n) && n >= 0 && n < this.__vixenNodes.length ? this.__vixenNodes[n] : null;
    }
    namedItem(name) {
      const value = String(name);
      return this.__vixenNodes.find((node) => node.id === value || elementAttribute(node.__vixenNodeId, 'name') === value) || null;
    }
    [Symbol.iterator]() { return this.__vixenNodes[Symbol.iterator](); }
  }

  class VixenAttr {
    constructor(ownerElement, name, value) {
      Object.defineProperties(this, {
        ownerElement: { value: ownerElement, enumerable: true, configurable: true },
        name: { value: String(name), enumerable: true, configurable: true },
        localName: { value: String(name), enumerable: true, configurable: true },
        value: { value: String(value), enumerable: true, configurable: true },
        namespaceURI: { value: null, enumerable: true, configurable: true },
        prefix: { value: null, enumerable: true, configurable: true },
      });
    }
  }

  class VixenNamedNodeMap {
    constructor(ownerElement) {
      const attrs = elementRecord(ownerElement).attributes.map(([name, value]) => new VixenAttr(ownerElement, name, value));
      Object.defineProperty(this, '__vixenAttributes', {
        value: Object.freeze(attrs),
        enumerable: false,
      });
      defineIndexedValues(this, this.__vixenAttributes);
    }
    get length() { return this.__vixenAttributes.length; }
    item(index) {
      const n = Number(index);
      return Number.isInteger(n) && n >= 0 && n < this.__vixenAttributes.length ? this.__vixenAttributes[n] : null;
    }
    getNamedItem(name) {
      const value = String(name);
      return this.__vixenAttributes.find((attr) => attr.name === value || attr.name.toLowerCase() === value.toLowerCase()) || null;
    }
  }

  class VixenDOMRectReadOnly {
    constructor(rect) {
      const source = rect || { x: 0, y: 0, width: 0, height: 0 };
      Object.defineProperties(this, {
        x: { value: Number(source.x) || 0, enumerable: true, configurable: true },
        y: { value: Number(source.y) || 0, enumerable: true, configurable: true },
        width: { value: Number(source.width) || 0, enumerable: true, configurable: true },
        height: { value: Number(source.height) || 0, enumerable: true, configurable: true },
      });
    }
    get left() { return Math.min(this.x, this.x + this.width); }
    get top() { return Math.min(this.y, this.y + this.height); }
    get right() { return Math.max(this.x, this.x + this.width); }
    get bottom() { return Math.max(this.y, this.y + this.height); }
    toJSON() {
      return {
        x: this.x,
        y: this.y,
        width: this.width,
        height: this.height,
        top: this.top,
        right: this.right,
        bottom: this.bottom,
        left: this.left,
      };
    }
    toString() { return '[object DOMRect]'; }
  }

  class VixenDOMRectList {
    constructor(rects) {
      const list = rects.map((rect) => new VixenDOMRectReadOnly(rect));
      Object.defineProperty(this, '__vixenRects', {
        value: Object.freeze(list),
        enumerable: false,
      });
      defineIndexedValues(this, this.__vixenRects);
    }
    get length() { return this.__vixenRects.length; }
    item(index) {
      const n = Number(index);
      return Number.isInteger(n) && n >= 0 && n < this.__vixenRects.length ? this.__vixenRects[n] : null;
    }
  }

  function makeDOMRectList(rect) {
    return new VixenDOMRectList(rect === null ? [] : [rect]);
  }

  function integerCssPixels(value) {
    const number = Number(value) || 0;
    return Math.max(0, Math.trunc(number));
  }

  function finiteScrollCoordinate(value) {
    const number = Number(value);
    return Number.isFinite(number) ? number : 0;
  }

  function applyTopLevelScroll(x, y) {
    syncRendererScrollState(false);
    const nextX = Math.min(topLevelScrollMaxX, Math.max(0, finiteScrollCoordinate(x)));
    const nextY = Math.min(topLevelScrollMaxY, Math.max(0, finiteScrollCoordinate(y)));
    if (nextX === topLevelScrollX && nextY === topLevelScrollY) return;
    topLevelScrollX = nextX;
    topLevelScrollY = nextY;
    if (globalThis.visualViewport) {
      globalThis.visualViewport.pageLeft = topLevelScrollX;
      globalThis.visualViewport.pageTop = topLevelScrollY;
    }
    unwrapDomOp(op_vixen_dom_set_root_scroll(nextX, nextY));
    dispatchRootScrollEvent();
  }

  function dispatchRootScrollEvent() {
    if (rootScrollEventQueued) return;
    rootScrollEventQueued = true;
    queueMicrotask(() => {
      try {
        vixenDocument.dispatchEvent(new Event('scroll', { bubbles: true }));
      } finally {
        rootScrollEventQueued = false;
      }
    });
  }

  function windowScrollTo(leftOrOptions = 0, top = 0) {
    if (leftOrOptions !== null && typeof leftOrOptions === 'object') {
      const left = leftOrOptions.left === undefined ? topLevelScrollX : leftOrOptions.left;
      const nextTop = leftOrOptions.top === undefined ? topLevelScrollY : leftOrOptions.top;
      applyTopLevelScroll(left, nextTop);
      return;
    }
    applyTopLevelScroll(leftOrOptions, top);
  }

  function windowScrollBy(leftOrOptions = 0, top = 0) {
    if (leftOrOptions !== null && typeof leftOrOptions === 'object') {
      const left = leftOrOptions.left === undefined ? 0 : leftOrOptions.left;
      const nextTop = leftOrOptions.top === undefined ? 0 : leftOrOptions.top;
      applyTopLevelScroll(
        topLevelScrollX + finiteScrollCoordinate(left),
        topLevelScrollY + finiteScrollCoordinate(nextTop),
      );
      return;
    }
    applyTopLevelScroll(
      topLevelScrollX + finiteScrollCoordinate(leftOrOptions),
      topLevelScrollY + finiteScrollCoordinate(top),
    );
  }

  function dispatchElementScrollEvent(element) {
    const nodeId = element && element.__vixenNodeId;
    if (!Number.isInteger(nodeId) || elementScrollEventsQueued.has(nodeId)) return;
    elementScrollEventsQueued.add(nodeId);
    queueMicrotask(() => {
      try {
        element.dispatchEvent(new Event('scroll', { bubbles: false, cancelable: false }));
      } finally {
        elementScrollEventsQueued.delete(nodeId);
      }
    });
  }

  Object.defineProperty(globalThis, '__vixenSyncElementScrollState', {
    value(states = [], emitScroll = false) {
      const byNodeId = new Map();
      const byElementId = new Map();
      for (const state of Array.isArray(states) ? states : []) {
        const nodeId = Number(state && state.nodeId);
        if (Number.isInteger(nodeId) && nodeId > 0) byNodeId.set(nodeId, state);
        const id = state && typeof state.id === 'string' && state.id !== '' ? state.id : null;
        if (id !== null) byElementId.set(id, byElementId.has(id) ? null : state);
      }
      for (const nodeId of knownElementIds()) {
        const record = recordForElementNodeId(nodeId);
        if (!record) continue;
        let state = byNodeId.get(nodeId);
        if (state && (String(state.tag || '') !== String(record.tag || '')
            || (state.id || null) !== (record.id || null))) state = undefined;
        if (state === undefined && record.id) {
          const byId = byElementId.get(record.id);
          if (byId && String(byId.tag || '') === String(record.tag || '')) state = byId;
        }
        const nextLeft = Math.max(0, Number(state && state.left) || 0);
        const nextTop = Math.max(0, Number(state && state.top) || 0);
        const changed = nextLeft !== (Number(record.scrollLeft) || 0)
          || nextTop !== (Number(record.scrollTop) || 0);
        record.scrollLeft = nextLeft;
        record.scrollTop = nextTop;
        record.scrollMaxX = Math.max(0, Number(state && state.maxX) || 0);
        record.scrollMaxY = Math.max(0, Number(state && state.maxY) || 0);
        record.userScrollable = Boolean(state && state.userScrollable);
        record.overflowClips = record.overflowClips === true || state !== undefined;
        if (changed && emitScroll) {
          const element = wrapElementByNodeId(nodeId);
          if (element !== null) dispatchElementScrollEvent(element);
        }
      }
      return true;
    },
    configurable: true,
  });

  function syncRendererScrollState(emitScroll = false) {
    const result = unwrapDomOp(op_vixen_dom_scroll_state());
    if (result.rendererState !== true) return false;
    const root = result.root || {};
    topLevelScrollX = Math.max(0, Number(root.left) || 0);
    topLevelScrollY = Math.max(0, Number(root.top) || 0);
    topLevelScrollMaxX = Math.max(0, Number(root.maxX) || 0);
    topLevelScrollMaxY = Math.max(0, Number(root.maxY) || 0);
    if (globalThis.visualViewport) {
      globalThis.visualViewport.pageLeft = topLevelScrollX;
      globalThis.visualViewport.pageTop = topLevelScrollY;
    }
    globalThis.__vixenSyncElementScrollState(result.states || [], emitScroll);
    return true;
  }

  function applyElementScroll(element, x, y) {
    syncRendererScrollState(false);
    const record = elementRecord(element);
    if (!record || !Number.isInteger(record.nodeId) || record.nodeId <= 0) return false;
    const maxX = Math.max(0, Number(record.scrollMaxX) || 0);
    const maxY = Math.max(0, Number(record.scrollMaxY) || 0);
    const currentX = Math.min(maxX, Math.max(0, Number(record.scrollLeft) || 0));
    const currentY = Math.min(maxY, Math.max(0, Number(record.scrollTop) || 0));
    const nextX = Math.min(maxX, Math.max(0, finiteScrollCoordinate(x)));
    const nextY = Math.min(maxY, Math.max(0, finiteScrollCoordinate(y)));
    if (nextX === currentX && nextY === currentY) return false;
    record.scrollLeft = nextX;
    record.scrollTop = nextY;
    unwrapDomOp(op_vixen_dom_set_element_scroll(record.nodeId, nextX, nextY));
    dispatchElementScrollEvent(element);
    return true;
  }

  function elementScrollTo(element, leftOrOptions = 0, top = 0) {
    const record = elementRecord(element);
    if (!record) return;
    if (leftOrOptions !== null && typeof leftOrOptions === 'object') {
      const left = leftOrOptions.left === undefined ? record.scrollLeft : leftOrOptions.left;
      const nextTop = leftOrOptions.top === undefined ? record.scrollTop : leftOrOptions.top;
      applyElementScroll(element, left, nextTop);
      return;
    }
    applyElementScroll(element, leftOrOptions, top);
  }

  function elementScrollBy(element, leftOrOptions = 0, top = 0) {
    const record = elementRecord(element);
    if (!record) return;
    if (leftOrOptions !== null && typeof leftOrOptions === 'object') {
      applyElementScroll(
        element,
        (Number(record.scrollLeft) || 0) + finiteScrollCoordinate(leftOrOptions.left),
        (Number(record.scrollTop) || 0) + finiteScrollCoordinate(leftOrOptions.top),
      );
      return;
    }
    applyElementScroll(
      element,
      (Number(record.scrollLeft) || 0) + finiteScrollCoordinate(leftOrOptions),
      (Number(record.scrollTop) || 0) + finiteScrollCoordinate(top),
    );
  }

  function applyNestedWheelDefault(target, init) {
    if (init.ctrlKey || init.metaKey) return false;
    let remainingX = finiteScrollCoordinate(init.deltaX);
    let remainingY = finiteScrollCoordinate(init.deltaY);
    let changed = false;
    let sawNestedScrollport = false;
    let element = target;
    while (element && element.nodeType === 1) {
      if (element !== vixenDocument.documentElement && element !== vixenDocument.body) {
        const record = elementRecord(element);
        if (record && record.userScrollable === true) {
          sawNestedScrollport = true;
          const beforeX = Number(record.scrollLeft) || 0;
          const beforeY = Number(record.scrollTop) || 0;
          if (applyElementScroll(element, beforeX + remainingX, beforeY + remainingY)) {
            const consumedX = (Number(record.scrollLeft) || 0) - beforeX;
            const consumedY = (Number(record.scrollTop) || 0) - beforeY;
            remainingX -= consumedX;
            remainingY -= consumedY;
            changed = true;
          }
        }
      }
      element = element.parentElement;
    }
    if (!sawNestedScrollport) return false;
    const beforeRootX = topLevelScrollX;
    const beforeRootY = topLevelScrollY;
    applyTopLevelScroll(topLevelScrollX + remainingX, topLevelScrollY + remainingY);
    return changed || beforeRootX !== topLevelScrollX || beforeRootY !== topLevelScrollY;
  }

  function applyNestedKeyboardScrollDefault(target, init) {
    if (init.ctrlKey || init.metaKey || init.altKey || isPlatformTextEditable(target)) return false;
    const tag = elementTag(target);
    if (tag === 'button' || tag === 'select') return false;
    let scrollport = target;
    while (scrollport && scrollport.nodeType === 1) {
      const record = elementRecord(scrollport);
      if (record && record.userScrollable === true) break;
      scrollport = scrollport.parentElement;
    }
    if (!scrollport || scrollport.nodeType !== 1) return false;
    const page = Math.max(1, elementClientHeight(scrollport) * 0.9);
    let deltaY = 0;
    switch (String(init.key || '')) {
      case 'ArrowDown': deltaY = 40; break;
      case 'ArrowUp': deltaY = -40; break;
      case 'PageDown': deltaY = page; break;
      case 'PageUp': deltaY = -page; break;
      case 'Home': deltaY = -1e9; break;
      case 'End': deltaY = 1e9; break;
      case ' ': deltaY = init.shiftKey ? -page : page; break;
      default: return false;
    }
    return applyNestedWheelDefault(target, { deltaX: 0, deltaY, ctrlKey: false, metaKey: false });
  }

  function scrollIntoViewOptions(value) {
    if (typeof value === 'boolean') {
      return { block: value ? 'start' : 'end', inline: 'nearest' };
    }
    const options = value !== null && typeof value === 'object' ? value : {};
    const alignment = (candidate, fallback) => {
      const text = String(candidate === undefined ? fallback : candidate);
      return ['start', 'center', 'end', 'nearest'].includes(text) ? text : fallback;
    };
    return {
      block: alignment(options.block, 'start'),
      inline: alignment(options.inline, 'nearest'),
    };
  }

  function alignedScrollOffset(current, targetStart, targetSize, viewportStart, viewportSize, alignment) {
    if (alignment === 'start') return current + targetStart - viewportStart;
    if (alignment === 'center') return current + targetStart + targetSize / 2 - viewportStart - viewportSize / 2;
    if (alignment === 'end') return current + targetStart + targetSize - viewportStart - viewportSize;
    if (targetStart < viewportStart) return current + targetStart - viewportStart;
    if (targetStart + targetSize > viewportStart + viewportSize) {
      return current + targetStart + targetSize - viewportStart - viewportSize;
    }
    return current;
  }

  function scrollElementIntoView(element, value = true) {
    if (!element || typeof element.__vixenNodeId !== 'number') return;
    syncRendererScrollState(false);
    const options = scrollIntoViewOptions(value);
    let targetRect = elementRect(element.__vixenNodeId);
    if (!targetRect) return;
    let ancestor = element.parentElement;
    while (ancestor && ancestor.nodeType === 1) {
      const record = elementRecord(ancestor);
      const maxX = Math.max(0, Number(record && record.scrollMaxX) || 0);
      const maxY = Math.max(0, Number(record && record.scrollMaxY) || 0);
      if (maxX > 0 || maxY > 0) {
        const ancestorRect = elementRect(ancestor.__vixenNodeId);
        if (ancestorRect) {
          const nextX = alignedScrollOffset(
            Number(record.scrollLeft) || 0,
            targetRect.x,
            targetRect.width,
            ancestorRect.x,
            ancestorRect.width,
            options.inline,
          );
          const nextY = alignedScrollOffset(
            Number(record.scrollTop) || 0,
            targetRect.y,
            targetRect.height,
            ancestorRect.y,
            ancestorRect.height,
            options.block,
          );
          if (applyElementScroll(ancestor, nextX, nextY)) {
            targetRect = elementRect(element.__vixenNodeId);
          }
        }
      }
      ancestor = ancestor.parentElement;
    }
    const nextRootX = alignedScrollOffset(
      topLevelScrollX,
      targetRect.x,
      targetRect.width,
      0,
      globalThis.innerWidth,
      options.inline,
    );
    const nextRootY = alignedScrollOffset(
      topLevelScrollY,
      targetRect.y,
      targetRect.height,
      0,
      globalThis.innerHeight,
      options.block,
    );
    applyTopLevelScroll(nextRootX, nextRootY);
  }

  function elementClientWidth(element) {
    if (element === vixenDocument.documentElement || element === vixenDocument.body) return integerCssPixels(globalThis.innerWidth);
    const rect = elementRect(element.__vixenNodeId);
    return rect ? integerCssPixels(rect.width) : 0;
  }

  function elementClientHeight(element) {
    if (element === vixenDocument.documentElement || element === vixenDocument.body) return integerCssPixels(globalThis.innerHeight);
    const rect = elementRect(element.__vixenNodeId);
    return rect ? integerCssPixels(rect.height) : 0;
  }

  function elementScrollSize(element, axis) {
    const rect = elementRect(element.__vixenNodeId);
    const base = axis === 'x' ? elementClientWidth(element) : elementClientHeight(element);
    if (!rect) return base;
    const record = elementRecord(element);
    const max = Number(record && (axis === 'x' ? record.scrollMaxX : record.scrollMaxY)) || 0;
    if (max > 0) return integerCssPixels(Math.ceil(base + max));
    let extent = axis === 'x' ? rect.x + base : rect.y + base;
    for (const child of Array.from(element.children || [])) {
      const childRect = elementRect(child.__vixenNodeId);
      if (!childRect) continue;
      extent = Math.max(extent, axis === 'x' ? childRect.x + childRect.width : childRect.y + childRect.height);
    }
    const origin = axis === 'x' ? rect.x : rect.y;
    return Math.max(base, integerCssPixels(Math.ceil(extent - origin)));
  }

  function elementBoxQuad(element) {
    const rect = elementRect(element.__vixenNodeId);
    if (!rect) return [];
    return [DOMQuad.fromRect({ x: rect.x, y: rect.y, width: rect.width, height: rect.height })];
  }

  function nodeGeometryElement(node) {
    if (!node) return null;
    if (node.nodeType === 1 && typeof node.__vixenNodeId === 'number' && elementRecord(node)) return node;
    if (node.nodeType === 3 && node.parentElement) return node.parentElement;
    if (node === vixenDocument) return vixenDocument.body || vixenDocument.documentElement;
    return null;
  }

  function rangeGeometryRect(range) {
    if (!range) return null;
    const target = range.__vixenGeometryNode
      || nodeGeometryElement(range.startContainer)
      || nodeGeometryElement(range.endContainer);
    if (!target || typeof target.__vixenNodeId !== 'number') return null;
    const sameText = range.startContainer === range.endContainer && range.startContainer.nodeType === 3;
    const parent = sameText ? range.startContainer.parentElement : target;
    if (parent && typeof parent.__vixenNodeId === 'number' && parent.__vixenNodeId > 0) {
      const result = unwrapDomOp(op_vixen_dom_range_rect(
        parent.__vixenNodeId,
        sameText ? range.startOffset : 0,
        sameText ? range.endOffset : 0,
        range.collapsed,
        !sameText,
      ));
      if (result.rendererGeometry === true && (result.rect || range.collapsed)) return result.rect;
    }
    if (range.collapsed) return null;
    return elementRect(target.__vixenNodeId);
  }

  let activeElementNodeId = data.activeElementNodeId;

  function cssPropertyName(name) {
    const value = String(name);
    if (value === 'cssFloat') return 'float';
    return value.replace(/[A-Z]/g, (ch) => '-' + ch.toLowerCase()).replace(/_/g, '-');
  }

  function parseStyleEntries(cssText) {
    const entries = [];
    for (const raw of String(cssText || '').split(';')) {
      const part = raw.trim();
      if (!part) continue;
      const index = part.indexOf(':');
      if (index === -1) continue;
      const name = cssPropertyName(part.slice(0, index).trim());
      const value = part.slice(index + 1).trim();
      if (name) entries.push([name, value]);
    }
    return entries;
  }

  function serializeStyleEntries(entries) {
    return entries.map(([name, value]) => name + ': ' + value + ';').join(' ');
  }

  class VixenInlineStyle {
    constructor(ownerElement) {
      Object.defineProperty(this, '__vixenOwner', { value: ownerElement, enumerable: false });
    }
    get __vixenEntries() { return parseStyleEntries(elementAttribute(this.__vixenOwner.__vixenNodeId, 'style') || ''); }
    get cssText() { return elementAttribute(this.__vixenOwner.__vixenNodeId, 'style') || ''; }
    set cssText(value) { setElementAttribute(this.__vixenOwner.__vixenNodeId, 'style', String(value)); }
    get length() { return this.__vixenEntries.length; }
    item(index) {
      const n = Number(index);
      return Number.isInteger(n) && n >= 0 && n < this.__vixenEntries.length ? this.__vixenEntries[n][0] : '';
    }
    getPropertyValue(property) {
      const name = cssPropertyName(property);
      const pair = this.__vixenEntries.find(([prop]) => prop === name);
      return pair ? pair[1] : '';
    }
    setProperty(property, value) {
      const name = cssPropertyName(property);
      const entries = this.__vixenEntries.filter(([prop]) => prop !== name);
      if (String(value) !== '') entries.push([name, String(value)]);
      this.cssText = serializeStyleEntries(entries);
    }
    removeProperty(property) {
      const name = cssPropertyName(property);
      const old = this.getPropertyValue(name);
      this.cssText = serializeStyleEntries(this.__vixenEntries.filter(([prop]) => prop !== name));
      return old;
    }
    get display() { return this.getPropertyValue('display'); }
    set display(value) { this.setProperty('display', value); }
    get color() { return this.getPropertyValue('color'); }
    set color(value) { this.setProperty('color', value); }
    get backgroundColor() { return this.getPropertyValue('background-color'); }
    set backgroundColor(value) { this.setProperty('background-color', value); }
    get width() { return this.getPropertyValue('width'); }
    set width(value) { this.setProperty('width', value); }
    get height() { return this.getPropertyValue('height'); }
    set height(value) { this.setProperty('height', value); }
  }

  class VixenText {
    constructor(data, nodeId = nextLocalNodeId--) {
      Object.defineProperties(this, {
        __vixenNodeId: { value: nodeId, enumerable: false },
        __vixenRecord: {
          value: {
            nodeId,
            nodeType: 3,
            data: String(data),
            parentNodeId: null,
            previousSiblingNodeId: null,
            nextSiblingNodeId: null,
            isConnected: false,
          },
          enumerable: false,
        },
      });
      nodeObjects.set(nodeId, this);
    }
    get nodeType() { return 3; }
    get nodeName() { return '#text'; }
    get ownerDocument() { return vixenDocument; }
    getRootNode() { return vixenDocument; }
    contains(target) { return nodeContains(this, target); }
    get parentNode() { return wrapElementByNodeId(this.__vixenRecord.parentNodeId); }
    get parentElement() { return this.parentNode; }
    get previousSibling() { return wrapNodeById(this.__vixenRecord.previousSiblingNodeId); }
    get nextSibling() { return wrapNodeById(this.__vixenRecord.nextSiblingNodeId); }
    get isConnected() { return this.__vixenRecord.isConnected !== false; }
    get data() { return this.__vixenRecord.data; }
    set data(value) {
      const oldSerialized = escapeTextForHtml(this.__vixenRecord.data);
      const oldText = this.__vixenRecord.data;
      this.__vixenRecord.data = String(value);
      const parent = parentElementRecord(this.__vixenRecord);
      if (parent) parent.textContent = replaceFirst(parent.textContent || '', oldText, this.__vixenRecord.data);
      queueCharacterDataMutation(this, oldText);
      propagateSerializedChange(this.__vixenRecord, oldSerialized);
      commitNearestConnectedAncestor(this.__vixenRecord);
    }
    get nodeValue() { return this.data; }
    set nodeValue(value) { this.data = value; }
    get textContent() { return this.data; }
    set textContent(value) { this.data = value; }
    get wholeText() { return this.data; }
    get length() { return this.data.length; }
    toString() { return this.data; }
  }

  Object.defineProperty(globalThis, '__vixenDispatchMouseEvent', {
    value(nodeId, type, init = {}, targetId = null, targetTag = '') {
      const target = wrapPageElementByIdentity(Number(nodeId), targetId, targetTag);
      if (target === null) return false;
      const opts = Object.assign({
        bubbles: true,
        cancelable: true,
        composed: true,
      }, init || {});
      if (Number.isInteger(opts.relatedNodeId)) opts.relatedTarget = wrapElementByNodeId(opts.relatedNodeId);
      delete opts.relatedNodeId;
      const eventType = String(type);
      const EventCtor = eventType === 'wheel' && typeof WheelEvent === 'function' ? WheelEvent : MouseEvent;
      const event = new EventCtor(eventType, opts);
      const allowed = target.dispatchEvent(event);
      if (!allowed) return false;
      if (eventType === 'wheel' && applyNestedWheelDefault(target, opts)) return false;
      return true;
    },
    configurable: true,
  });

  Object.defineProperty(globalThis, '__vixenDispatchAccessibilityAction', {
    value(nodeId, action, value = null) {
      const target = wrapElementByNodeId(Number(nodeId));
      if (target === null) return false;
      if (String(action) === 'focus') {
        target.focus();
        return activeElementNodeId === target.__vixenNodeId;
      }
      if (String(action) === 'set_value') {
        if (!isPlatformTextEditable(target)) return false;
        const text = String(value === null ? '' : value);
        applyPlatformTextValue(target, text, text.length, text.length, 'insertReplacementText', text, true);
        return true;
      }
      if (String(action) === 'increase' || String(action) === 'decrease') {
        const direction = String(action) === 'increase' ? 1 : -1;
        if (adjustRangeControl(target, direction)) return true;
        return dispatchAuthoredRangeAdjustment(target, direction);
      }
      return false;
    },
    configurable: true,
  });

  function keyboardEventTarget() {
    return wrapElementByNodeId(activeElementNodeId) || vixenDocument.body || vixenDocument;
  }

  let activeTextComposition = null;

  function dispatchTextCompositionEvent(target, type, data) {
    const event = new Event(type, {
      bubbles: true,
      cancelable: false,
      composed: true,
    });
    Object.defineProperty(event, 'data', {
      value: String(data || ''),
      enumerable: true,
      configurable: true,
    });
    target.dispatchEvent(event);
  }

  function finishActiveTextComposition() {
    const composition = activeTextComposition;
    if (composition === null) return;
    activeTextComposition = null;
    const target = wrapElementByNodeId(composition.nodeId);
    if (target !== null) dispatchTextCompositionEvent(target, 'compositionend', composition.data);
  }

  Object.defineProperty(globalThis, '__vixenApplyTextInputState', {
    value(state = {}) {
      const target = keyboardEventTarget();
      if (!isPlatformTextEditable(target)) return false;
      const text = String(state.text || '');
      const selectionBase = Number(state.selectionBase);
      const selectionExtent = Number(state.selectionExtent);
      const composing = Number.isInteger(state.composingBase) && Number.isInteger(state.composingExtent)
        ? [state.composingBase, state.composingExtent]
        : null;
      if (composing !== null && activeTextComposition !== null && activeTextComposition.nodeId !== target.__vixenNodeId) {
        finishActiveTextComposition();
      }
      const priorComposition = activeTextComposition;
      const compositionText = composing === null ? '' : text.slice(composing[0], composing[1]);
      if (composing !== null && (priorComposition === null || priorComposition.nodeId !== target.__vixenNodeId)) {
        activeTextComposition = { nodeId: target.__vixenNodeId, data: compositionText };
        dispatchTextCompositionEvent(target, 'compositionstart', '');
        if (keyboardEventTarget() !== target
            || activeTextComposition === null
            || activeTextComposition.nodeId !== target.__vixenNodeId) return true;
      } else if (composing !== null) {
        activeTextComposition = { nodeId: target.__vixenNodeId, data: compositionText };
      }

      const previous = platformTextValue(target);
      if (previous !== text) {
        const inputType = composing !== null
          ? 'insertCompositionText'
          : priorComposition !== null && priorComposition.nodeId === target.__vixenNodeId
          ? 'insertFromComposition'
          : 'insertText';
        const dataValue = composing === null ? null : compositionText;
        const beforeInput = new InputEvent('beforeinput', {
          bubbles: true,
          cancelable: true,
          composed: true,
          data: dataValue,
          inputType,
          isComposing: composing !== null,
        });
        if (!target.dispatchEvent(beforeInput)) return true;
        if (keyboardEventTarget() !== target
            || (composing !== null
                && (activeTextComposition === null
                    || activeTextComposition.nodeId !== target.__vixenNodeId))) return true;
        applyPlatformTextValue(target, text, selectionBase, selectionExtent);
        target.dispatchEvent(new InputEvent('input', {
          bubbles: true,
          composed: true,
          data: dataValue,
          inputType,
          isComposing: composing !== null,
        }));
      } else {
        setPlatformTextSelection(target, selectionBase, selectionExtent);
      }

      if (composing !== null) {
        dispatchTextCompositionEvent(target, 'compositionupdate', compositionText);
      } else if (priorComposition !== null
          && priorComposition.nodeId === target.__vixenNodeId
          && activeTextComposition !== null
          && activeTextComposition.nodeId === target.__vixenNodeId) {
        activeTextComposition = null;
        dispatchTextCompositionEvent(target, 'compositionend', priorComposition.data);
      }
      return true;
    },
    configurable: true,
  });

  Object.defineProperty(globalThis, '__vixenDispatchKeyEvent', {
    value(type, init = {}) {
      const target = keyboardEventTarget();
      const kind = String(type);
      const opts = Object.assign({
        bubbles: true,
        cancelable: true,
        composed: true,
      }, init || {});
      if (kind === 'char') {
        if (opts.applyText !== false) insertTextIntoControl(target, opts.inputText || opts.text || opts.key || '');
        return true;
      }
      const eventType = kind === 'keyUp' ? 'keyup' : 'keydown';
      const event = new KeyboardEvent(eventType, opts);
      Object.defineProperty(event, '__vixenInputText', { value: String(opts.inputText || opts.text || ''), configurable: true });
      Object.defineProperty(event, '__vixenApplyText', { value: Boolean(opts.applyText), configurable: true });
      const allowed = target.dispatchEvent(event);
      if (!allowed) return false;
      if (eventType === 'keydown' && applyNestedKeyboardScrollDefault(target, opts)) return false;
      return true;
    },
    configurable: true,
  });

  webidl.adoptInterface('Attr', VixenAttr);
  webidl.adoptInterface('NamedNodeMap', VixenNamedNodeMap);
  webidl.adoptInterface('NodeList', VixenNodeList);
  webidl.adoptInterface('HTMLCollection', VixenHTMLCollection);
  webidl.adoptInterface('DOMTokenList', VixenDOMTokenList);
  webidl.adoptInterface('DOMStringMap', VixenDOMStringMap);
  webidl.adoptInterface('DOMRectReadOnly', VixenDOMRectReadOnly);
  webidl.adoptInterface('DOMRectList', VixenDOMRectList);
  webidl.adoptInterface('Text', VixenText);

  class VixenDocumentFragment {
    constructor() {
      defineWritableValue(this, 'textContent', '');
      defineWritableValue(this, 'innerHTML', '');
    }
    get nodeType() { return 11; }
    get nodeName() { return '#document-fragment'; }
    get ownerDocument() { return vixenDocument; }
    get isConnected() { return false; }
    get parentNode() { return null; }
    get childNodes() { return new VixenNodeList([]); }
    get children() { return new VixenHTMLCollection([]); }
    get firstChild() { return null; }
    get lastChild() { return null; }
    get firstElementChild() { return null; }
    get lastElementChild() { return null; }
    get childElementCount() { return 0; }
    get previousSibling() { return null; }
    get nextSibling() { return null; }
    contains(target) { return target === this; }
    getRootNode() { return this; }
    querySelector(_selector) { return null; }
    querySelectorAll(_selector) { return new VixenNodeList([]); }
    getElementById(_id) { return null; }
  }

  class VixenShadowRoot extends VixenDocumentFragment {
    constructor(host, mode) {
      super();
      defineWritableValue(this, 'host', host, false);
      defineWritableValue(this, 'mode', mode);
    }
  }

  webidl.adoptInterface('DocumentFragment', VixenDocumentFragment);
  webidl.adoptInterface('ShadowRoot', VixenShadowRoot);

  const nodeTypeConstants = Object.freeze({
    ELEMENT_NODE: 1,
    ATTRIBUTE_NODE: 2,
    TEXT_NODE: 3,
    CDATA_SECTION_NODE: 4,
    PROCESSING_INSTRUCTION_NODE: 7,
    COMMENT_NODE: 8,
    DOCUMENT_NODE: 9,
    DOCUMENT_TYPE_NODE: 10,
    DOCUMENT_FRAGMENT_NODE: 11,
  });
  for (const [name, value] of Object.entries(nodeTypeConstants)) {
    Object.defineProperty(Node, name, { value, enumerable: true, configurable: true });
    Object.defineProperty(Node.prototype, name, { value, enumerable: true, configurable: true });
  }

  function cachedElementObject(element, key, make) {
    if (!Object.prototype.hasOwnProperty.call(element, key)) {
      Object.defineProperty(element, key, {
        value: make(),
        configurable: true,
      });
    }
    return element[key];
  }

  function makeStyleSheetObject(ownerNode) {
    const ctor = webidl.interfaceConstructor('CSSStyleSheet');
    const sheet = Object.create(ctor.prototype);
    Object.defineProperties(sheet, {
      disabled: { value: false, writable: true, enumerable: true, configurable: true },
      href: { value: null, enumerable: true, configurable: true },
      ownerNode: { value: ownerNode, enumerable: true, configurable: true },
      cssRules: { value: [], enumerable: true, configurable: true },
      rules: { value: [], enumerable: true, configurable: true },
      insertRule: { value: function () { return 0; }, writable: true, enumerable: true, configurable: true },
      deleteRule: { value: function () {}, writable: true, enumerable: true, configurable: true },
    });
    return sheet;
  }

  function isDescendantOf(nodeId, ancestorId) {
    let current = wrapElementByNodeId(nodeId);
    while (current !== null) {
      const parentId = elementRecord(current).parentNodeId;
      if (parentId === ancestorId) return true;
      current = wrapElementByNodeId(parentId);
    }
    return false;
  }

  function nodeFromAppendArg(value) {
    if (value instanceof VixenElement || value instanceof VixenText) return value;
    return new VixenText(String(value));
  }

  function ensureAppendNode(value) {
    if (value instanceof VixenElement || value instanceof VixenText) return value;
    throw new TypeError('DOM child mutation expects a Vixen Node');
  }

  function childIds(record) {
    if (!record.childNodeIds) record.childNodeIds = (record.childElementNodeIds || []).slice();
    const text = syntheticTextForRecord(record);
    const isTextOnly = record.childNodeIds.length === 0 && (!record.childElementNodeIds || record.childElementNodeIds.length === 0);
    const needsLabelText = String(record.tag || '').toLowerCase() === 'label'
      && text.length > 0
      && (record.syntheticTextNodeId === undefined || !record.childNodeIds.includes(record.syntheticTextNodeId));
    if ((isTextOnly || needsLabelText) && text.length > 0) {
      let textNode = record.syntheticTextNodeId === undefined ? null : wrapNodeById(record.syntheticTextNodeId);
      if (!(textNode instanceof VixenText)) {
        textNode = new VixenText(text);
        record.syntheticTextNodeId = textNode.__vixenNodeId;
      }
      textNode.__vixenRecord.data = text;
      textNode.__vixenRecord.parentNodeId = record.nodeId;
      textNode.__vixenRecord.previousSiblingNodeId = null;
      textNode.__vixenRecord.nextSiblingNodeId = record.childNodeIds.length ? record.childNodeIds[0] : null;
      textNode.__vixenRecord.isConnected = record.isConnected !== false;
      if (isTextOnly) record.childNodeIds.push(textNode.__vixenNodeId);
      else record.childNodeIds.unshift(textNode.__vixenNodeId);
    }
    return record.childNodeIds;
  }

  function elementChildIds(record) {
    if (!record.childElementNodeIds) record.childElementNodeIds = [];
    return record.childElementNodeIds;
  }

  function setConnectedSubtree(node, isConnected) {
    const record = nodeRecord(node);
    if (!record) return;
    record.isConnected = Boolean(isConnected);
    if (node instanceof VixenElement) {
      for (const childId of childIds(record)) {
        const child = wrapNodeById(childId);
        if (child) setConnectedSubtree(child, isConnected);
      }
    }
  }

  function refreshChildTopology(parentRecord) {
    const ids = childIds(parentRecord);
    const elementIds = ids.filter((nodeId) => wrapNodeById(nodeId) instanceof VixenElement);
    parentRecord.childElementNodeIds = elementIds;
    parentRecord.firstElementChildNodeId = elementIds.length ? elementIds[0] : null;
    parentRecord.lastElementChildNodeId = elementIds.length ? elementIds[elementIds.length - 1] : null;
    for (let i = 0; i < ids.length; i++) {
      const node = wrapNodeById(ids[i]);
      const record = nodeRecord(node);
      if (!record) continue;
      record.parentNodeId = parentRecord.nodeId;
      record.previousSiblingNodeId = i > 0 ? ids[i - 1] : null;
      record.nextSiblingNodeId = i + 1 < ids.length ? ids[i + 1] : null;
      record.isConnected = parentRecord.isConnected !== false;
    }
    for (let i = 0; i < elementIds.length; i++) {
      const record = nodeRecord(wrapElementByNodeId(elementIds[i]));
      if (!record) continue;
      record.previousElementSiblingNodeId = i > 0 ? elementIds[i - 1] : null;
      record.nextElementSiblingNodeId = i + 1 < elementIds.length ? elementIds[i + 1] : null;
    }
  }

  function detachFromCurrentParent(child) {
    const childRecord = nodeRecord(child);
    const parent = childRecord && childRecord.parentNodeId !== null && childRecord.parentNodeId !== undefined
      ? wrapElementByNodeId(childRecord.parentNodeId)
      : null;
    if (!parent) return;
    removeChildNode(parent, child, true);
  }

  function appendChildNode(parent, child) {
    return insertChildNode(parent, child, null);
  }

  function maybeExecuteInsertedScript(node) {
    const record = nodeRecord(node);
    if (!record || String(record.tag || '').toLowerCase() !== 'script') return;
    if (record.isConnected === false || record.__vixenAlreadyStarted) return;
    record.__vixenAlreadyStarted = true;
    const type = String(recordAttr(record, 'type') || '').trim().toLowerCase();
    if (type && !['text/javascript', 'application/javascript', 'application/ecmascript', 'text/ecmascript'].includes(type)) return;
    if (recordAttr(record, 'src')) return;
    const source = String(record.textContent || topLevelTextFromHtml(record.innerHTML || ''));
    if (source) (0, eval)(source);
    if (typeof node.dispatchEvent === 'function') node.dispatchEvent(new Event('load'));
  }

  function maybeDispatchInsertedStyleLoad(node) {
    const record = nodeRecord(node);
    if (!record || String(record.tag || '').toLowerCase() !== 'style') return;
    if (record.isConnected === false) return;
    if (typeof node.dispatchEvent === 'function') node.dispatchEvent(new Event('load'));
  }

  function insertChildNode(parent, child, before = null) {
    child = ensureAppendNode(child);
    if (before !== null) before = ensureAppendNode(before);
    const parentRecord = elementRecord(parent);
    const oldSerialized = serializeElementRecord(parentRecord);
    detachFromCurrentParent(child);
    const ids = childIds(parentRecord);
    const beforeId = before === null ? null : before.__vixenNodeId;
    const index = beforeId === null ? -1 : ids.indexOf(beforeId);
    if (beforeId !== null && index === -1) throw new TypeError('insertBefore reference is not a child of this element');
    const previousSibling = index === -1 ? wrapNodeById(ids.length ? ids[ids.length - 1] : null) : wrapNodeById(index > 0 ? ids[index - 1] : null);
    const nextSibling = index === -1 ? null : before;
    if (index === -1) {
      ids.push(child.__vixenNodeId);
      parentRecord.innerHTML = (parentRecord.innerHTML || '') + serializeNodeObject(child);
      parentRecord.textContent = (parentRecord.textContent || '') + textContentOfNode(child);
    } else {
      ids.splice(index, 0, child.__vixenNodeId);
      parentRecord.innerHTML = replaceFirst(parentRecord.innerHTML || '', serializeNodeObject(before), serializeNodeObject(child) + serializeNodeObject(before));
      parentRecord.textContent = (parentRecord.textContent || '') + textContentOfNode(child);
    }
    refreshChildTopology(parentRecord);
    setConnectedSubtree(child, parentRecord.isConnected !== false);
    queueChildListMutation(parent, [child], [], previousSibling, nextSibling);
    propagateSerializedChange(parentRecord, oldSerialized);
    commitNearestConnectedAncestor(parentRecord);
    maybeExecuteInsertedScript(child);
    maybeDispatchInsertedStyleLoad(child);
    return child;
  }

  function removeChildNode(parent, child, commit = true, queueRecord = true) {
    child = ensureAppendNode(child);
    const parentRecord = elementRecord(parent);
    const ids = childIds(parentRecord);
    const index = ids.indexOf(child.__vixenNodeId);
    if (index === -1) throw new TypeError('removeChild target is not a child of this element');
    const oldSerialized = serializeElementRecord(parentRecord);
    const previousSibling = wrapNodeById(index > 0 ? ids[index - 1] : null);
    const nextSibling = wrapNodeById(index + 1 < ids.length ? ids[index + 1] : null);
    ids.splice(index, 1);
    parentRecord.innerHTML = replaceFirst(parentRecord.innerHTML || '', serializeNodeObject(child), '');
    parentRecord.textContent = replaceFirst(parentRecord.textContent || '', textContentOfNode(child), '');
    refreshChildTopology(parentRecord);
    const childRecord = nodeRecord(child);
    childRecord.parentNodeId = null;
    childRecord.previousSiblingNodeId = null;
    childRecord.nextSiblingNodeId = null;
    if (childRecord.previousElementSiblingNodeId !== undefined) childRecord.previousElementSiblingNodeId = null;
    if (childRecord.nextElementSiblingNodeId !== undefined) childRecord.nextElementSiblingNodeId = null;
    setConnectedSubtree(child, false);
    if (queueRecord) queueChildListMutation(parent, [], [child], previousSibling, nextSibling);
    propagateSerializedChange(parentRecord, oldSerialized);
    if (commit) commitNearestConnectedAncestor(parentRecord);
    return child;
  }

  function replaceElementChildren(parent, nodes) {
    const parentRecord = elementRecord(parent);
    const oldSerialized = serializeElementRecord(parentRecord);
    const removed = nodesFromIds(childIds(parentRecord).slice());
    for (const childId of childIds(parentRecord).slice()) {
      const child = wrapNodeById(childId);
      if (child) removeChildNode(parent, child, false, false);
    }
    parentRecord.childNodeIds = [];
    parentRecord.childElementNodeIds = [];
    parentRecord.innerHTML = '';
    for (const value of nodes) {
      const child = nodeFromAppendArg(value);
      detachFromCurrentParent(child);
      parentRecord.childNodeIds.push(child.__vixenNodeId);
      parentRecord.innerHTML += serializeNodeObject(child);
    }
    refreshChildTopology(parentRecord);
    for (const childId of parentRecord.childNodeIds) {
      const child = wrapNodeById(childId);
      if (child) setConnectedSubtree(child, parentRecord.isConnected !== false);
    }
    parentRecord.textContent = nodes.map((value) => value instanceof VixenText ? value.data : value instanceof VixenElement ? elementText(value.__vixenNodeId) : String(value)).join('');
    queueChildListMutation(parent, nodesFromIds(parentRecord.childNodeIds), removed);
    propagateSerializedChange(parentRecord, oldSerialized);
    commitNearestConnectedAncestor(parentRecord);
  }

  function eventPathForTarget(target, _event) {
    if (target === vixenDocument) return [vixenDocument, globalThis];
    if (!target || typeof target.__vixenNodeId !== 'number') return [target];
    const path = [];
    let current = target;
    while (current !== null) {
      path.push(current);
      current = current.parentElement;
    }
    path.push(vixenDocument);
    return path;
  }

  function nodeContains(root, target) {
    if (root === target) return true;
    if (root === vixenDocument) return Boolean(target && (target === vixenDocument || typeof target.__vixenNodeId === 'number'));
    let current = target;
    while (current && current !== vixenDocument) {
      current = current.parentNode || null;
      if (current === root) return true;
    }
    return false;
  }

  function elementName(element) {
    return elementAttribute(element.__vixenNodeId, 'name') || '';
  }

  const labelableControlSelector = 'button,input,meter,output,progress,select,textarea';

  function reflectedAttribute(element, name) {
    return elementAttribute(element.__vixenNodeId, name) || '';
  }

  function nullableReflectedAttribute(element, name) {
    const value = elementAttribute(element.__vixenNodeId, name);
    return value === null ? null : value;
  }

  function setReflectedAttribute(element, name, value) {
    setElementAttribute(element.__vixenNodeId, name, String(value));
  }

  function reflectedUnsigned(element, name) {
    const value = Number.parseInt(reflectedAttribute(element, name), 10);
    return Number.isFinite(value) && value >= 0 ? value : 0;
  }

  function reflectedInteger(element, name, missingDefault = 0) {
    const value = Number.parseInt(reflectedAttribute(element, name), 10);
    return Number.isFinite(value) ? value : missingDefault;
  }

  function setReflectedInteger(element, name, value) {
    const number = Number(value);
    setReflectedAttribute(element, name, String(Number.isFinite(number) ? Math.trunc(number) : 0));
  }

  function reflectedNumber(element, name, missingDefault = 0) {
    const value = Number.parseFloat(reflectedAttribute(element, name));
    return Number.isFinite(value) ? value : missingDefault;
  }

  function setReflectedNumber(element, name, value) {
    const number = Number(value);
    setReflectedAttribute(element, name, String(Number.isFinite(number) ? number : 0));
  }

  function setReflectedUnsigned(element, name, value) {
    const number = Number(value);
    setReflectedAttribute(element, name, String(Number.isFinite(number) && number >= 0 ? Math.trunc(number) : 0));
  }

  function elementUrlAttribute(element, name) {
    const value = reflectedAttribute(element, name);
    if (value === '') return null;
    try { return new URL(value, data.baseURI || currentUrl); } catch (_) { return null; }
  }

  function elementUrlPart(element, name, part) {
    const url = elementUrlAttribute(element, name);
    return url ? url[part] : '';
  }

  function reflectedKeyword(element, name, allowed, missingDefault = '') {
    const value = reflectedAttribute(element, name).trim().toLowerCase();
    if (allowed.includes(value)) return value;
    return missingDefault;
  }

  function reflectedBooleanKeyword(element, name, trueKeywords, falseKeywords, missingDefault) {
    const attr = elementAttribute(element.__vixenNodeId, name);
    if (attr === null) return missingDefault;
    const value = String(attr).trim().toLowerCase();
    if (trueKeywords.includes(value)) return true;
    if (falseKeywords.includes(value)) return false;
    return missingDefault;
  }

  function reflectedFormMethod(element) {
    const value = reflectedAttribute(element, 'method').trim().toLowerCase();
    return value === 'post' || value === 'dialog' ? value : 'get';
  }

  function reflectedSubmitMethod(element) {
    const value = reflectedAttribute(element, 'formmethod').trim().toLowerCase();
    return value === 'post' || value === 'dialog' ? value : 'get';
  }

  function reflectedFormEnctype(element) {
    const value = reflectedAttribute(element, 'enctype').trim().toLowerCase();
    if (value === 'multipart/form-data' || value === 'text/plain') return value;
    return 'application/x-www-form-urlencoded';
  }

  function reflectedSubmitEnctype(element) {
    const value = reflectedAttribute(element, 'formenctype').trim().toLowerCase();
    if (value === 'multipart/form-data' || value === 'text/plain') return value;
    return 'application/x-www-form-urlencoded';
  }

  function booleanAttribute(element, name) {
    return elementAttribute(element.__vixenNodeId, name) !== null;
  }

  function setBooleanAttribute(element, name, value) {
    if (Boolean(value)) setElementAttribute(element.__vixenNodeId, name, '');
    else removeElementAttribute(element.__vixenNodeId, name);
  }

  function rawElementType(element) {
    return reflectedAttribute(element, 'type').toLowerCase();
  }

  function mediaState(element) {
    const record = elementRecord(element);
    if (!record.__vixenMediaState) {
      record.__vixenCurrentTime = 0;
      record.__vixenVolume = 1;
      record.__vixenMuted = booleanAttribute(element, 'muted');
      record.__vixenMediaState = true;
    }
    return record;
  }

  function setMediaCurrentTime(element, value) {
    const number = Number(value);
    mediaState(element).__vixenCurrentTime = Number.isFinite(number) && number >= 0 ? number : 0;
  }

  function setMediaVolume(element, value) {
    const number = Number(value);
    if (!Number.isFinite(number) || number < 0 || number > 1) throw new RangeError('volume must be between 0 and 1');
    mediaState(element).__vixenVolume = number;
  }

  function reflectedElementValue(element) {
    const tag = elementTag(element);
    if (tag === 'data' || tag === 'param') return reflectedAttribute(element, 'value');
    if (tag === 'li') return reflectedInteger(element, 'value', 0);
    if (tag === 'progress' || tag === 'meter') return reflectedNumber(element, 'value', 0);
    return controlValue(element);
  }

  function setReflectedElementValue(element, value) {
    const tag = elementTag(element);
    if (tag === 'data' || tag === 'param') {
      setReflectedAttribute(element, 'value', value);
    } else if (tag === 'li') {
      setReflectedInteger(element, 'value', value);
    } else if (tag === 'progress' || tag === 'meter') {
      setReflectedNumber(element, 'value', value);
    } else {
      setControlValue(element, value);
    }
  }

  function reflectedNonNegativeInteger(element, name, missingDefault = -1) {
    const number = Number.parseInt(reflectedAttribute(element, name), 10);
    return Number.isFinite(number) && number >= 0 ? number : missingDefault;
  }

  function textLength(element) {
    return Array.from(controlValue(element)).length;
  }

  function validationMessageForElement(element) {
    return String(elementRecord(element).__vixenCustomValidityMessage || '');
  }

  function inputValueAsNumber(element) {
    const value = controlValue(element);
    if (value === '') return NaN;
    const number = Number(value);
    return Number.isFinite(number) ? number : NaN;
  }

  function setInputValueAsNumber(element, value) {
    const number = Number(value);
    setControlValue(element, Number.isFinite(number) ? String(number) : '');
  }

  function stepControl(element, direction, count = 1) {
    if (elementTag(element) !== 'input') return;
    const type = elementType(element);
    if (type !== 'number' && type !== 'range') return;
    const stepAttr = reflectedAttribute(element, 'step');
    const step = stepAttr && stepAttr.toLowerCase() !== 'any' ? Number(stepAttr) : 1;
    const delta = (Number.isFinite(step) && step > 0 ? step : 1) * (Number(count) || 1) * direction;
    const base = Number.isFinite(inputValueAsNumber(element)) ? inputValueAsNumber(element) : (parseNumberAttr(element, 'min') ?? 0);
    setInputValueAsNumber(element, base + delta);
  }

  function adjustRangeControl(element, direction) {
    if (elementTag(element) !== 'input' || elementType(element) !== 'range') return false;
    const minimum = parseNumberAttr(element, 'min') ?? 0;
    const authoredMaximum = parseNumberAttr(element, 'max');
    const maximum = authoredMaximum !== null && authoredMaximum >= minimum ? authoredMaximum : Math.max(100, minimum);
    const rawStep = reflectedAttribute(element, 'step');
    const parsedStep = rawStep && rawStep.toLowerCase() !== 'any' ? Number(rawStep) : 1;
    const step = Number.isFinite(parsedStep) && parsedStep > 0 ? parsedStep : 1;
    const currentNumber = inputValueAsNumber(element);
    const current = Number.isFinite(currentNumber) ? currentNumber : minimum + (maximum - minimum) / 2;
    const next = Math.min(maximum, Math.max(minimum, current + step * direction));
    if (next === current) return true;
    const text = String(next);
    applyControlValue(element, text, text.length, text.length, 'insertReplacementText', text, true);
    return true;
  }

  function dispatchAuthoredRangeAdjustment(element, direction) {
    const role = (element.getAttribute('role') || '').trim().toLowerCase().split(/\s+/)[0];
    if ((role !== 'slider' && role !== 'spinbutton') || element.getAttribute('aria-valuenow') === null) return false;
    element.focus();
    const vertical = role === 'spinbutton' || (element.getAttribute('aria-orientation') || '').trim().toLowerCase() === 'vertical';
    const key = vertical
      ? (direction > 0 ? 'ArrowUp' : 'ArrowDown')
      : (direction > 0 ? 'ArrowRight' : 'ArrowLeft');
    element.dispatchEvent(new KeyboardEvent('keydown', {
      key,
      code: key,
      bubbles: true,
      cancelable: true,
      composed: true,
    }));
    return true;
  }

  function setRangeText(element, replacement, start = undefined, end = undefined, selectionMode = 'preserve') {
    if (!isTextEditableControl(element)) return;
    const value = controlValue(element);
    const [selectionStart, selectionEnd] = controlSelection(element);
    const from = start === undefined ? selectionStart : clampControlOffset(element, start);
    const to = end === undefined ? selectionEnd : clampControlOffset(element, end);
    const left = Math.min(from, to);
    const right = Math.max(from, to);
    const text = String(replacement);
    setControlValue(element, value.slice(0, left) + text + value.slice(right));
    const newEnd = left + text.length;
    if (selectionMode === 'select') setControlSelection(element, left, newEnd);
    else if (selectionMode === 'start') setControlSelection(element, left, left);
    else if (selectionMode === 'end') setControlSelection(element, newEnd, newEnd);
    else setControlSelection(element, selectionStart, selectionStart + text.length - (right - left));
  }

  function descendantElementsBySelector(element, selector) {
    return findAllNodeIds(selector).filter((nodeId) => isDescendantOf(nodeId, element.__vixenNodeId));
  }

  function firstDescendantElementBySelector(element, selector) {
    const ids = descendantElementsBySelector(element, selector);
    return ids.length > 0 ? wrapElementByNodeId(ids[0]) : null;
  }

  function tableRowsCollection(element) {
    const tag = elementTag(element);
    if (tag === 'table' || tag === 'thead' || tag === 'tbody' || tag === 'tfoot') return new VixenHTMLCollection(descendantElementsBySelector(element, 'tr'));
    return reflectedUnsigned(element, 'rows') || 2;
  }

  function tableCellsCollection(row) {
    if (elementTag(row) !== 'tr') return new VixenHTMLCollection([]);
    const cellIds = elementRecord(row).childElementNodeIds.filter((nodeId) => {
      const cell = wrapElementByNodeId(nodeId);
      return cell && (elementTag(cell) === 'td' || elementTag(cell) === 'th');
    });
    return new VixenHTMLCollection(cellIds);
  }

  function tableRowIndex(row) {
    if (elementTag(row) !== 'tr') return -1;
    return descendantElementsBySelector(vixenDocument.documentElement, 'tr').indexOf(row.__vixenNodeId);
  }

  function sectionRowIndex(row) {
    if (elementTag(row) !== 'tr') return -1;
    const parent = row.parentElement;
    if (!parent) return -1;
    return descendantElementsBySelector(parent, 'tr').indexOf(row.__vixenNodeId);
  }

  function tableCellIndex(cell) {
    const tag = elementTag(cell);
    if (tag !== 'td' && tag !== 'th') return -1;
    const row = cell.parentElement;
    if (!row || elementTag(row) !== 'tr') return -1;
    return Array.from(tableCellsCollection(row)).indexOf(cell);
  }

  function textTrackForTrackElement(trackElement) {
    return cachedElementObject(trackElement, '__vixenTextTrack', () => new VixenTextTrack(
      reflectedAttribute(trackElement, 'kind') || 'subtitles',
      reflectedAttribute(trackElement, 'label'),
      reflectedAttribute(trackElement, 'srclang'),
      trackElement.id || '',
    ));
  }

  function mediaTextTracks(element) {
    if (elementTag(element) !== 'audio' && elementTag(element) !== 'video') return new VixenTextTrackList([]);
    const tracks = descendantElementsBySelector(element, 'track')
      .map((nodeId) => wrapElementByNodeId(nodeId))
      .filter((track) => track !== null)
      .map(textTrackForTrackElement);
    return new VixenTextTrackList(tracks);
  }

  function attachShadowRoot(element, init = {}) {
    const record = elementRecord(element);
    if (record.__vixenShadowRoot) throw new TypeError('Shadow root already attached');
    const mode = init && init.mode === 'closed' ? 'closed' : 'open';
    const root = new VixenShadowRoot(element, mode);
    record.__vixenShadowRoot = root;
    return root;
  }

  function templateContentFragment(element) {
    if (elementTag(element) !== 'template') return reflectedAttribute(element, 'content');
    return cachedElementObject(element, '__vixenTemplateContent', () => {
      const fragment = new VixenDocumentFragment();
      fragment.innerHTML = elementRecord(element).innerHTML || '';
      fragment.textContent = elementText(element.__vixenNodeId);
      return fragment;
    });
  }

  function setTemplateContent(element, value) {
    if (elementTag(element) === 'template') setElementInnerHTML(element, String(value));
    else setReflectedAttribute(element, 'content', value);
  }

  function numericMax(element) {
    const tag = elementTag(element);
    if (tag === 'progress' || tag === 'meter') return reflectedNumber(element, 'max', 1);
    return reflectedAttribute(element, 'max');
  }

  function numericMin(element) {
    return elementTag(element) === 'meter' ? reflectedNumber(element, 'min', 0) : reflectedAttribute(element, 'min');
  }

  function progressPosition(element) {
    if (elementTag(element) !== 'progress' || elementAttribute(element.__vixenNodeId, 'value') === null) return -1;
    const max = Math.max(0, numericMax(element));
    if (max <= 0) return -1;
    return Math.min(Math.max(0, reflectedNumber(element, 'value', 0)), max) / max;
  }

  function scriptTypeSupports(type) {
    const value = String(type || '').trim().toLowerCase();
    return value === 'classic' || value === 'module' || value === 'importmap' || value === 'speculationrules';
  }

  function dialogState(element) {
    const record = elementRecord(element);
    if (!Object.prototype.hasOwnProperty.call(record, '__vixenReturnValue')) record.__vixenReturnValue = '';
    return record;
  }

  function setDialogOpen(element, value) {
    if (elementTag(element) === 'dialog' || elementTag(element) === 'details') setBooleanAttribute(element, 'open', value);
  }

  function showDialog(element, modal = false) {
    if (elementTag(element) !== 'dialog') return;
    setBooleanAttribute(element, 'open', true);
    dialogState(element).__vixenModal = Boolean(modal);
  }

  function closeDialog(element, returnValue = undefined) {
    if (elementTag(element) !== 'dialog') return;
    const record = dialogState(element);
    if (returnValue !== undefined) record.__vixenReturnValue = String(returnValue);
    setBooleanAttribute(element, 'open', false);
    record.__vixenModal = false;
    element.dispatchEvent(new Event('close'));
  }

  function canvasContext2d(element) {
    return cachedElementObject(element, '__vixenCanvas2d', () => new VixenCanvasRenderingContext2D(element));
  }

  function canvasDataUrl(type = 'image/png') {
    const normalized = String(type || 'image/png').toLowerCase() === 'image/jpeg' ? 'image/jpeg' : 'image/png';
    return 'data:' + normalized + ';base64,';
  }

  function elementType(element) {
    const tag = elementTag(element);
    if (tag === 'input') return rawElementType(element) || 'text';
    if (tag === 'button') {
      const type = rawElementType(element);
      return type === 'button' || type === 'reset' || type === 'submit' ? type : 'submit';
    }
    if (tag === 'select') return booleanAttribute(element, 'multiple') ? 'select-multiple' : 'select-one';
    if (tag === 'textarea') return 'textarea';
    return rawElementType(element);
  }

  function labelControlElement(label) {
    if (elementTag(label) !== 'label') return null;
    const forId = reflectedAttribute(label, 'for');
    if (forId !== '') return vixenDocument.getElementById(forId);
    return label.querySelector(labelableControlSelector);
  }

  function controlLabelNodeIds(element) {
    if (!labelableControlSelector.split(',').includes(elementTag(element))) return [];
    const id = element.id;
    return findAllNodeIds('label').filter((nodeId) => {
      const label = wrapElementByNodeId(nodeId);
      if (!label) return false;
      const forId = reflectedAttribute(label, 'for');
      if (forId !== '') return id !== '' && forId === id;
      return nodeContains(label, element);
    });
  }

  function isDisabled(element) {
    return elementAttribute(element.__vixenNodeId, 'disabled') !== null;
  }

  function contentEditableState(element) {
    const attr = elementAttribute(element.__vixenNodeId, 'contenteditable');
    if (attr === null) return 'inherit';
    const value = String(attr).toLowerCase();
    if (value === '' || value === 'true') return 'true';
    if (value === 'false') return 'false';
    if (value === 'plaintext-only') return 'plaintext-only';
    return 'inherit';
  }

  function isContentEditableElement(element) {
    let current = element;
    while (current) {
      const state = contentEditableState(current);
      if (state === 'true' || state === 'plaintext-only') return true;
      if (state === 'false') return false;
      current = current.parentElement;
    }
    return false;
  }

  function contenteditableHost(element) {
    let current = element;
    while (current) {
      const state = contentEditableState(current);
      if (state === 'true' || state === 'plaintext-only') return current;
      if (state === 'false') return null;
      current = current.parentElement;
    }
    return null;
  }

  const textInputTypes = new Set(['', 'text', 'search', 'url', 'tel', 'email', 'password', 'number']);

  function elementTag(element) {
    return String(elementRecord(element).tag).toLowerCase();
  }

  function initialControlValue(element) {
    const tag = elementTag(element);
    if (tag === 'textarea') return elementText(element.__vixenNodeId);
    if (tag === 'input') {
      const type = elementType(element) || 'text';
      const value = elementAttribute(element.__vixenNodeId, 'value');
      if ((type === 'checkbox' || type === 'radio') && value === null) return 'on';
      return value || '';
    }
    return elementAttribute(element.__vixenNodeId, 'value') || '';
  }

  function ensureControlState(element) {
    const record = elementRecord(element);
    if (!record.__vixenControlState) {
      const value = initialControlValue(element);
      record.defaultValue = value;
      record.value = value;
      record.selectionStart = value.length;
      record.selectionEnd = value.length;
      record.__vixenValueDirty = false;
      if (elementTag(element) === 'input') record.defaultChecked = booleanAttribute(element, 'checked');
      if (elementTag(element) === 'option') record.defaultSelected = booleanAttribute(element, 'selected');
      record.__vixenControlState = true;
    }
    return record;
  }

  function filesFromList(value) {
    if (value === null || value === undefined) return [];
    if (typeof value[Symbol.iterator] === 'function') return Array.from(value).filter((file) => file instanceof File);
    const length = Math.max(0, Number(value.length) || 0);
    const files = [];
    for (let i = 0; i < length; i++) {
      const file = typeof value.item === 'function' ? value.item(i) : value[i];
      if (file instanceof File) files.push(file);
    }
    return files;
  }

  function inputFiles(element) {
    if (elementTag(element) !== 'input' || elementType(element) !== 'file') return null;
    const record = ensureControlState(element);
    if (!record.__vixenFiles) record.__vixenFiles = new FileList([]);
    return record.__vixenFiles;
  }

  function setInputFiles(element, value) {
    if (elementTag(element) !== 'input' || elementType(element) !== 'file') return;
    const files = filesFromList(value);
    const record = ensureControlState(element);
    record.__vixenFiles = new FileList(files);
    record.value = files.length > 0 ? 'C:\\fakepath\\' + files[0].name : '';
    record.__vixenValueDirty = true;
    commitControlValue(element);
  }

  function isValueControl(element) {
    if (!element || typeof element.__vixenNodeId !== 'number') return false;
    const tag = elementTag(element);
    return tag === 'input' || tag === 'textarea' || tag === 'select' || tag === 'option' || tag === 'button';
  }

  function isTextEditableControl(element) {
    if (!element || typeof element.__vixenNodeId !== 'number' || isDisabled(element)) return false;
    const tag = elementTag(element);
    if (tag === 'textarea') return elementAttribute(element.__vixenNodeId, 'readonly') === null;
    if (tag !== 'input') return false;
    if (elementAttribute(element.__vixenNodeId, 'readonly') !== null) return false;
    return textInputTypes.has(elementType(element));
  }

  function isContenteditableHost(element) {
    return element !== null && contenteditableHost(element) === element;
  }

  function isPlatformTextEditable(element) {
    return isTextEditableControl(element) || isContenteditableHost(element);
  }

  function controlValue(element) {
    if (!isValueControl(element)) return '';
    const tag = elementTag(element);
    if (tag === 'select') return selectedOptionValueLive(element);
    if (tag === 'option') return optionValue(element);
    return ensureControlState(element).value;
  }

  function clampControlOffset(element, value) {
    const length = controlValue(element).length;
    const n = Number(value);
    if (!Number.isFinite(n)) return 0;
    return Math.min(length, Math.max(0, Math.trunc(n)));
  }

  function controlSelection(element) {
    const record = ensureControlState(element);
    const length = controlValue(element).length;
    let start = Number(record.selectionStart);
    let end = Number(record.selectionEnd);
    if (!Number.isFinite(start)) start = length;
    if (!Number.isFinite(end)) end = start;
    start = Math.min(length, Math.max(0, Math.trunc(start)));
    end = Math.min(length, Math.max(0, Math.trunc(end)));
    if (start > end) end = start;
    record.selectionStart = start;
    record.selectionEnd = end;
    return [start, end];
  }

  function setControlSelection(element, start, end = start) {
    const record = ensureControlState(element);
    let nextStart = clampControlOffset(element, start);
    let nextEnd = clampControlOffset(element, end);
    if (nextStart > nextEnd) nextEnd = nextStart;
    record.selectionStart = nextStart;
    record.selectionEnd = nextEnd;
    if (record.nodeId > 0 && isTextEditableControl(element)) {
      unwrapDomOp(op_vixen_dom_set_control_selection(
        record.nodeId,
        record.id || '',
        recordAttr(record, 'name') || '',
        elementTag(element),
        nextStart,
        nextEnd,
      ));
    }
  }

  function commitControlValue(element) {
    const record = ensureControlState(element);
    if (record.nodeId > 0) {
      const tag = elementTag(element);
      if (tag === 'input' || tag === 'textarea') {
        unwrapDomOp(op_vixen_dom_set_control_value(
          record.nodeId,
          record.id || '',
          recordAttr(record, 'name') || '',
          tag,
          record.value,
        ));
      }
    } else {
      commitNearestConnectedAncestor(record);
    }
  }

  function dispatchValueEvents(element, inputType, dataValue) {
    element.dispatchEvent(new InputEvent('input', {
      bubbles: true,
      composed: true,
      data: dataValue === undefined ? null : dataValue,
      inputType: inputType || '',
    }));
    element.dispatchEvent(new Event('change', { bubbles: true, composed: true }));
  }

  function applyControlValue(element, value, selectionStart, selectionEnd, inputType = '', dataValue = null, dispatchEvents = false) {
    const record = ensureControlState(element);
    record.value = String(value);
    record.__vixenValueDirty = true;
    commitControlValue(element);
    setControlSelection(element, selectionStart, selectionEnd);
    if (dispatchEvents) dispatchValueEvents(element, inputType, dataValue);
    return record.value;
  }

  function ensureContenteditableState(element) {
    const record = elementRecord(element);
    if (!record.__vixenContenteditableState) {
      const value = elementText(element.__vixenNodeId);
      record.__vixenEditableValue = value;
      record.__vixenEditableSelectionStart = value.length;
      record.__vixenEditableSelectionEnd = value.length;
      record.__vixenContenteditableState = true;
    }
    return record;
  }

  function contenteditableSelection(element) {
    const record = ensureContenteditableState(element);
    const length = record.__vixenEditableValue.length;
    let start = Number(record.__vixenEditableSelectionStart);
    let end = Number(record.__vixenEditableSelectionEnd);
    if (!Number.isFinite(start)) start = length;
    if (!Number.isFinite(end)) end = start;
    start = Math.min(length, Math.max(0, Math.trunc(start)));
    end = Math.min(length, Math.max(0, Math.trunc(end)));
    record.__vixenEditableSelectionStart = start;
    record.__vixenEditableSelectionEnd = end;
    return [start, end];
  }

  function setContenteditableState(element, value, selectionStart, selectionEnd) {
    const record = ensureContenteditableState(element);
    const text = String(value);
    let start = Math.min(text.length, Math.max(0, Math.trunc(Number(selectionStart) || 0)));
    let end = Math.min(text.length, Math.max(0, Math.trunc(Number(selectionEnd) || 0)));
    if (record.__vixenEditableValue !== text) {
      const oldSerialized = serializeElementRecord(record);
      const oldText = record.textContent || '';
      const removed = nodesFromIds((record.childNodeIds || record.childElementNodeIds || []).slice());
      record.textContent = text;
      record.innerHTML = escapeTextForHtml(text);
      record.childNodeIds = [];
      record.childElementNodeIds = [];
      record.firstElementChildNodeId = null;
      record.lastElementChildNodeId = null;
      const parent = parentElementRecord(record);
      if (parent) parent.textContent = replaceFirst(parent.textContent || '', oldText, text);
      queueChildListMutation(element, [], removed);
      propagateSerializedChange(record, oldSerialized);
    }
    record.__vixenEditableValue = text;
    record.__vixenEditableSelectionStart = start;
    record.__vixenEditableSelectionEnd = end;
    unwrapDomOp(op_vixen_dom_set_contenteditable_state(record.nodeId, text, start, end));
    return text;
  }

  function platformTextValue(element) {
    return isContenteditableHost(element)
      ? ensureContenteditableState(element).__vixenEditableValue
      : controlValue(element);
  }

  function platformTextSelection(element) {
    return isContenteditableHost(element) ? contenteditableSelection(element) : controlSelection(element);
  }

  function setPlatformTextSelection(element, start, end = start) {
    if (isContenteditableHost(element)) {
      return setContenteditableState(element, platformTextValue(element), start, end);
    }
    setControlSelection(element, start, end);
    return platformTextValue(element);
  }

  function applyPlatformTextValue(element, value, selectionStart, selectionEnd, inputType = '', dataValue = null, dispatchEvents = false) {
    if (!isContenteditableHost(element)) {
      return applyControlValue(element, value, selectionStart, selectionEnd, inputType, dataValue, dispatchEvents);
    }
    const result = setContenteditableState(element, value, selectionStart, selectionEnd);
    if (dispatchEvents) dispatchValueEvents(element, inputType, dataValue);
    return result;
  }

  function setControlValue(element, value) {
    const text = String(value);
    const tag = elementTag(element);
    if (tag === 'select') return setSelectValue(element, text);
    if (tag === 'option') {
      setElementAttribute(element.__vixenNodeId, 'value', text);
      return text;
    }
    return applyControlValue(element, text, text.length, text.length);
  }

  function insertTextIntoControl(element, text) {
    if (!isPlatformTextEditable(element)) return false;
    const input = String(text);
    if (input === '') return false;
    const beforeInput = new InputEvent('beforeinput', {
      bubbles: true,
      cancelable: true,
      composed: true,
      data: input,
      inputType: 'insertText',
      isComposing: false,
    });
    if (!element.dispatchEvent(beforeInput)) return false;
    const value = platformTextValue(element);
    const [start, end] = platformTextSelection(element);
    const next = value.slice(0, start) + input + value.slice(end);
    const caret = start + input.length;
    applyPlatformTextValue(element, next, caret, caret, 'insertText', input, true);
    return true;
  }

  function deleteTextFromControl(element, direction) {
    if (!isPlatformTextEditable(element)) return false;
    const value = platformTextValue(element);
    let [start, end] = platformTextSelection(element);
    if (start === end) {
      if (direction === 'backward') {
        if (start === 0) return false;
        start -= 1;
      } else {
        if (end >= value.length) return false;
        end += 1;
      }
    }
    const next = value.slice(0, start) + value.slice(end);
    const inputType = direction === 'backward' ? 'deleteContentBackward' : 'deleteContentForward';
    applyPlatformTextValue(element, next, start, start, inputType, null, true);
    return true;
  }

  function moveControlCaret(element, key) {
    if (!isPlatformTextEditable(element)) return false;
    const value = platformTextValue(element);
    const [start, end] = platformTextSelection(element);
    let next = end;
    if (key === 'ArrowLeft') next = Math.max(0, start - 1);
    else if (key === 'ArrowRight') next = Math.min(value.length, end + 1);
    else if (key === 'Home') next = 0;
    else if (key === 'End') next = value.length;
    else return false;
    setPlatformTextSelection(element, next, next);
    return true;
  }

  function handleKeyboardDefault(target, event) {
    if (!target || typeof target.__vixenNodeId !== 'number' || event.type !== 'keydown') return;
    if (!isPlatformTextEditable(target)) return;
    const key = String(event.key || '');
    if ((event.ctrlKey || event.metaKey) && key.toLowerCase() === 'a') {
      setPlatformTextSelection(target, 0, platformTextValue(target).length);
      return;
    }
    if (event.__vixenApplyText && event.__vixenInputText) {
      insertTextIntoControl(target, event.__vixenInputText);
      return;
    }
    if (key === 'Backspace') {
      deleteTextFromControl(target, 'backward');
    } else if (key === 'Delete') {
      deleteTextFromControl(target, 'forward');
    } else if (key === 'Enter') {
      if (elementTag(target) === 'textarea' || isContenteditableHost(target)) insertTextIntoControl(target, '\n');
      else submitFormDefault(findOwnerForm(target), null);
    } else {
      moveControlCaret(target, key);
    }
  }

  function isDisableableFormElementTag(tag) {
    return tag === 'button' || tag === 'fieldset' || tag === 'input' || tag === 'optgroup' || tag === 'option' || tag === 'select' || tag === 'textarea';
  }

  function collectLiveFormEntries(element, disabledAncestor, entries) {
    const tag = elementTag(element);
    const disabledHere = disabledAncestor || (isDisableableFormElementTag(tag) && isDisabled(element));
    if (!disabledHere) {
      const entry = liveFormEntryForControl(element, tag);
      if (entry) entries.push(entry);
    }
    for (const child of element.children) collectLiveFormEntries(child, disabledHere, entries);
  }

  function liveFormEntryForControl(element, tag) {
    const name = elementName(element);
    if (name === '') return null;
    if (tag === 'input') return liveInputFormEntry(element, name);
    if (tag === 'textarea') return { name, kind: 'text', value: controlValue(element) };
    if (tag === 'select') return { name, kind: 'text', value: selectedOptionValueLive(element) };
    return null;
  }

  function liveInputFormEntry(element, name) {
    const type = elementType(element) || 'text';
    if (type === 'button' || type === 'reset' || type === 'submit' || type === 'image') return null;
    if (type === 'checkbox' || type === 'radio') {
      return element.checked ? { name, kind: 'text', value: elementAttribute(element.__vixenNodeId, 'value') || 'on' } : null;
    }
    if (type === 'file') {
      const files = inputFiles(element);
      const file = files && files.item(0);
      return {
        name,
        kind: 'file',
        file: file || null,
        filename: file ? file.name : controlValue(element),
        type: file ? file.type : 'application/octet-stream',
        size: file ? file.size : 0,
      };
    }
    return { name, kind: 'text', value: controlValue(element) };
  }

  function selectedOptionValueLive(select) {
    const selected = selectedOptionElements(select)[0];
    return selected ? optionValue(selected) : '';
  }

  function optionElements(select) {
    if (!select || typeof select.__vixenNodeId !== 'number' || elementTag(select) !== 'select') return [];
    return Array.from(select.querySelectorAll('option'));
  }

  function optionValue(option) {
    return elementAttribute(option.__vixenNodeId, 'value') || elementText(option.__vixenNodeId);
  }

  function optionLabel(option) {
    return elementAttribute(option.__vixenNodeId, 'label') || option.textContent;
  }

  function optionDefaultSelected(option) {
    const record = ensureControlState(option);
    if (record.defaultSelected === undefined) record.defaultSelected = booleanAttribute(option, 'selected');
    return Boolean(record.defaultSelected);
  }

  function setOptionDefaultSelected(option, selected) {
    const record = ensureControlState(option);
    record.defaultSelected = Boolean(selected);
    if (selected) setElementAttribute(option.__vixenNodeId, 'selected', '');
    else removeElementAttribute(option.__vixenNodeId, 'selected');
  }

  function selectedOptionElements(select) {
    const options = optionElements(select);
    const selected = options.filter((option) => option.hasAttribute('selected'));
    if (selected.length > 0) return selected;
    return options.length > 0 ? [options[0]] : [];
  }

  function optionIndex(option) {
    const parent = option.parentElement;
    if (!parent || elementTag(parent) !== 'select') return 0;
    return optionElements(parent).indexOf(option);
  }

  function setOptionSelected(option, selected) {
    if (elementTag(option) !== 'option') return;
    optionDefaultSelected(option);
    const parent = option.parentElement;
    if (selected && parent && elementTag(parent) === 'select' && !parent.multiple) {
      for (const sibling of optionElements(parent)) {
        optionDefaultSelected(sibling);
        if (sibling !== option) sibling.removeAttribute('selected');
      }
    }
    if (selected) option.setAttribute('selected', '');
    else option.removeAttribute('selected');
  }

  function inputDefaultChecked(input) {
    const record = ensureControlState(input);
    if (record.defaultChecked === undefined) record.defaultChecked = booleanAttribute(input, 'checked');
    return Boolean(record.defaultChecked);
  }

  function setInputDefaultChecked(input, checked) {
    const record = ensureControlState(input);
    record.defaultChecked = Boolean(checked);
    if (checked) setElementAttribute(input.__vixenNodeId, 'checked', '');
    else removeElementAttribute(input.__vixenNodeId, 'checked');
  }

  function setInputChecked(input, checked) {
    inputDefaultChecked(input);
    if (checked) setElementAttribute(input.__vixenNodeId, 'checked', '');
    else removeElementAttribute(input.__vixenNodeId, 'checked');
  }

  function selectSelectedIndex(select) {
    const options = optionElements(select);
    if (options.length === 0) return -1;
    const selected = options.findIndex((option) => option.hasAttribute('selected'));
    return selected === -1 ? 0 : selected;
  }

  function setSelectSelectedIndex(select, index) {
    const options = optionElements(select);
    const n = Number(index);
    if (!Number.isInteger(n) || n < 0 || n >= options.length) {
      for (const option of options) option.removeAttribute('selected');
      return -1;
    }
    setOptionSelected(options[n], true);
    return n;
  }

  function setSelectValue(select, value) {
    const text = String(value);
    const options = optionElements(select);
    const matches = options.filter((option) => optionValue(option) === text);
    if (matches.length === 0) {
      setSelectSelectedIndex(select, -1);
      return '';
    }
    if (select.multiple) {
      for (const option of options) setOptionSelected(option, matches.includes(option));
    } else {
      setOptionSelected(matches[0], true);
    }
    return controlValue(select);
  }

  function sameRadioGroup(a, b) {
    if (a === b || String(elementRecord(b).tag).toLowerCase() !== 'input') return false;
    if (elementType(b) !== 'radio') return false;
    const name = elementName(a);
    if (name === '' || elementName(b) !== name) return false;
    return a.closest('form') === b.closest('form');
  }

  function findOwnerForm(element) {
    return typeof element.closest === 'function' ? element.closest('form') : null;
  }

  function submitFormDefault(form, submitter) {
    if (!form) return;
    const bypassValidation = form.noValidate || Boolean(submitter && submitter.formNoValidate);
    if (!bypassValidation && !checkValidityElement(form)) return;
    const submitEvent = new SubmitEvent('submit', {
      bubbles: true,
      cancelable: true,
      composed: true,
      submitter: submitter || null,
    });
    if (form.dispatchEvent(submitEvent)) queueFormSubmission(form, submitter);
  }

  function queueFormSubmission(form, submitter) {
    if (!form) return;
    const submitterAction = submitter ? elementAttribute(submitter.__vixenNodeId, 'formaction') : null;
    const submitterMethod = submitter ? elementAttribute(submitter.__vixenNodeId, 'formmethod') : null;
    const submitterEnctype = submitter ? elementAttribute(submitter.__vixenNodeId, 'formenctype') : null;
    const action = submitterAction || elementAttribute(form.__vixenNodeId, 'action') || currentUrl;
    queueNavigationAction({
      type: 'form-submit',
      formId: form.id || '',
      formNodeId: form.__vixenNodeId,
      submitterNodeId: submitter ? submitter.__vixenNodeId : 0,
      submitterId: submitter ? (submitter.id || '') : '',
      action: resolveNavigationUrl(action, data.baseURI || currentUrl),
      method: (submitterMethod || elementAttribute(form.__vixenNodeId, 'method') || 'get').toLowerCase(),
      enctype: (submitterEnctype || elementAttribute(form.__vixenNodeId, 'enctype') || 'application/x-www-form-urlencoded').toLowerCase(),
    });
  }

  function resetFormDefault(form) {
    if (!form) return;
    const resetEvent = new Event('reset', { bubbles: true, cancelable: true, composed: true });
    if (form.dispatchEvent(resetEvent)) resetFormControls(form);
  }

  function resetFormControls(form) {
    for (const control of Array.from(form.querySelectorAll('input,textarea,select'))) resetControl(control);
  }

  function resetControl(control) {
    const tag = elementTag(control);
    if (tag === 'input') {
      const type = elementType(control) || 'text';
      if (type === 'checkbox' || type === 'radio') {
        setInputChecked(control, inputDefaultChecked(control));
        return;
      }
      if (type === 'file') {
        setInputFiles(control, []);
        return;
      }
      const record = ensureControlState(control);
      applyControlValue(control, record.defaultValue || '', (record.defaultValue || '').length, (record.defaultValue || '').length);
      return;
    }
    if (tag === 'textarea') {
      const record = ensureControlState(control);
      applyControlValue(control, record.defaultValue || '', (record.defaultValue || '').length, (record.defaultValue || '').length);
      return;
    }
    if (tag === 'select') {
      for (const option of optionElements(control)) setOptionSelected(option, optionDefaultSelected(option));
    }
  }

  const validityFlagNames = [
    'valueMissing', 'typeMismatch', 'patternMismatch', 'tooLong', 'tooShort',
    'rangeUnderflow', 'rangeOverflow', 'stepMismatch', 'badInput', 'customError',
  ];

  class VixenValidityState {
    constructor(flags) {
      for (const name of validityFlagNames) defineReadonlyValue(this, name, Boolean(flags && flags[name]));
    }
    get valid() { return validityFlagNames.every((name) => !this[name]); }
  }

  function emptyValidityFlags() {
    const flags = {};
    for (const name of validityFlagNames) flags[name] = false;
    return flags;
  }

  function mergeValidityFlags(target, source) {
    for (const name of validityFlagNames) target[name] = Boolean(target[name] || source[name]);
    return target;
  }

  function willValidateElement(element) {
    if (!element || typeof element.__vixenNodeId !== 'number' || isDisabled(element)) return false;
    const tag = elementTag(element);
    if (tag === 'input') {
      const type = elementType(element);
      return !['hidden', 'button', 'reset', 'submit', 'image'].includes(type) && elementAttribute(element.__vixenNodeId, 'readonly') === null;
    }
    if (tag === 'select') return true;
    if (tag === 'textarea') return elementAttribute(element.__vixenNodeId, 'readonly') === null;
    return false;
  }

  function validityStateForElementOrForm(element) {
    return new VixenValidityState(elementTag(element) === 'form'
      ? formValidityFlags(element)
      : elementValidityFlags(element));
  }

  function formValidityFlags(form) {
    const aggregate = emptyValidityFlags();
    for (const control of formControlElements(form)) mergeValidityFlags(aggregate, elementValidityFlags(control));
    return aggregate;
  }

  function formControlElements(form) {
    return Array.from(form.querySelectorAll('input,select,textarea'));
  }

  function elementValidityFlags(element) {
    const flags = emptyValidityFlags();
    const record = elementRecord(element);
    if (record.__vixenCustomValidityMessage) flags.customError = true;
    if (!willValidateElement(element)) return flags;

    const value = controlValue(element);
    const type = elementType(element);
    if (element.required) {
      const missing = type === 'checkbox' || type === 'radio' ? !element.checked : value === '';
      if (missing) flags.valueMissing = true;
    }

    if (value !== '') {
      if (type === 'email' && !emailIsValid(value)) flags.typeMismatch = true;
      else if (type === 'url' && !urlIsValid(value)) flags.typeMismatch = true;
      else if (type === 'number' || type === 'range') applyNumericValidity(element, value, flags);
      applyLengthValidity(element, value, flags);
    }
    return flags;
  }

  function emailIsValid(value) {
    const text = String(value);
    const parts = text.split('@');
    if (parts.length !== 2 || parts[0] === '') return false;
    return parts[1].includes('.') && !parts[1].startsWith('.') && !parts[1].endsWith('.');
  }

  function urlIsValid(value) {
    const text = String(value);
    const colon = text.indexOf(':');
    if (colon <= 0 || !/^[A-Za-z]+$/.test(text.slice(0, colon))) return false;
    const rest = text.slice(colon + 1);
    return rest.startsWith('//') && rest.slice(2).split('/')[0] !== '';
  }

  function parseNumberAttr(element, name) {
    const raw = elementAttribute(element.__vixenNodeId, name);
    if (raw === null || raw === '') return null;
    const number = Number(raw);
    return Number.isFinite(number) ? number : null;
  }

  function applyNumericValidity(element, value, flags) {
    const number = Number(value);
    if (!Number.isFinite(number)) {
      flags.badInput = true;
      return;
    }
    const min = parseNumberAttr(element, 'min');
    const max = parseNumberAttr(element, 'max');
    if (min !== null && number < min) flags.rangeUnderflow = true;
    if (max !== null && number > max) flags.rangeOverflow = true;
    const rawStep = elementAttribute(element.__vixenNodeId, 'step');
    if (rawStep !== null && rawStep.toLowerCase() !== 'any') {
      const step = Number(rawStep);
      if (Number.isFinite(step) && step > 0) {
        const base = min === null ? 0 : min;
        const n = Math.round((number - base) / step);
        flags.stepMismatch = Math.abs(number - (base + n * step)) > 1e-9 * Math.abs(step);
      }
    }
  }

  function parseNonNegativeIntAttr(element, name) {
    const raw = elementAttribute(element.__vixenNodeId, name);
    if (raw === null || raw === '') return null;
    const number = Number.parseInt(raw, 10);
    return Number.isFinite(number) && number >= 0 ? number : null;
  }

  function applyLengthValidity(element, value, flags) {
    const length = Array.from(String(value)).length;
    const min = parseNonNegativeIntAttr(element, 'minlength');
    const max = parseNonNegativeIntAttr(element, 'maxlength');
    if (min !== null && length < min) flags.tooShort = true;
    if (max !== null && length > max) flags.tooLong = true;
  }

  function checkValidityElement(element) {
    if (elementTag(element) === 'form') {
      let valid = true;
      for (const control of formControlElements(element)) {
        if (!checkValidityElement(control)) valid = false;
      }
      return valid;
    }
    const valid = validityStateForElementOrForm(element).valid;
    if (!valid && willValidateElement(element)) {
      element.dispatchEvent(new Event('invalid', { cancelable: true }));
    }
    return valid;
  }

  function runDomDefaultAction(target, event) {
    if (!target || typeof target.__vixenNodeId !== 'number') return;
    if (event.type === 'keydown') {
      handleKeyboardDefault(target, event);
      return;
    }
    if (event.type !== 'click') return;
    const tag = String(elementRecord(target).tag).toLowerCase();
    if (isDisabled(target)) return;
    if (tag === 'input' || tag === 'textarea' || tag === 'select' || tag === 'button') target.focus();
    else {
      const editingHost = contenteditableHost(target);
      if (editingHost) editingHost.focus();
    }
    if (tag === 'input') {
      const type = elementType(target) || 'text';
      if (type === 'checkbox') {
        if (target.checked) target.removeAttribute('checked');
        else target.setAttribute('checked', '');
        target.dispatchEvent(new Event('input', { bubbles: true, composed: true }));
        target.dispatchEvent(new Event('change', { bubbles: true, composed: true }));
        return;
      }
      if (type === 'radio') {
        for (const radio of document.querySelectorAll('input')) {
          if (sameRadioGroup(target, radio)) radio.removeAttribute('checked');
        }
        target.setAttribute('checked', '');
        target.dispatchEvent(new Event('input', { bubbles: true, composed: true }));
        target.dispatchEvent(new Event('change', { bubbles: true, composed: true }));
        return;
      }
      if (type === 'submit') submitFormDefault(findOwnerForm(target), target);
      if (type === 'reset') resetFormDefault(findOwnerForm(target));
      return;
    }
    if (tag === 'button') {
      const type = elementType(target) || 'submit';
      if (type === 'submit') submitFormDefault(findOwnerForm(target), target);
      if (type === 'reset') resetFormDefault(findOwnerForm(target));
      return;
    }
    if (tag === 'a') {
      const href = elementAttribute(target.__vixenNodeId, 'href');
      if (href !== null) queueNavigationAction({ type: 'navigate', url: resolveNavigationUrl(href, data.baseURI || currentUrl), replace: false });
    }
  }

  class VixenElement {
    constructor(nodeId) {
      Object.defineProperty(this, '__vixenNodeId', {
        value: nodeId,
        enumerable: false,
      });
    }
    get id() { return elementRecord(this).id || ''; }
    set id(value) { setElementAttribute(this.__vixenNodeId, 'id', String(value)); }
    get className() { return elementRecord(this).className; }
    set className(value) { setElementAttribute(this.__vixenNodeId, 'class', String(value)); }
    get tagName() { return elementRecord(this).tag.toUpperCase(); }
    get nodeName() { return this.tagName; }
    get localName() { return elementRecord(this).tag; }
    get namespaceURI() { return 'http://www.w3.org/1999/xhtml'; }
    get prefix() { return null; }
    get nodeType() { return 1; }
    get isConnected() { return elementRecord(this).isConnected !== false; }
    get ownerDocument() { return vixenDocument; }
    getRootNode() { return vixenDocument; }
    contains(target) { return nodeContains(this, target); }
    get parentNode() {
      const parentId = elementRecord(this).parentNodeId;
      if ((parentId === null || parentId === undefined) && this.__vixenNodeId === data.documentElementNodeId) return vixenDocument;
      return wrapElementByNodeId(parentId);
    }
    get parentElement() {
      const parent = this.parentNode;
      return parent && parent.nodeType === 1 ? parent : null;
    }
    get childNodes() { return new VixenNodeList(childIds(elementRecord(this))); }
    get children() { return new VixenHTMLCollection(elementRecord(this).childElementNodeIds); }
    get firstChild() {
      const ids = childIds(elementRecord(this));
      return wrapNodeById(ids.length ? ids[0] : null);
    }
    get lastChild() {
      const ids = childIds(elementRecord(this));
      return wrapNodeById(ids.length ? ids[ids.length - 1] : null);
    }
    get firstElementChild() { return wrapElementByNodeId(elementRecord(this).firstElementChildNodeId); }
    get lastElementChild() { return wrapElementByNodeId(elementRecord(this).lastElementChildNodeId); }
    get childElementCount() { return elementRecord(this).childElementNodeIds.length; }
    get previousSibling() { return wrapNodeById(elementRecord(this).previousSiblingNodeId ?? elementRecord(this).previousElementSiblingNodeId); }
    get nextSibling() { return wrapNodeById(elementRecord(this).nextSiblingNodeId ?? elementRecord(this).nextElementSiblingNodeId); }
    get previousElementSibling() { return wrapElementByNodeId(elementRecord(this).previousElementSiblingNodeId); }
    get nextElementSibling() { return wrapElementByNodeId(elementRecord(this).nextElementSiblingNodeId); }
    get clientWidth() { return elementClientWidth(this); }
    get clientHeight() { return elementClientHeight(this); }
    get clientTop() { return 0; }
    get clientLeft() { return 0; }
    get scrollWidth() { return elementScrollSize(this, 'x'); }
    get scrollHeight() { return elementScrollSize(this, 'y'); }
    get scrollTop() {
      syncRendererScrollState(false);
      if (this === vixenDocument.documentElement || this === vixenDocument.body) return topLevelScrollY;
      return Number(elementRecord(this).scrollTop) || 0;
    }
    set scrollTop(value) {
      if (this === vixenDocument.documentElement || this === vixenDocument.body) applyTopLevelScroll(topLevelScrollX, value);
      else applyElementScroll(this, this.scrollLeft, value);
    }
    get scrollLeft() {
      syncRendererScrollState(false);
      if (this === vixenDocument.documentElement || this === vixenDocument.body) return topLevelScrollX;
      return Number(elementRecord(this).scrollLeft) || 0;
    }
    set scrollLeft(value) {
      if (this === vixenDocument.documentElement || this === vixenDocument.body) applyTopLevelScroll(value, topLevelScrollY);
      else applyElementScroll(this, value, this.scrollTop);
    }
    scroll(leftOrOptions = 0, top = 0) { elementScrollTo(this, leftOrOptions, top); }
    scrollTo(leftOrOptions = 0, top = 0) { elementScrollTo(this, leftOrOptions, top); }
    scrollBy(leftOrOptions = 0, top = 0) { elementScrollBy(this, leftOrOptions, top); }
    get offsetWidth() { const rect = elementRect(this.__vixenNodeId); return rect ? integerCssPixels(rect.width) : 0; }
    get offsetHeight() { const rect = elementRect(this.__vixenNodeId); return rect ? integerCssPixels(rect.height) : 0; }
    get offsetTop() { const rect = elementRect(this.__vixenNodeId); return rect ? Math.trunc(Number(rect.y) || 0) : 0; }
    get offsetLeft() { const rect = elementRect(this.__vixenNodeId); return rect ? Math.trunc(Number(rect.x) || 0) : 0; }
    get offsetParent() { return this === vixenDocument.body || this === vixenDocument.documentElement ? null : vixenDocument.body; }
    get textContent() { return elementText(this.__vixenNodeId); }
    set textContent(value) { setElementText(this.__vixenNodeId, value); }
    get innerText() { return elementText(this.__vixenNodeId); }
    set innerText(value) { setElementText(this.__vixenNodeId, value); }
    get text() { return elementText(this.__vixenNodeId); }
    set text(value) { setElementText(this.__vixenNodeId, value); }
    get innerHTML() { return elementRecord(this).innerHTML; }
    set innerHTML(value) { setElementInnerHTML(this, String(value)); }
    get outerHTML() { return serializeElementRecord(elementRecord(this)); }
    get attributes() { return new VixenNamedNodeMap(this); }
    get classList() {
      return cachedElementObject(this, '__vixenClassList', () => new VixenDOMTokenList(this, 'class'));
    }
    get relList() {
      return cachedElementObject(this, '__vixenRelList', () => new VixenDOMTokenList(this, 'rel'));
    }
    get sandbox() {
      return cachedElementObject(this, '__vixenSandboxList', () => new VixenDOMTokenList(this, 'sandbox'));
    }
    get style() { return cachedElementObject(this, '__vixenStyle', () => new VixenInlineStyle(this)); }
    get sheet() {
      const tag = String(elementRecord(this).tag || '').toLowerCase();
      if (tag !== 'style' && tag !== 'link') return null;
      return cachedElementObject(this, '__vixenSheet', () => makeStyleSheetObject(this));
    }
    get dataset() {
      return cachedElementObject(this, '__vixenDataset', () => new VixenDOMStringMap(elementDataset(this.__vixenNodeId)));
    }
    get hidden() { return booleanAttribute(this, 'hidden'); }
    set hidden(value) { setBooleanAttribute(this, 'hidden', value); }
    get tabIndex() { return reflectedInteger(this, 'tabindex', -1); }
    set tabIndex(value) { setReflectedInteger(this, 'tabindex', value); }
    get accessKey() { return reflectedAttribute(this, 'accesskey'); }
    set accessKey(value) { setReflectedAttribute(this, 'accesskey', value); }
    get accessKeyLabel() { return this.accessKey; }
    get draggable() { return reflectedBooleanKeyword(this, 'draggable', ['true'], ['false'], false); }
    set draggable(value) { setReflectedAttribute(this, 'draggable', Boolean(value) ? 'true' : 'false'); }
    get spellcheck() { return reflectedBooleanKeyword(this, 'spellcheck', ['', 'true'], ['false'], true); }
    set spellcheck(value) { setReflectedAttribute(this, 'spellcheck', Boolean(value) ? 'true' : 'false'); }
    get translate() { return reflectedBooleanKeyword(this, 'translate', ['', 'yes', 'true'], ['no', 'false'], true); }
    set translate(value) { setReflectedAttribute(this, 'translate', Boolean(value) ? 'yes' : 'no'); }
    get inputMode() { return reflectedAttribute(this, 'inputmode'); }
    set inputMode(value) { setReflectedAttribute(this, 'inputmode', value); }
    get enterKeyHint() { return reflectedAttribute(this, 'enterkeyhint'); }
    set enterKeyHint(value) { setReflectedAttribute(this, 'enterkeyhint', value); }
    get popover() { return reflectedAttribute(this, 'popover'); }
    set popover(value) { setReflectedAttribute(this, 'popover', value); }
    get title() { return reflectedAttribute(this, 'title'); }
    set title(value) { setReflectedAttribute(this, 'title', value); }
    get lang() { return reflectedAttribute(this, 'lang'); }
    set lang(value) { setReflectedAttribute(this, 'lang', value); }
    get dir() { return reflectedAttribute(this, 'dir'); }
    set dir(value) { setReflectedAttribute(this, 'dir', value); }
    get type() { return elementType(this); }
    set type(value) { setReflectedAttribute(this, 'type', String(value).toLowerCase()); }
    get name() { return elementName(this); }
    set name(value) { setReflectedAttribute(this, 'name', value); }
    get content() { return templateContentFragment(this); }
    set content(value) { setTemplateContent(this, value); }
    get httpEquiv() { return reflectedAttribute(this, 'http-equiv'); }
    set httpEquiv(value) { setReflectedAttribute(this, 'http-equiv', value); }
    get charset() { return reflectedAttribute(this, 'charset'); }
    set charset(value) { setReflectedAttribute(this, 'charset', value); }
    get method() { return reflectedFormMethod(this); }
    set method(value) { setReflectedAttribute(this, 'method', value); }
    get enctype() { return reflectedFormEnctype(this); }
    set enctype(value) { setReflectedAttribute(this, 'enctype', value); }
    get encoding() { return this.enctype; }
    set encoding(value) { this.enctype = value; }
    get action() { return reflectedAttribute(this, 'action'); }
    set action(value) { setReflectedAttribute(this, 'action', value); }
    get accept() { return reflectedAttribute(this, 'accept'); }
    set accept(value) { setReflectedAttribute(this, 'accept', value); }
    get acceptCharset() { return reflectedAttribute(this, 'accept-charset'); }
    set acceptCharset(value) { setReflectedAttribute(this, 'accept-charset', value); }
    get noValidate() { return booleanAttribute(this, 'novalidate'); }
    set noValidate(value) { setBooleanAttribute(this, 'novalidate', value); }
    get formAction() { return reflectedAttribute(this, 'formaction'); }
    set formAction(value) { setReflectedAttribute(this, 'formaction', value); }
    get formEnctype() { return reflectedSubmitEnctype(this); }
    set formEnctype(value) { setReflectedAttribute(this, 'formenctype', value); }
    get formMethod() { return reflectedSubmitMethod(this); }
    set formMethod(value) { setReflectedAttribute(this, 'formmethod', value); }
    get formNoValidate() { return booleanAttribute(this, 'formnovalidate'); }
    set formNoValidate(value) { setBooleanAttribute(this, 'formnovalidate', value); }
    get formTarget() { return reflectedAttribute(this, 'formtarget'); }
    set formTarget(value) { setReflectedAttribute(this, 'formtarget', value); }
    get cite() { return reflectedAttribute(this, 'cite'); }
    set cite(value) { setReflectedAttribute(this, 'cite', value); }
    get dateTime() { return reflectedAttribute(this, 'datetime'); }
    set dateTime(value) { setReflectedAttribute(this, 'datetime', value); }
    get reversed() { return booleanAttribute(this, 'reversed'); }
    set reversed(value) { setBooleanAttribute(this, 'reversed', value); }
    get start() { return reflectedInteger(this, 'start', 1); }
    set start(value) { setReflectedInteger(this, 'start', value); }
    get href() { return elementUrlPart(this, 'href', 'href'); }
    set href(value) { setReflectedAttribute(this, 'href', value); }
    get origin() { return elementUrlPart(this, 'href', 'origin'); }
    get protocol() { return elementUrlPart(this, 'href', 'protocol'); }
    get host() { return elementUrlPart(this, 'href', 'host'); }
    get hostname() { return elementUrlPart(this, 'href', 'hostname'); }
    get port() { return elementUrlPart(this, 'href', 'port'); }
    get pathname() { return elementUrlPart(this, 'href', 'pathname'); }
    get search() { return elementUrlPart(this, 'href', 'search'); }
    get hash() { return elementUrlPart(this, 'href', 'hash'); }
    get target() { return reflectedAttribute(this, 'target'); }
    set target(value) { setReflectedAttribute(this, 'target', value); }
    get download() { return reflectedAttribute(this, 'download'); }
    set download(value) { setReflectedAttribute(this, 'download', value); }
    get rel() { return reflectedAttribute(this, 'rel'); }
    set rel(value) { setReflectedAttribute(this, 'rel', value); }
    get hreflang() { return reflectedAttribute(this, 'hreflang'); }
    set hreflang(value) { setReflectedAttribute(this, 'hreflang', value); }
    get coords() { return reflectedAttribute(this, 'coords'); }
    set coords(value) { setReflectedAttribute(this, 'coords', value); }
    get shape() { return reflectedAttribute(this, 'shape'); }
    set shape(value) { setReflectedAttribute(this, 'shape', value); }
    get src() { return reflectedAttribute(this, 'src'); }
    set src(value) { setReflectedAttribute(this, 'src', value); }
    get srcset() { return reflectedAttribute(this, 'srcset'); }
    set srcset(value) { setReflectedAttribute(this, 'srcset', value); }
    get sizes() { return reflectedAttribute(this, 'sizes'); }
    set sizes(value) { setReflectedAttribute(this, 'sizes', value); }
    get media() { return reflectedAttribute(this, 'media'); }
    set media(value) { setReflectedAttribute(this, 'media', value); }
    get as() { return reflectedAttribute(this, 'as'); }
    set as(value) { setReflectedAttribute(this, 'as', value); }
    get async() { return booleanAttribute(this, 'async'); }
    set async(value) { setBooleanAttribute(this, 'async', value); }
    get defer() { return booleanAttribute(this, 'defer'); }
    set defer(value) { setBooleanAttribute(this, 'defer', value); }
    get noModule() { return booleanAttribute(this, 'nomodule'); }
    set noModule(value) { setBooleanAttribute(this, 'nomodule', value); }
    get kind() { return reflectedAttribute(this, 'kind') || 'subtitles'; }
    set kind(value) { setReflectedAttribute(this, 'kind', value); }
    get srclang() { return reflectedAttribute(this, 'srclang'); }
    set srclang(value) { setReflectedAttribute(this, 'srclang', value); }
    get default() { return booleanAttribute(this, 'default'); }
    set default(value) { setBooleanAttribute(this, 'default', value); }
    get track() { return elementTag(this) === 'track' ? textTrackForTrackElement(this) : undefined; }
    get textTracks() { return mediaTextTracks(this); }
    get srcdoc() { return reflectedAttribute(this, 'srcdoc'); }
    set srcdoc(value) { setReflectedAttribute(this, 'srcdoc', value); }
    get allow() { return reflectedAttribute(this, 'allow'); }
    set allow(value) { setReflectedAttribute(this, 'allow', value); }
    get data() { return reflectedAttribute(this, 'data'); }
    set data(value) { setReflectedAttribute(this, 'data', value); }
    get span() { return reflectedUnsigned(this, 'span') || 1; }
    set span(value) { setReflectedUnsigned(this, 'span', value); }
    get colSpan() { return reflectedUnsigned(this, 'colspan') || 1; }
    set colSpan(value) { setReflectedUnsigned(this, 'colspan', value); }
    get rowSpan() { return reflectedUnsigned(this, 'rowspan') || 1; }
    set rowSpan(value) { setReflectedUnsigned(this, 'rowspan', value); }
    get headers() { return reflectedAttribute(this, 'headers'); }
    set headers(value) { setReflectedAttribute(this, 'headers', value); }
    get scope() { return reflectedAttribute(this, 'scope'); }
    set scope(value) { setReflectedAttribute(this, 'scope', value); }
    get abbr() { return reflectedAttribute(this, 'abbr'); }
    set abbr(value) { setReflectedAttribute(this, 'abbr', value); }
    get contentDocument() { return null; }
    get contentWindow() { return null; }
    get currentSrc() { return imageCurrentSrc(this); }
    get alt() { return reflectedAttribute(this, 'alt'); }
    set alt(value) { setReflectedAttribute(this, 'alt', value); }
    get crossOrigin() { return nullableReflectedAttribute(this, 'crossorigin'); }
    set crossOrigin(value) { value === null ? this.removeAttribute('crossorigin') : setReflectedAttribute(this, 'crossorigin', value); }
    get useMap() { return reflectedAttribute(this, 'usemap'); }
    set useMap(value) { setReflectedAttribute(this, 'usemap', value); }
    get isMap() { return booleanAttribute(this, 'ismap'); }
    set isMap(value) { setBooleanAttribute(this, 'ismap', value); }
    get width() { return elementTag(this) === 'canvas' ? (reflectedUnsigned(this, 'width') || 300) : reflectedUnsigned(this, 'width'); }
    set width(value) { setReflectedUnsigned(this, 'width', value); }
    get height() { return elementTag(this) === 'canvas' ? (reflectedUnsigned(this, 'height') || 150) : reflectedUnsigned(this, 'height'); }
    set height(value) { setReflectedUnsigned(this, 'height', value); }
    get naturalWidth() { return 0; }
    get naturalHeight() { return 0; }
    get complete() { return true; }
    get loading() { return reflectedAttribute(this, 'loading') || 'eager'; }
    set loading(value) { setReflectedAttribute(this, 'loading', value); }
    get decoding() { return reflectedAttribute(this, 'decoding') || 'auto'; }
    set decoding(value) { setReflectedAttribute(this, 'decoding', value); }
    decode() { return Promise.resolve(); }
    get autoplay() { return booleanAttribute(this, 'autoplay'); }
    set autoplay(value) { setBooleanAttribute(this, 'autoplay', value); }
    get loop() { return booleanAttribute(this, 'loop'); }
    set loop(value) { setBooleanAttribute(this, 'loop', value); }
    get controls() { return booleanAttribute(this, 'controls'); }
    set controls(value) { setBooleanAttribute(this, 'controls', value); }
    get muted() { return Boolean(mediaState(this).__vixenMuted); }
    set muted(value) { mediaState(this).__vixenMuted = Boolean(value); }
    get defaultMuted() { return booleanAttribute(this, 'muted'); }
    set defaultMuted(value) { setBooleanAttribute(this, 'muted', value); }
    get preload() { return reflectedKeyword(this, 'preload', ['none', 'metadata', 'auto']); }
    set preload(value) { setReflectedAttribute(this, 'preload', value); }
    get networkState() { return 0; }
    get readyState() { return 0; }
    get currentTime() { return Number(mediaState(this).__vixenCurrentTime) || 0; }
    set currentTime(value) { setMediaCurrentTime(this, value); }
    get duration() { return NaN; }
    get paused() { return true; }
    get ended() { return false; }
    get volume() { return Number(mediaState(this).__vixenVolume); }
    set volume(value) { setMediaVolume(this, value); }
    get videoWidth() { return 0; }
    get videoHeight() { return 0; }
    get poster() { return reflectedAttribute(this, 'poster'); }
    set poster(value) { setReflectedAttribute(this, 'poster', value); }
    get playsInline() { return booleanAttribute(this, 'playsinline'); }
    set playsInline(value) { setBooleanAttribute(this, 'playsinline', value); }
    load() {}
    play() { return Promise.resolve(); }
    pause() {}
    canPlayType(_type = '') { return ''; }
    getContext(contextId = '') {
      if (elementTag(this) !== 'canvas') return null;
      const id = String(contextId).toLowerCase();
      return id === '2d' ? canvasContext2d(this) : null;
    }
    toDataURL(type = 'image/png') { return elementTag(this) === 'canvas' ? canvasDataUrl(type) : ''; }
    toBlob(callback, type = 'image/png') {
      if (typeof callback !== 'function') return;
      callback(new Blob([], { type: canvasDataUrl(type).slice(5).split(';', 1)[0] }));
    }
    transferControlToOffscreen() { throw new TypeError('OffscreenCanvas is not implemented'); }
    get open() { return booleanAttribute(this, 'open'); }
    set open(value) { setDialogOpen(this, value); }
    get returnValue() { return dialogState(this).__vixenReturnValue; }
    set returnValue(value) { dialogState(this).__vixenReturnValue = String(value); }
    show() { showDialog(this, false); }
    showModal() { showDialog(this, true); }
    close(returnValue = undefined) { closeDialog(this, returnValue); }
    requestClose(returnValue = undefined) { closeDialog(this, returnValue); }
    get disabled() { return booleanAttribute(this, 'disabled'); }
    set disabled(value) { setBooleanAttribute(this, 'disabled', value); }
    get readOnly() { return booleanAttribute(this, 'readonly'); }
    set readOnly(value) { setBooleanAttribute(this, 'readonly', value); }
    get required() { return booleanAttribute(this, 'required'); }
    set required(value) { setBooleanAttribute(this, 'required', value); }
    get multiple() { return booleanAttribute(this, 'multiple'); }
    set multiple(value) { setBooleanAttribute(this, 'multiple', value); }
    get placeholder() { return reflectedAttribute(this, 'placeholder'); }
    set placeholder(value) { setReflectedAttribute(this, 'placeholder', value); }
    get autocomplete() { return reflectedAttribute(this, 'autocomplete'); }
    set autocomplete(value) { setReflectedAttribute(this, 'autocomplete', value); }
    get min() { return numericMin(this); }
    set min(value) { setReflectedAttribute(this, 'min', value); }
    get max() { return numericMax(this); }
    set max(value) { setReflectedAttribute(this, 'max', value); }
    get maxLength() { return reflectedNonNegativeInteger(this, 'maxlength', -1); }
    set maxLength(value) { setReflectedInteger(this, 'maxlength', value); }
    get minLength() { return reflectedNonNegativeInteger(this, 'minlength', -1); }
    set minLength(value) { setReflectedInteger(this, 'minlength', value); }
    get pattern() { return reflectedAttribute(this, 'pattern'); }
    set pattern(value) { setReflectedAttribute(this, 'pattern', value); }
    get step() { return reflectedAttribute(this, 'step'); }
    set step(value) { setReflectedAttribute(this, 'step', value); }
    get cols() { return reflectedUnsigned(this, 'cols') || 20; }
    set cols(value) { setReflectedUnsigned(this, 'cols', value); }
    get rows() { return tableRowsCollection(this); }
    set rows(value) { if (elementTag(this) === 'textarea') setReflectedUnsigned(this, 'rows', value); }
    get wrap() { return reflectedAttribute(this, 'wrap') || 'soft'; }
    set wrap(value) { setReflectedAttribute(this, 'wrap', value); }
    get low() { return elementTag(this) === 'meter' ? reflectedNumber(this, 'low', numericMin(this)) : 0; }
    set low(value) { setReflectedNumber(this, 'low', value); }
    get high() { return elementTag(this) === 'meter' ? reflectedNumber(this, 'high', numericMax(this)) : 0; }
    set high(value) { setReflectedNumber(this, 'high', value); }
    get optimum() { return elementTag(this) === 'meter' ? reflectedNumber(this, 'optimum', (numericMin(this) + numericMax(this)) / 2) : 0; }
    set optimum(value) { setReflectedNumber(this, 'optimum', value); }
    get position() { return progressPosition(this); }
    get caption() { return elementTag(this) === 'table' ? firstDescendantElementBySelector(this, 'caption') : null; }
    get tHead() { return elementTag(this) === 'table' ? firstDescendantElementBySelector(this, 'thead') : null; }
    get tFoot() { return elementTag(this) === 'table' ? firstDescendantElementBySelector(this, 'tfoot') : null; }
    get tBodies() { return elementTag(this) === 'table' ? new VixenHTMLCollection(descendantElementsBySelector(this, 'tbody')) : new VixenHTMLCollection([]); }
    get cells() { return tableCellsCollection(this); }
    get rowIndex() { return tableRowIndex(this); }
    get sectionRowIndex() { return sectionRowIndex(this); }
    get cellIndex() { return tableCellIndex(this); }
    get htmlFor() { return reflectedAttribute(this, 'for'); }
    set htmlFor(value) { setReflectedAttribute(this, 'for', value); }
    get control() { return labelControlElement(this); }
    get labels() { return new VixenNodeList(controlLabelNodeIds(this)); }
    get contentEditable() { return contentEditableState(this); }
    set contentEditable(value) { setReflectedAttribute(this, 'contenteditable', value); }
    get isContentEditable() { return isContentEditableElement(this); }
    get options() {
      return elementTag(this) === 'select'
        ? new VixenHTMLCollection(optionElements(this).map((option) => option.__vixenNodeId))
        : undefined;
    }
    get selectedOptions() {
      return elementTag(this) === 'select'
        ? new VixenHTMLCollection(selectedOptionElements(this).map((option) => option.__vixenNodeId))
        : undefined;
    }
    get selectedIndex() { return elementTag(this) === 'select' ? selectSelectedIndex(this) : -1; }
    set selectedIndex(value) { if (elementTag(this) === 'select') setSelectSelectedIndex(this, value); }
    get length() { return elementTag(this) === 'select' ? optionElements(this).length : 0; }
    get size() {
      if (elementTag(this) !== 'select' && elementTag(this) !== 'input') return 0;
      const n = Number.parseInt(reflectedAttribute(this, 'size') || '0', 10);
      return Number.isFinite(n) && n > 0 ? n : 0;
    }
    set size(value) {
      const n = Number.parseInt(String(value), 10);
      setReflectedAttribute(this, 'size', String(Number.isFinite(n) && n > 0 ? n : 0));
    }
    get label() { return elementTag(this) === 'option' ? optionLabel(this) : reflectedAttribute(this, 'label'); }
    set label(value) { setReflectedAttribute(this, 'label', value); }
    get defaultSelected() { return elementTag(this) === 'option' ? optionDefaultSelected(this) : booleanAttribute(this, 'selected'); }
    set defaultSelected(value) { if (elementTag(this) === 'option') setOptionDefaultSelected(this, Boolean(value)); else setBooleanAttribute(this, 'selected', value); }
    get selected() {
      if (elementTag(this) !== 'option') return booleanAttribute(this, 'selected');
      const parent = this.parentElement;
      if (!parent || elementTag(parent) !== 'select') return booleanAttribute(this, 'selected');
      return selectedOptionElements(parent).includes(this);
    }
    set selected(value) { setOptionSelected(this, Boolean(value)); }
    get index() { return elementTag(this) === 'option' ? optionIndex(this) : 0; }
    get files() { return inputFiles(this); }
    set files(value) { setInputFiles(this, value); }
    get value() { return reflectedElementValue(this); }
    set value(value) { setReflectedElementValue(this, value); }
    get valueAsNumber() { return elementTag(this) === 'input' ? inputValueAsNumber(this) : NaN; }
    set valueAsNumber(value) { if (elementTag(this) === 'input') setInputValueAsNumber(this, value); }
    get valueAsDate() { return null; }
    set valueAsDate(_value) { if (elementTag(this) === 'input') setControlValue(this, ''); }
    get textLength() { return textLength(this); }
    get indeterminate() { return Boolean(ensureControlState(this).__vixenIndeterminate); }
    set indeterminate(value) { ensureControlState(this).__vixenIndeterminate = Boolean(value); }
    get willValidate() { return willValidateElement(this); }
    get validity() { return validityStateForElementOrForm(this); }
    get validationMessage() { return validationMessageForElement(this); }
    checkValidity() { return checkValidityElement(this); }
    reportValidity() { return checkValidityElement(this); }
    setCustomValidity(message) { elementRecord(this).__vixenCustomValidityMessage = String(message || ''); }
    get defaultValue() { return ensureControlState(this).defaultValue; }
    set defaultValue(value) {
      const text = String(value);
      const record = ensureControlState(this);
      record.defaultValue = text;
      if (elementTag(this) === 'input') setElementAttribute(this.__vixenNodeId, 'value', text);
      else if (elementTag(this) === 'textarea') setElementText(this.__vixenNodeId, text);
    }
    get defaultChecked() { return elementTag(this) === 'input' ? inputDefaultChecked(this) : booleanAttribute(this, 'checked'); }
    set defaultChecked(value) { if (elementTag(this) === 'input') setInputDefaultChecked(this, Boolean(value)); else setBooleanAttribute(this, 'checked', value); }
    get selectionStart() { return isTextEditableControl(this) ? controlSelection(this)[0] : null; }
    set selectionStart(value) {
      if (!isTextEditableControl(this)) return;
      setControlSelection(this, value, Math.max(clampControlOffset(this, value), controlSelection(this)[1]));
    }
    get selectionEnd() { return isTextEditableControl(this) ? controlSelection(this)[1] : null; }
    set selectionEnd(value) {
      if (!isTextEditableControl(this)) return;
      const start = controlSelection(this)[0];
      const end = clampControlOffset(this, value);
      setControlSelection(this, Math.min(start, end), end);
    }
    setSelectionRange(start, end = start) { if (isTextEditableControl(this)) setControlSelection(this, start, end); }
    setRangeText(replacement, start = undefined, end = undefined, selectionMode = 'preserve') { setRangeText(this, replacement, start, end, selectionMode); }
    select() { if (isTextEditableControl(this)) setControlSelection(this, 0, controlValue(this).length); }
    stepUp(count = 1) { stepControl(this, 1, count); }
    stepDown(count = 1) { stepControl(this, -1, count); }
    showPicker() {}
    get checked() { return this.hasAttribute('checked'); }
    set checked(value) { if (elementTag(this) === 'input') setInputChecked(this, Boolean(value)); else setBooleanAttribute(this, 'checked', value); }
    getAttribute(name) {
      return elementAttribute(this.__vixenNodeId, name);
    }
    hasAttribute(name) { return elementAttribute(this.__vixenNodeId, name) !== null; }
    setAttribute(name, value) { setElementAttribute(this.__vixenNodeId, name, value); }
    removeAttribute(name) { removeElementAttribute(this.__vixenNodeId, name); }
    toggleAttribute(name, force = undefined) {
      const present = this.hasAttribute(name);
      const shouldHave = force === undefined ? !present : Boolean(force);
      if (shouldHave) this.setAttribute(name, '');
      else this.removeAttribute(name);
      return shouldHave;
    }
    getAttributeNames() { return elementRecord(this).attributes.map(([name]) => name); }
    hasAttributes() { return elementRecord(this).attributes.length > 0; }
    matches(selector) { return elementMatches(this.__vixenNodeId, selector); }
    closest(selector) {
      let current = this;
      while (current !== null) {
        if (current.matches(selector)) return current;
        current = current.parentElement;
      }
      return null;
    }
    querySelector(selector) { return this.querySelectorAll(selector).item(0); }
    querySelectorAll(selector) {
      const rootId = this.__vixenNodeId;
      return new VixenNodeList(findAllNodeIds(selector).filter((nodeId) => isDescendantOf(nodeId, rootId)));
    }
    getElementsByTagName(tagName) {
      const selector = String(tagName) === '*' ? '*' : String(tagName);
      return new VixenHTMLCollection(findAllNodeIds(selector).filter((nodeId) => isDescendantOf(nodeId, this.__vixenNodeId)));
    }
    getElementsByClassName(className) {
      return new VixenHTMLCollection(findAllNodeIds('.' + String(className)).filter((nodeId) => isDescendantOf(nodeId, this.__vixenNodeId)));
    }
    appendChild(child) { return appendChildNode(this, child); }
    removeChild(child) { return removeChildNode(this, child); }
    insertBefore(child, before) { return insertChildNode(this, child, before); }
    replaceChildren(...nodes) { replaceElementChildren(this, nodes); }
    append(...nodes) { for (const node of nodes) appendChildNode(this, nodeFromAppendArg(node)); }
    prepend(...nodes) { for (let i = nodes.length - 1; i >= 0; i--) insertChildNode(this, nodeFromAppendArg(nodes[i]), this.firstChild); }
    click() { this.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, composed: true })); }
    focus() {
      if (activeElementNodeId === this.__vixenNodeId) return;
      const old = wrapElementByNodeId(activeElementNodeId);
      if (activeTextComposition !== null && activeTextComposition.nodeId !== this.__vixenNodeId) {
        finishActiveTextComposition();
      }
      activeElementNodeId = this.__vixenNodeId;
      if (this.__vixenNodeId > 0) unwrapDomOp(op_vixen_dom_set_focused_element(this.__vixenNodeId));
      if (old) old.dispatchEvent(new FocusEvent('focusout', { bubbles: true, composed: true, relatedTarget: this }));
      this.dispatchEvent(new FocusEvent('focusin', { bubbles: true, composed: true, relatedTarget: old }));
      if (old) old.dispatchEvent(new FocusEvent('blur', { composed: true, relatedTarget: this }));
      this.dispatchEvent(new FocusEvent('focus', { composed: true, relatedTarget: old }));
      if (isPlatformTextEditable(this)) {
        const [start, end] = platformTextSelection(this);
        setPlatformTextSelection(this, start, end);
      }
    }
    blur() {
      if (activeElementNodeId !== this.__vixenNodeId) return;
      if (activeTextComposition !== null && activeTextComposition.nodeId === this.__vixenNodeId) {
        finishActiveTextComposition();
      }
      activeElementNodeId = null;
      unwrapDomOp(op_vixen_dom_set_focused_element(0));
      this.dispatchEvent(new FocusEvent('focusout', { bubbles: true, composed: true, relatedTarget: null }));
      this.dispatchEvent(new FocusEvent('blur', { composed: true, relatedTarget: null }));
    }
    scrollIntoView(options = true) { scrollElementIntoView(this, options); }
    attachShadow(init = {}) { return attachShadowRoot(this, init); }
    assignedNodes(_options = {}) { return new VixenNodeList([]); }
    assignedElements(_options = {}) { return new VixenHTMLCollection([]); }
    submit() { if (elementTag(this) === 'form') queueFormSubmission(this, null); }
    requestSubmit(submitter = undefined) { if (elementTag(this) === 'form') submitFormDefault(this, submitter && typeof submitter.__vixenNodeId === 'number' ? submitter : null); }
    reset() { if (elementTag(this) === 'form') resetFormDefault(this); }
    getBoundingClientRect() { return new VixenDOMRectReadOnly(elementRect(this.__vixenNodeId)); }
    getClientRects() { return makeDOMRectList(elementRect(this.__vixenNodeId)); }
    getBoxQuads() { return elementBoxQuad(this); }
  }

  webidl.adoptInterface('ValidityState', VixenValidityState);
  webidl.adoptInterface('Element', VixenElement);

  const elementImplementationMembers = [
    'id', 'className', 'tagName', 'nodeName', 'localName', 'namespaceURI', 'prefix', 'nodeType', 'isConnected',
    'ownerDocument', 'parentNode', 'parentElement', 'childNodes', 'children', 'firstChild',
    'lastChild', 'firstElementChild', 'lastElementChild', 'childElementCount', 'previousSibling',
    'nextSibling', 'previousElementSibling', 'nextElementSibling', 'clientWidth', 'clientHeight', 'clientTop', 'clientLeft',
    'scrollWidth', 'scrollHeight', 'scrollTop', 'scrollLeft', 'offsetWidth', 'offsetHeight', 'offsetTop', 'offsetLeft', 'offsetParent',
    'textContent', 'innerText', 'text',
    'innerHTML', 'outerHTML', 'attributes', 'classList', 'relList', 'sandbox', 'dataset', 'style', 'sheet',
    'hidden', 'tabIndex', 'accessKey', 'accessKeyLabel', 'draggable', 'spellcheck', 'translate', 'inputMode', 'enterKeyHint', 'popover',
    'title', 'lang', 'dir', 'type', 'name', 'content', 'httpEquiv', 'charset', 'method', 'enctype', 'encoding', 'action',
    'accept', 'acceptCharset', 'noValidate', 'formAction', 'formEnctype', 'formMethod', 'formNoValidate', 'formTarget',
    'cite', 'dateTime', 'reversed', 'start',
    'href', 'origin', 'protocol', 'host', 'hostname', 'port', 'pathname', 'search', 'hash', 'target', 'download', 'rel', 'hreflang', 'coords', 'shape',
    'src', 'srcset', 'sizes', 'media', 'as', 'async', 'defer', 'noModule', 'kind', 'srclang', 'default', 'track', 'textTracks', 'srcdoc', 'allow', 'data', 'span', 'colSpan', 'rowSpan', 'headers', 'scope', 'abbr',
    'contentDocument', 'contentWindow',
    'currentSrc', 'alt', 'crossOrigin', 'useMap', 'isMap', 'width', 'height', 'naturalWidth', 'naturalHeight', 'complete', 'loading', 'decoding', 'decode',
    'autoplay', 'loop', 'controls', 'muted', 'defaultMuted', 'preload', 'networkState', 'readyState', 'currentTime', 'duration', 'paused', 'ended', 'volume',
    'videoWidth', 'videoHeight', 'poster', 'playsInline', 'load', 'play', 'pause', 'canPlayType',
    'getContext', 'toDataURL', 'toBlob', 'transferControlToOffscreen',
    'open', 'returnValue', 'show', 'showModal', 'close', 'requestClose',
    'disabled', 'readOnly', 'required', 'multiple',
    'placeholder', 'autocomplete', 'min', 'max', 'maxLength', 'minLength', 'pattern', 'step', 'cols', 'rows', 'wrap', 'low', 'high', 'optimum', 'position',
    'caption', 'tHead', 'tFoot', 'tBodies', 'cells', 'rowIndex', 'sectionRowIndex', 'cellIndex',
    'htmlFor', 'control', 'labels', 'contentEditable', 'isContentEditable',
    'options', 'selectedOptions', 'selectedIndex', 'length', 'size', 'label', 'defaultSelected', 'selected', 'index',
    'files', 'value', 'valueAsNumber', 'valueAsDate', 'textLength', 'indeterminate', 'willValidate', 'validity', 'validationMessage', 'checkValidity', 'reportValidity', 'setCustomValidity',
    'defaultValue', 'defaultChecked', 'selectionStart', 'selectionEnd', 'setSelectionRange', 'setRangeText', 'select', 'stepUp', 'stepDown', 'showPicker', 'checked',
    'getAttribute', 'hasAttribute', 'setAttribute', 'removeAttribute', 'toggleAttribute',
    'getAttributeNames', 'hasAttributes', 'matches', 'closest', 'appendChild', 'removeChild',
    'insertBefore', 'replaceChildren', 'append', 'prepend', 'click', 'focus', 'blur',
    'submit', 'requestSubmit', 'reset',
    'scrollIntoView', 'attachShadow', 'assignedNodes', 'assignedElements',
    'querySelector', 'querySelectorAll', 'getElementsByTagName', 'getElementsByClassName',
    'getBoundingClientRect', 'getClientRects', 'getBoxQuads',
  ];

  function installElementMembers(interfaceName) {
    const prototype = webidl.interfaceConstructor(interfaceName).prototype;
    for (const name of elementImplementationMembers) {
      const descriptor = Object.getOwnPropertyDescriptor(VixenElement.prototype, name);
      if (descriptor) Object.defineProperty(prototype, name, descriptor);
    }
  }

  installElementMembers('HTMLElement');
  for (const interfaceName of new Set(htmlElementInterfaceByTag.values())) {
    installElementMembers(interfaceName);
  }

  function installInterfaceConstants(interfaceName, entries) {
    const constructor = webidl.interfaceConstructor(interfaceName);
    for (const [name, value] of entries) {
      Object.defineProperty(constructor, name, { value, enumerable: true, configurable: false });
      Object.defineProperty(constructor.prototype, name, { value, enumerable: true, configurable: false });
    }
  }

  installInterfaceConstants('HTMLMediaElement', [
    ['NETWORK_EMPTY', 0], ['NETWORK_IDLE', 1], ['NETWORK_LOADING', 2], ['NETWORK_NO_SOURCE', 3],
    ['HAVE_NOTHING', 0], ['HAVE_METADATA', 1], ['HAVE_CURRENT_DATA', 2], ['HAVE_FUTURE_DATA', 3], ['HAVE_ENOUGH_DATA', 4],
  ]);

  function installScriptElementSupports() {
    const supports = (type) => scriptTypeSupports(type);
    const constructor = webidl.interfaceConstructor('HTMLScriptElement');
    const constructorDescriptor = Object.getOwnPropertyDescriptor(constructor, 'supports');
    if (!constructorDescriptor || constructorDescriptor.configurable) {
      Object.defineProperty(constructor, 'supports', { value: supports, enumerable: true, configurable: true });
    }
    const prototypeDescriptor = Object.getOwnPropertyDescriptor(constructor.prototype, 'supports');
    if (!prototypeDescriptor || prototypeDescriptor.configurable) {
      Object.defineProperty(constructor.prototype, 'supports', { value: supports, enumerable: true, configurable: true });
    }
  }

  installScriptElementSupports();

  class VixenTextMetrics {
    constructor(width = 0) {
      defineWritableValue(this, 'width', Number(width) || 0);
      defineWritableValue(this, 'actualBoundingBoxLeft', 0);
      defineWritableValue(this, 'actualBoundingBoxRight', Number(width) || 0);
      defineWritableValue(this, 'fontBoundingBoxAscent', 10);
      defineWritableValue(this, 'fontBoundingBoxDescent', 2);
      defineWritableValue(this, 'actualBoundingBoxAscent', 10);
      defineWritableValue(this, 'actualBoundingBoxDescent', 2);
    }
  }

  class VixenCanvasGradient {
    addColorStop(_offset, _color) {}
  }

  class VixenCanvasPattern {
    setTransform(_transform = undefined) {}
  }

  class VixenImageData {
    constructor(width, height = undefined) {
      const dataLike = ArrayBuffer.isView(width) ? width : null;
      const w = dataLike ? Math.max(0, Math.trunc(Number(height) || 0)) : Math.max(0, Math.trunc(Number(width) || 0));
      const h = dataLike ? Math.max(0, Math.trunc(arguments.length > 2 ? Number(arguments[2]) || 0 : 0)) : Math.max(0, Math.trunc(Number(height) || 0));
      defineWritableValue(this, 'width', w);
      defineWritableValue(this, 'height', h);
      defineWritableValue(this, 'data', dataLike ? new Uint8ClampedArray(dataLike) : new Uint8ClampedArray(w * h * 4));
      defineWritableValue(this, 'colorSpace', 'srgb');
    }
  }

  class VixenImageBitmap {
    constructor(width = 0, height = 0) {
      defineWritableValue(this, 'width', Math.max(0, Math.trunc(Number(width) || 0)));
      defineWritableValue(this, 'height', Math.max(0, Math.trunc(Number(height) || 0)));
    }
    close() { this.width = 0; this.height = 0; }
  }

  class VixenPath2D {
    constructor(_path = undefined) {}
    addPath(_path, _transform = undefined) {}
    closePath() {}
    moveTo(_x, _y) {}
    lineTo(_x, _y) {}
    bezierCurveTo(_cp1x, _cp1y, _cp2x, _cp2y, _x, _y) {}
    quadraticCurveTo(_cpx, _cpy, _x, _y) {}
    arc(_x, _y, _radius, _startAngle, _endAngle, _counterclockwise = false) {}
    rect(_x, _y, _w, _h) {}
  }

  class VixenCanvasRenderingContext2D {
    constructor(canvas) {
      defineWritableValue(this, 'canvas', canvas, false);
      defineWritableValue(this, 'globalAlpha', 1);
      defineWritableValue(this, 'globalCompositeOperation', 'source-over');
      defineWritableValue(this, 'fillStyle', '#000000');
      defineWritableValue(this, 'strokeStyle', '#000000');
      defineWritableValue(this, 'lineWidth', 1);
      defineWritableValue(this, 'font', '10px sans-serif');
      defineWritableValue(this, 'textAlign', 'start');
      defineWritableValue(this, 'textBaseline', 'alphabetic');
    }
    save() {}
    restore() {}
    scale(_x, _y) {}
    rotate(_angle) {}
    translate(_x, _y) {}
    transform(_a, _b, _c, _d, _e, _f) {}
    setTransform(_a = 1, _b = 0, _c = 0, _d = 1, _e = 0, _f = 0) {}
    resetTransform() {}
    clearRect(_x, _y, _w, _h) {}
    fillRect(_x, _y, _w, _h) {}
    strokeRect(_x, _y, _w, _h) {}
    beginPath() {}
    closePath() {}
    moveTo(_x, _y) {}
    lineTo(_x, _y) {}
    bezierCurveTo(_cp1x, _cp1y, _cp2x, _cp2y, _x, _y) {}
    quadraticCurveTo(_cpx, _cpy, _x, _y) {}
    arc(_x, _y, _radius, _startAngle, _endAngle, _counterclockwise = false) {}
    rect(_x, _y, _w, _h) {}
    fill() {}
    stroke() {}
    clip() {}
    drawImage(_image, _dx, _dy) {}
    fillText(_text, _x, _y) {}
    strokeText(_text, _x, _y) {}
    measureText(text = '') { return new VixenTextMetrics(String(text).length * 10); }
    getImageData(_sx, _sy, sw, sh) { return this.createImageData(sw, sh); }
    putImageData(_imageData, _dx, _dy) {}
    createImageData(width, height) {
      return new VixenImageData(width, height);
    }
    createLinearGradient(_x0, _y0, _x1, _y1) { return new VixenCanvasGradient(); }
    createRadialGradient(_x0, _y0, _r0, _x1, _y1, _r1) { return new VixenCanvasGradient(); }
    createPattern(_image, _repetition = '') { return new VixenCanvasPattern(); }
  }

  class VixenOffscreenCanvasRenderingContext2D {
    constructor(canvas) { defineWritableValue(this, 'canvas', canvas, false); }
    commit() {}
  }

  class VixenOffscreenCanvas {
    constructor(width, height) {
      defineWritableValue(this, 'width', Math.max(0, Math.trunc(Number(width) || 0)));
      defineWritableValue(this, 'height', Math.max(0, Math.trunc(Number(height) || 0)));
    }
    getContext(contextId = '') {
      return String(contextId).toLowerCase() === '2d'
        ? cachedElementObject(this, '__vixenOffscreen2d', () => new VixenOffscreenCanvasRenderingContext2D(this))
        : null;
    }
    convertToBlob(options = {}) {
      const type = options && options.type ? String(options.type) : 'image/png';
      return Promise.resolve(new Blob([], { type }));
    }
    transferToImageBitmap() { return new VixenImageBitmap(this.width, this.height); }
  }

  class VixenImageBitmapRenderingContext {
    constructor(canvas = null) { defineWritableValue(this, 'canvas', canvas, false); }
    transferFromImageBitmap(_bitmap) {}
  }

  webidl.adoptInterface('CanvasRenderingContext2D', VixenCanvasRenderingContext2D);
  webidl.adoptInterface('TextMetrics', VixenTextMetrics);
  webidl.adoptInterface('CanvasGradient', VixenCanvasGradient);
  webidl.adoptInterface('CanvasPattern', VixenCanvasPattern);
  webidl.adoptInterface('ImageData', VixenImageData);
  webidl.adoptInterface('ImageBitmap', VixenImageBitmap);
  webidl.adoptInterface('Path2D', VixenPath2D);
  webidl.adoptInterface('OffscreenCanvas', VixenOffscreenCanvas);
  webidl.adoptInterface('OffscreenCanvasRenderingContext2D', VixenOffscreenCanvasRenderingContext2D);
  webidl.adoptInterface('ImageBitmapRenderingContext', VixenImageBitmapRenderingContext);

  class VixenTextTrack {
    constructor(kind = 'subtitles', label = '', language = '', id = '') {
      defineWritableValue(this, 'kind', String(kind || 'subtitles'));
      defineWritableValue(this, 'label', String(label || ''));
      defineWritableValue(this, 'language', String(language || ''));
      defineWritableValue(this, 'id', String(id || ''));
      defineWritableValue(this, 'mode', 'disabled');
      defineWritableValue(this, 'cues', null);
      defineWritableValue(this, 'activeCues', null);
    }
    addCue(_cue) {}
    removeCue(_cue) {}
  }

  class VixenTextTrackList {
    constructor(tracks) {
      Object.defineProperty(this, '__vixenTracks', { value: Object.freeze(tracks.slice()), enumerable: false });
      defineIndexedValues(this, this.__vixenTracks);
    }
    get length() { return this.__vixenTracks.length; }
    item(index) {
      const n = Number(index);
      return Number.isInteger(n) && n >= 0 && n < this.__vixenTracks.length ? this.__vixenTracks[n] : null;
    }
    getTrackById(id) {
      const value = String(id);
      return this.__vixenTracks.find((track) => track.id === value) || null;
    }
    [Symbol.iterator]() { return this.__vixenTracks[Symbol.iterator](); }
  }

  class VixenTextTrackCue {}
  class VixenTimeRanges {
    get length() { return 0; }
    start(_index) { throw new TypeError('TimeRanges is empty'); }
    end(_index) { throw new TypeError('TimeRanges is empty'); }
  }

  webidl.adoptInterface('TextTrack', VixenTextTrack);
  webidl.adoptInterface('TextTrackList', VixenTextTrackList);
  webidl.adoptInterface('TextTrackCue', VixenTextTrackCue);
  webidl.adoptInterface('TimeRanges', VixenTimeRanges);

  class VixenFormData {
    constructor(form = undefined) {
      const entries = [];
      if (form !== undefined) {
        if (!form || typeof form.__vixenNodeId !== 'number') {
          throw new TypeError('FormData expects a Vixen form element');
        }
        for (const entry of formEntries(form.__vixenNodeId)) {
          entries.push([entry.name, formDataEntryValue(entry)]);
        }
      }
      Object.defineProperty(this, '__vixenEntries', {
        value: entries,
        enumerable: false,
      });
    }
    append(name, value) { this.__vixenEntries.push([String(name), value]); }
    delete(name) {
      const value = String(name);
      for (let i = this.__vixenEntries.length - 1; i >= 0; i--) {
        if (this.__vixenEntries[i][0] === value) this.__vixenEntries.splice(i, 1);
      }
    }
    get(name) {
      const value = String(name);
      const entry = this.__vixenEntries.find(([entryName]) => entryName === value);
      return entry ? entry[1] : null;
    }
    getAll(name) {
      const value = String(name);
      return this.__vixenEntries.filter(([entryName]) => entryName === value).map(([, entryValue]) => entryValue);
    }
    has(name) {
      const value = String(name);
      return this.__vixenEntries.some(([entryName]) => entryName === value);
    }
    set(name, value) {
      this.delete(name);
      this.append(name, value);
    }
    entries() { return this.__vixenEntries.map(([name, value]) => [name, value])[Symbol.iterator](); }
    keys() { return this.__vixenEntries.map(([name]) => name)[Symbol.iterator](); }
    values() { return this.__vixenEntries.map(([, value]) => value)[Symbol.iterator](); }
    forEach(callback, thisArg = undefined) {
      for (const [name, value] of this.__vixenEntries) callback.call(thisArg, value, name, this);
    }
    [Symbol.iterator]() { return this.entries(); }
  }

  function formDataEntryValue(entry) {
    if (entry.kind === 'file') {
      if (entry.file instanceof File) return entry.file;
      return new File([], entry.filename || '', { type: entry.type || 'application/octet-stream' });
    }
    return String(entry.value || '');
  }

  webidl.adoptInterface('FormData', VixenFormData);

  class VixenXMLSerializer {
    serializeToString(node) {
      if (!node) return '';
      if (node.nodeType === 3) return escapeTextForHtml(node.textContent || '');
      if (node.nodeType === 11) return String(node.innerHTML || '');
      if (node === vixenDocument) return vixenDocument.documentElement ? vixenDocument.documentElement.outerHTML : '';
      if (typeof node.outerHTML === 'string') return node.outerHTML;
      return String(node.textContent || '');
    }
  }

  webidl.adoptInterface('XMLSerializer', VixenXMLSerializer);

  function rangeNodeLength(node) {
    if (node === vixenDocument) return node.childNodes.length;
    if (node && node.nodeType === 3) return node.length;
    if (node && node.childNodes) return node.childNodes.length;
    throw new TypeError('Range boundary must be a document, element, or text node');
  }

  function checkedRangeOffset(node, offset) {
    const number = Number(offset);
    if (!Number.isInteger(number) || number < 0 || number > rangeNodeLength(node)) {
      throw new TypeError('Range boundary offset is outside the node');
    }
    return number;
  }

  function rangeDocumentOrder() {
    const nodes = [vixenDocument];
    function visit(node) {
      if (!node) return;
      nodes.push(node);
      if (node.nodeType !== 3 && node.childNodes) {
        for (const child of node.childNodes) visit(child);
      }
    }
    visit(vixenDocument.documentElement);
    return nodes;
  }

  function compareRangeBoundaries(aNode, aOffset, bNode, bOffset) {
    if (aNode === bNode) return aOffset < bOffset ? -1 : (aOffset > bOffset ? 1 : 0);
    const nodes = rangeDocumentOrder();
    const a = nodes.indexOf(aNode);
    const b = nodes.indexOf(bNode);
    if (a === -1 || b === -1) throw new TypeError('Range boundary is not in this document');
    return a < b ? -1 : 1;
  }

  function commonRangeAncestor(a, b) {
    const ancestors = new Set();
    for (let node = a; node; node = node.parentNode) ancestors.add(node);
    for (let node = b; node; node = node.parentNode) if (ancestors.has(node)) return node;
    return vixenDocument;
  }

  function rangeNodeId(node) {
    if (node === vixenDocument) return 0;
    return node && Number.isInteger(node.__vixenNodeId) && node.__vixenNodeId > 0
      ? node.__vixenNodeId
      : null;
  }

  function nodeFromRangeNodeId(nodeId) {
    return Number(nodeId) === 0 ? vixenDocument : wrapElementByNodeId(Number(nodeId));
  }

  class VixenRange {
    constructor() {
      this.__vixenStartContainer = vixenDocument;
      this.__vixenEndContainer = vixenDocument;
      this.__vixenStartOffset = 0;
      this.__vixenEndOffset = 0;
      this.__vixenGeometryNode = null;
      this.__vixenSelectionOwner = null;
    }
    get startContainer() { return this.__vixenStartContainer; }
    get endContainer() { return this.__vixenEndContainer; }
    get startOffset() { return this.__vixenStartOffset; }
    get endOffset() { return this.__vixenEndOffset; }
    get commonAncestorContainer() { return commonRangeAncestor(this.startContainer, this.endContainer); }
    get collapsed() { return this.startContainer === this.endContainer && this.startOffset === this.endOffset; }
    __vixenChanged() {
      this.__vixenGeometryNode = null;
      if (this.__vixenSelectionOwner) this.__vixenSelectionOwner.__vixenRangeChanged(this);
    }
    __vixenSet(startNode, startOffset, endNode, endOffset, notify = true) {
      this.__vixenStartContainer = startNode;
      this.__vixenStartOffset = checkedRangeOffset(startNode, startOffset);
      this.__vixenEndContainer = endNode;
      this.__vixenEndOffset = checkedRangeOffset(endNode, endOffset);
      if (notify) this.__vixenChanged();
    }
    setStart(node, offset) {
      const checked = checkedRangeOffset(node, offset);
      if (compareRangeBoundaries(node, checked, this.endContainer, this.endOffset) > 0) {
        this.__vixenSet(node, checked, node, checked);
      } else {
        this.__vixenSet(node, checked, this.endContainer, this.endOffset);
      }
    }
    setEnd(node, offset) {
      const checked = checkedRangeOffset(node, offset);
      if (compareRangeBoundaries(node, checked, this.startContainer, this.startOffset) < 0) {
        this.__vixenSet(node, checked, node, checked);
      } else {
        this.__vixenSet(this.startContainer, this.startOffset, node, checked);
      }
    }
    collapse(toStart = false) {
      const node = toStart ? this.startContainer : this.endContainer;
      const offset = toStart ? this.startOffset : this.endOffset;
      this.__vixenSet(node, offset, node, offset);
    }
    selectNode(node) {
      const parent = node && node.parentNode;
      if (!parent || !parent.childNodes) throw new TypeError('Range node has no parent');
      const siblings = Array.from(parent.childNodes);
      const index = siblings.indexOf(node);
      if (index === -1) throw new TypeError('Range node is not attached to its parent');
      this.__vixenSet(parent, index, parent, index + 1);
      this.__vixenGeometryNode = nodeGeometryElement(node);
    }
    selectNodeContents(node) {
      this.__vixenSet(node, 0, node, rangeNodeLength(node));
      this.__vixenGeometryNode = nodeGeometryElement(node);
    }
    cloneRange() {
      const range = new VixenRange();
      range.__vixenSet(this.startContainer, this.startOffset, this.endContainer, this.endOffset, false);
      range.__vixenGeometryNode = this.__vixenGeometryNode;
      return range;
    }
    detach() {}
    deleteContents() {
      if (this.collapsed) return;
      if (this.startContainer === this.endContainer && this.startContainer.nodeType === 3) {
        const node = this.startContainer;
        node.data = node.data.slice(0, this.startOffset) + node.data.slice(this.endOffset);
        this.__vixenSet(node, this.startOffset, node, this.startOffset);
        return;
      }
      if (this.startContainer !== this.endContainer || this.startContainer.nodeType !== 1) {
        throw new TypeError('Cross-container Range deletion is not supported');
      }
      const parent = this.startContainer;
      const children = Array.from(parent.childNodes).slice(this.startOffset, this.endOffset);
      for (let index = children.length - 1; index >= 0; index--) parent.removeChild(children[index]);
      this.__vixenSet(parent, this.startOffset, parent, this.startOffset);
    }
    cloneContents() {
      const fragment = vixenDocument.createDocumentFragment();
      if (this.collapsed) return fragment;
      if (this.startContainer === this.endContainer && this.startContainer.nodeType === 3) {
        fragment.textContent = this.startContainer.data.slice(this.startOffset, this.endOffset);
        fragment.innerHTML = escapeTextForHtml(fragment.textContent);
        return fragment;
      }
      if (this.startContainer !== this.endContainer || this.startContainer.nodeType !== 1) {
        throw new TypeError('Cross-container Range cloning is not supported');
      }
      const children = Array.from(this.startContainer.childNodes).slice(this.startOffset, this.endOffset);
      fragment.innerHTML = children.map(serializeNodeObject).join('');
      fragment.textContent = children.map(textContentOfNode).join('');
      return fragment;
    }
    extractContents() {
      const fragment = this.cloneContents();
      this.deleteContents();
      return fragment;
    }
    insertNode(node) {
      if (this.startContainer.nodeType !== 1) throw new TypeError('Range insertion currently requires an element boundary');
      const before = this.startContainer.childNodes.item(this.startOffset);
      this.startContainer.insertBefore(node, before);
      this.__vixenChanged();
    }
    surroundContents(newParent) {
      if (!newParent || newParent.nodeType !== 1) throw new TypeError('Range surround parent must be an element');
      const fragment = this.cloneContents();
      this.deleteContents();
      newParent.innerHTML = fragment.innerHTML;
      this.insertNode(newParent);
      this.selectNode(newParent);
    }
    isPointInRange(node, offset) {
      const checked = checkedRangeOffset(node, offset);
      return compareRangeBoundaries(node, checked, this.startContainer, this.startOffset) >= 0
        && compareRangeBoundaries(node, checked, this.endContainer, this.endOffset) <= 0;
    }
    comparePoint(node, offset) {
      const checked = checkedRangeOffset(node, offset);
      if (compareRangeBoundaries(node, checked, this.startContainer, this.startOffset) < 0) return -1;
      if (compareRangeBoundaries(node, checked, this.endContainer, this.endOffset) > 0) return 1;
      return 0;
    }
    intersectsNode(node) {
      const parent = node && node.parentNode;
      if (!parent || !parent.childNodes) return false;
      const index = Array.from(parent.childNodes).indexOf(node);
      if (index === -1) return false;
      return compareRangeBoundaries(parent, index + 1, this.startContainer, this.startOffset) > 0
        && compareRangeBoundaries(parent, index, this.endContainer, this.endOffset) < 0;
    }
    getBoundingClientRect() { return new VixenDOMRectReadOnly(rangeGeometryRect(this)); }
    getClientRects() { return makeDOMRectList(rangeGeometryRect(this)); }
    toString() {
      if (this.collapsed) return '';
      if (this.startContainer === this.endContainer && this.startContainer.nodeType === 3) {
        return this.startContainer.data.slice(this.startOffset, this.endOffset);
      }
      if (this.startContainer === this.endContainer && this.startContainer.childNodes) {
        return Array.from(this.startContainer.childNodes)
          .slice(this.startOffset, this.endOffset)
          .map(textContentOfNode)
          .join('');
      }
      return '';
    }
  }

  class VixenSelection {
    constructor() {
      this.__vixenRanges = [];
      this.__vixenAnchorNode = null;
      this.__vixenAnchorOffset = 0;
      this.__vixenFocusNode = null;
      this.__vixenFocusOffset = 0;
    }
    get anchorNode() { return this.__vixenAnchorNode; }
    get anchorOffset() { return this.__vixenAnchorOffset; }
    get focusNode() { return this.__vixenFocusNode; }
    get focusOffset() { return this.__vixenFocusOffset; }
    get isCollapsed() { return this.rangeCount === 0 || this.__vixenRanges.every((range) => range.collapsed); }
    get rangeCount() { return this.__vixenRanges.length; }
    get type() { return this.rangeCount === 0 ? 'None' : (this.isCollapsed ? 'Caret' : 'Range'); }
    get direction() {
      if (this.rangeCount === 0 || this.isCollapsed) return 'none';
      return compareRangeBoundaries(this.anchorNode, this.anchorOffset, this.focusNode, this.focusOffset) <= 0
        ? 'forward'
        : 'backward';
    }
    __vixenRestore(snapshot) {
      if (!snapshot) return;
      const anchor = nodeFromRangeNodeId(snapshot.anchorNodeId);
      const focus = nodeFromRangeNodeId(snapshot.focusNodeId);
      if (!anchor || !focus) return;
      const range = new VixenRange();
      if (compareRangeBoundaries(anchor, snapshot.anchorOffset, focus, snapshot.focusOffset) <= 0) {
        range.__vixenSet(anchor, snapshot.anchorOffset, focus, snapshot.focusOffset, false);
      } else {
        range.__vixenSet(focus, snapshot.focusOffset, anchor, snapshot.anchorOffset, false);
      }
      range.__vixenSelectionOwner = this;
      this.__vixenRanges = [range];
      this.__vixenAnchorNode = anchor;
      this.__vixenAnchorOffset = snapshot.anchorOffset;
      this.__vixenFocusNode = focus;
      this.__vixenFocusOffset = snapshot.focusOffset;
    }
    __vixenCommit() {
      if (this.rangeCount === 0) {
        unwrapDomOp(op_vixen_dom_set_selection(null));
      } else {
        const anchorNodeId = rangeNodeId(this.anchorNode);
        const focusNodeId = rangeNodeId(this.focusNode);
        if (anchorNodeId !== null && focusNodeId !== null) {
          unwrapDomOp(op_vixen_dom_set_selection({
            anchorNodeId,
            anchorOffset: this.anchorOffset,
            focusNodeId,
            focusOffset: this.focusOffset,
          }));
        }
      }
      if (typeof vixenDocument.dispatchEvent === 'function') vixenDocument.dispatchEvent(new Event('selectionchange'));
    }
    __vixenRangeChanged(range) {
      this.__vixenAnchorNode = range.startContainer;
      this.__vixenAnchorOffset = range.startOffset;
      this.__vixenFocusNode = range.endContainer;
      this.__vixenFocusOffset = range.endOffset;
      this.__vixenCommit();
    }
    getRangeAt(index) {
      const number = Number(index);
      if (!Number.isInteger(number) || number < 0 || number >= this.rangeCount) throw new TypeError('Selection range index is out of bounds');
      return this.__vixenRanges[number];
    }
    addRange(range) {
      if (!range || typeof range.cloneRange !== 'function') throw new TypeError('Selection.addRange requires a Range');
      for (const current of this.__vixenRanges) current.__vixenSelectionOwner = null;
      range.__vixenSelectionOwner = this;
      this.__vixenRanges = [range];
      this.__vixenAnchorNode = range.startContainer;
      this.__vixenAnchorOffset = range.startOffset;
      this.__vixenFocusNode = range.endContainer;
      this.__vixenFocusOffset = range.endOffset;
      this.__vixenCommit();
    }
    removeRange(range) {
      if (!this.__vixenRanges.includes(range)) throw new TypeError('Selection does not contain this Range');
      this.removeAllRanges();
    }
    removeAllRanges() {
      for (const range of this.__vixenRanges) range.__vixenSelectionOwner = null;
      this.__vixenRanges = [];
      this.__vixenAnchorNode = null;
      this.__vixenAnchorOffset = 0;
      this.__vixenFocusNode = null;
      this.__vixenFocusOffset = 0;
      this.__vixenCommit();
    }
    empty() { this.removeAllRanges(); }
    collapse(node, offset = 0) {
      if (node === null) { this.removeAllRanges(); return; }
      const checked = checkedRangeOffset(node, offset);
      const range = new VixenRange();
      range.__vixenSet(node, checked, node, checked, false);
      this.addRange(range);
    }
    setPosition(node, offset = 0) { this.collapse(node, offset); }
    collapseToStart() {
      if (this.rangeCount === 0) throw new TypeError('Selection has no ranges');
      const range = this.getRangeAt(0);
      this.collapse(range.startContainer, range.startOffset);
    }
    collapseToEnd() {
      if (this.rangeCount === 0) throw new TypeError('Selection has no ranges');
      const range = this.getRangeAt(this.rangeCount - 1);
      this.collapse(range.endContainer, range.endOffset);
    }
    extend(node, offset = 0) {
      if (this.rangeCount === 0) throw new TypeError('Selection has no ranges');
      const checked = checkedRangeOffset(node, offset);
      const range = new VixenRange();
      if (compareRangeBoundaries(this.anchorNode, this.anchorOffset, node, checked) <= 0) {
        range.__vixenSet(this.anchorNode, this.anchorOffset, node, checked, false);
      } else {
        range.__vixenSet(node, checked, this.anchorNode, this.anchorOffset, false);
      }
      range.__vixenSelectionOwner = this;
      this.__vixenRanges = [range];
      this.__vixenFocusNode = node;
      this.__vixenFocusOffset = checked;
      this.__vixenCommit();
    }
    selectAllChildren(node) {
      const range = new VixenRange();
      range.selectNodeContents(node);
      this.addRange(range);
    }
    deleteFromDocument() {
      if (this.rangeCount === 0) return;
      const range = this.getRangeAt(0);
      range.deleteContents();
      this.__vixenAnchorNode = range.startContainer;
      this.__vixenAnchorOffset = range.startOffset;
      this.__vixenFocusNode = range.endContainer;
      this.__vixenFocusOffset = range.endOffset;
      this.__vixenCommit();
    }
    containsNode(node, allowPartialContainment = false) {
      if (this.rangeCount === 0) return false;
      const range = this.getRangeAt(0);
      if (allowPartialContainment) return range.intersectsNode(node);
      const parent = node && node.parentNode;
      if (!parent || !parent.childNodes) return false;
      const index = Array.from(parent.childNodes).indexOf(node);
      return index !== -1 && range.isPointInRange(parent, index) && range.isPointInRange(parent, index + 1);
    }
    toString() { return this.__vixenRanges.map((range) => range.toString()).join(''); }
  }

  webidl.adoptInterface('Range', VixenRange);
  webidl.adoptInterface('Selection', VixenSelection);

  const NodeFilter = Object.freeze({
    FILTER_ACCEPT: 1,
    FILTER_REJECT: 2,
    FILTER_SKIP: 3,
    SHOW_ALL: 0xFFFFFFFF,
    SHOW_ELEMENT: 0x1,
    SHOW_TEXT: 0x4,
    SHOW_COMMENT: 0x80,
    SHOW_DOCUMENT: 0x100,
    SHOW_DOCUMENT_TYPE: 0x200,
    SHOW_DOCUMENT_FRAGMENT: 0x400,
  });

  Object.defineProperty(globalThis, 'NodeFilter', {
    value: NodeFilter,
    writable: true,
    configurable: true,
  });

  function showsElement(whatToShow) {
    const mask = Number(whatToShow) >>> 0;
    return mask === NodeFilter.SHOW_ALL || (mask & NodeFilter.SHOW_ELEMENT) !== 0;
  }

  function traversalFilterResult(filter, node) {
    if (filter === null || filter === undefined) return NodeFilter.FILTER_ACCEPT;
    const result = typeof filter === 'function' ? filter(node) : filter.acceptNode(node);
    return Number(result) || NodeFilter.FILTER_ACCEPT;
  }

  function traversalAccept(node, whatToShow, filter) {
    if (!node || !showsElement(whatToShow)) return NodeFilter.FILTER_SKIP;
    return traversalFilterResult(filter, node);
  }

  function preorderSuccessor(node, root) {
    if (node.firstElementChild) return node.firstElementChild;
    let current = node;
    while (current && current !== root) {
      if (current.nextElementSibling) return current.nextElementSibling;
      current = current.parentElement;
    }
    return null;
  }

  function lastDescendant(node) {
    let current = node;
    while (current.lastElementChild) current = current.lastElementChild;
    return current;
  }

  function preorderPredecessor(node, root) {
    if (node === root) return null;
    if (node.previousElementSibling) return lastDescendant(node.previousElementSibling);
    return node.parentElement || null;
  }

  function subtreeSkipSuccessor(node, root) {
    let current = node;
    while (current && current !== root) {
      if (current.nextElementSibling) return current.nextElementSibling;
      current = current.parentElement;
    }
    return null;
  }

  function walkForward(start, boundary, whatToShow, filter, rejectSkipsSubtree) {
    let current = start;
    while (current) {
      const result = traversalAccept(current, whatToShow, filter);
      if (result === NodeFilter.FILTER_ACCEPT) return current;
      current = result === NodeFilter.FILTER_REJECT && rejectSkipsSubtree
        ? subtreeSkipSuccessor(current, boundary)
        : preorderSuccessor(current, boundary);
    }
    return null;
  }

  function walkBackward(start, boundary, whatToShow, filter) {
    let current = start;
    while (current) {
      const result = traversalAccept(current, whatToShow, filter);
      if (result === NodeFilter.FILTER_ACCEPT) return current;
      current = preorderPredecessor(current, boundary);
    }
    return null;
  }

  class VixenTreeWalker {
    constructor(root, whatToShow = NodeFilter.SHOW_ALL, filter = null) {
      if (!root || typeof root.__vixenNodeId !== 'number') throw new TypeError('TreeWalker root must be a Vixen Element');
      defineWritableValue(this, 'root', root);
      defineWritableValue(this, 'whatToShow', Number(whatToShow) >>> 0);
      defineWritableValue(this, 'filter', filter || null);
      defineWritableValue(this, 'currentNode', root);
    }
    parentNode() {
      let current = this.currentNode;
      while (current && current !== this.root) {
        current = current.parentElement;
        if (current && traversalAccept(current, this.whatToShow, this.filter) === NodeFilter.FILTER_ACCEPT) {
          this.currentNode = current;
          return current;
        }
      }
      return null;
    }
    firstChild() {
      const found = walkForward(this.currentNode.firstElementChild, this.currentNode, this.whatToShow, this.filter, true);
      if (found) this.currentNode = found;
      return found;
    }
    lastChild() {
      const found = walkBackward(this.currentNode.lastElementChild, this.currentNode, this.whatToShow, this.filter);
      if (found) this.currentNode = found;
      return found;
    }
    previousSibling() {
      const parent = this.currentNode.parentElement;
      if (!parent) return null;
      const found = walkBackward(this.currentNode.previousElementSibling, parent, this.whatToShow, this.filter);
      if (found) this.currentNode = found;
      return found;
    }
    nextSibling() {
      const parent = this.currentNode.parentElement;
      if (!parent) return null;
      const found = walkForward(this.currentNode.nextElementSibling, parent, this.whatToShow, this.filter, true);
      if (found) this.currentNode = found;
      return found;
    }
    previousNode() {
      const found = walkBackward(preorderPredecessor(this.currentNode, this.root), this.root, this.whatToShow, this.filter);
      if (found) this.currentNode = found;
      return found;
    }
    nextNode() {
      const found = walkForward(preorderSuccessor(this.currentNode, this.root), this.root, this.whatToShow, this.filter, true);
      if (found) this.currentNode = found;
      return found;
    }
  }

  class VixenNodeIterator {
    constructor(root, whatToShow = NodeFilter.SHOW_ALL, filter = null) {
      if (!root || typeof root.__vixenNodeId !== 'number') throw new TypeError('NodeIterator root must be a Vixen Element');
      defineWritableValue(this, 'root', root);
      defineWritableValue(this, 'whatToShow', Number(whatToShow) >>> 0);
      defineWritableValue(this, 'filter', filter || null);
      defineWritableValue(this, 'referenceNode', root);
      defineWritableValue(this, 'pointerBeforeReferenceNode', false);
    }
    nextNode() {
      const found = walkForward(preorderSuccessor(this.referenceNode, this.root), this.root, this.whatToShow, this.filter, false);
      if (found) this.referenceNode = found;
      return found;
    }
    previousNode() {
      const found = walkBackward(preorderPredecessor(this.referenceNode, this.root), this.root, this.whatToShow, this.filter);
      if (found) this.referenceNode = found;
      return found;
    }
    detach() {}
  }

  webidl.adoptInterface('TreeWalker', VixenTreeWalker);
  webidl.adoptInterface('NodeIterator', VixenNodeIterator);

  let documentWriteBuffer = '';
  let documentWriteOpen = false;
  let documentVisibilityState = 'visible';
  let documentHasFocus = true;

  function setDocumentTitle(value) {
    data.title = String(value);
    unwrapDomOp(op_vixen_dom_set_document_title(data.title));
  }

  function documentWriteBodyHtml(html) {
    const source = String(html);
    const bodyMatch = /<body\b[^>]*>([\s\S]*?)<\/body>/i.exec(source);
    if (bodyMatch) return bodyMatch[1];
    return source
      .replace(/<!doctype[^>]*>/i, '')
      .replace(/<html\b[^>]*>/i, '')
      .replace(/<\/html>/i, '')
      .replace(/<head\b[^>]*>[\s\S]*?<\/head>/i, '')
      .replace(/<title\b[^>]*>[\s\S]*?<\/title>/i, '');
  }

  class VixenDocument {
    get nodeType() { return 9; }
    get nodeName() { return '#document'; }
    get ownerDocument() { return null; }
    get parentNode() { return null; }
    get parentElement() { return null; }
    get childNodes() { return new VixenNodeList(data.documentElementNodeId === null ? [] : [data.documentElementNodeId]); }
    get firstChild() { return this.documentElement; }
    get lastChild() { return this.documentElement; }
    get textContent() { return null; }
    get title() { return data.title; }
    set title(value) { setDocumentTitle(value); }
    get URL() { return currentUrl; }
    get documentURI() { return currentUrl; }
    get baseURI() { return data.baseURI; }
    get readyState() { return 'complete'; }
    get compatMode() { return 'CSS1Compat'; }
    get characterSet() { return 'UTF-8'; }
    get charset() { return 'UTF-8'; }
    get contentType() { return 'text/html'; }
    get visibilityState() { return documentVisibilityState; }
    get hidden() { return documentVisibilityState !== 'visible'; }
    get referrer() { return ''; }
    get cookie() { return documentCookie(); }
    set cookie(value) { setDocumentCookie(value); }
    get defaultView() { return globalThis; }
    get location() { return globalThis.location; }
    get documentElement() { return wrapElementByNodeId(data.documentElementNodeId); }
    get head() { return wrapElementByNodeId(data.headNodeId); }
    get body() { return wrapElementByNodeId(data.bodyNodeId); }
    get activeElement() { return wrapElementByNodeId(activeElementNodeId) || this.body; }
    get scrollingElement() { return wrapElementByNodeId(data.scrollingElementNodeId); }
    get forms() { return new VixenHTMLCollection(data.collections.forms); }
    get images() { return new VixenHTMLCollection(data.collections.images); }
    get links() { return new VixenHTMLCollection(data.collections.links); }
    get scripts() { return new VixenHTMLCollection(data.collections.scripts); }
    getRootNode() { return this; }
    contains(target) { return nodeContains(this, target); }
    hasFocus() { return documentHasFocus; }
    querySelector(selector) { return this.querySelectorAll(selector).item(0); }
    querySelectorAll(selector) { return new VixenNodeList(findAllNodeIds(selector)); }
    elementFromPoint(x, y) {
      const nodeId = hitTestElementIds(x, y)[0];
      return nodeId === undefined ? null : wrapElementByNodeId(nodeId);
    }
    elementsFromPoint(x, y) {
      return hitTestElementIds(x, y).map(wrapElementByNodeId).filter((element) => element !== null);
    }
    getElementById(id) {
      const value = String(id);
      return wrapElementByNodeId(findAllNodeIds('*').find((nodeId) => {
        const record = recordForElementNodeId(nodeId);
        return record && record.id === value;
      }));
    }
    getElementsByTagName(tagName) { return new VixenHTMLCollection(findAllNodeIds(String(tagName) === '*' ? '*' : String(tagName))); }
    getElementsByClassName(className) { return new VixenHTMLCollection(findAllNodeIds('.' + String(className))); }
    createElement(tagName) {
      const tag = String(tagName).trim().toLowerCase();
      if (!/^[a-z][a-z0-9-]*$/.test(tag)) throw new TypeError('Invalid tag name: ' + tagName);
      const record = {
        nodeId: nextLocalNodeId--,
        tag,
        id: null,
        className: '',
        classes: [],
        attributes: [],
        textContent: '',
        innerHTML: '',
        outerHTML: '',
        parentNodeId: null,
        childNodeIds: [],
        firstElementChildNodeId: null,
        lastElementChildNodeId: null,
        previousSiblingNodeId: null,
        nextSiblingNodeId: null,
        previousElementSiblingNodeId: null,
        nextElementSiblingNodeId: null,
        childElementNodeIds: [],
        isConnected: false,
      };
      return makeElementObject(record);
    }
    createElementNS(_namespace, qualifiedName) { return this.createElement(String(qualifiedName).split(':').pop()); }
    createTextNode(data) { return new VixenText(String(data)); }
    createDocumentFragment() { return new VixenDocumentFragment(); }
    createRange() { return new VixenRange(); }
    getSelection() { return vixenSelection; }
    open() {
      documentWriteOpen = true;
      documentWriteBuffer = '';
      return this;
    }
    write(...parts) {
      if (!documentWriteOpen) this.open();
      documentWriteBuffer += parts.map(String).join('');
    }
    writeln(...parts) {
      this.write(...parts);
      documentWriteBuffer += '\n';
    }
    close() {
      const source = documentWriteBuffer;
      documentWriteOpen = false;
      documentWriteBuffer = '';
      const titleMatch = /<title\b[^>]*>([\s\S]*?)<\/title>/i.exec(source);
      if (titleMatch) setDocumentTitle(topLevelTextFromHtml(titleMatch[1]));
      if (vixenDocument.body) setElementInnerHTML(vixenDocument.body, documentWriteBodyHtml(source));
      queueNavigationAction({ type: 'set-content', html: source });
    }
    createTreeWalker(root, whatToShow = NodeFilter.SHOW_ALL, filter = null) { return new VixenTreeWalker(root, whatToShow, filter); }
    createNodeIterator(root, whatToShow = NodeFilter.SHOW_ALL, filter = null) { return new VixenNodeIterator(root, whatToShow, filter); }
  }
  webidl.adoptInterface('Document', VixenDocument);

  const vixenDocument = new VixenDocument();
  Object.defineProperty(globalThis, '__vixenApplyHistoryState', {
    value(url, length, index, stateJson, scrollRestoration) {
      currentUrl = String(url || currentUrl);
      historyLength = Math.max(1, Number(length) || 1);
      historyIndex = Math.min(historyLength - 1, Math.max(0, Number(index) || 0));
      historyState = parseHistoryState(stateJson);
      historyScrollRestoration = scrollRestoration === 'manual' ? 'manual' : 'auto';
      return true;
    },
    writable: false,
    configurable: false,
  });
  Object.defineProperty(globalThis, '__vixenApplyHostViewState', {
    value(focused, visible, viewportWidth, viewportHeight, deviceScale, maxScrollX, maxScrollY, scrollX, scrollY, emitScroll) {
      globalThis.innerWidth = Math.max(1, Number(viewportWidth) || 1);
      globalThis.innerHeight = Math.max(1, Number(viewportHeight) || 1);
      globalThis.devicePixelRatio = Math.max(0.1, Number(deviceScale) || 1);
      if (globalThis.screen) {
        globalThis.screen.width = globalThis.innerWidth;
        globalThis.screen.height = globalThis.innerHeight;
        globalThis.screen.availWidth = globalThis.innerWidth;
        globalThis.screen.availHeight = globalThis.innerHeight;
      }
      topLevelScrollMaxX = Math.max(0, Number(maxScrollX) || 0);
      topLevelScrollMaxY = Math.max(0, Number(maxScrollY) || 0);
      const nextScrollX = Math.min(topLevelScrollMaxX, Math.max(0, Number(scrollX) || 0));
      const nextScrollY = Math.min(topLevelScrollMaxY, Math.max(0, Number(scrollY) || 0));
      const scrollChanged = nextScrollX !== topLevelScrollX || nextScrollY !== topLevelScrollY;
      topLevelScrollX = nextScrollX;
      topLevelScrollY = nextScrollY;
      if (globalThis.visualViewport) {
        globalThis.visualViewport.width = globalThis.innerWidth;
        globalThis.visualViewport.height = globalThis.innerHeight;
        globalThis.visualViewport.pageLeft = topLevelScrollX;
        globalThis.visualViewport.pageTop = topLevelScrollY;
      }
      if (emitScroll && scrollChanged) dispatchRootScrollEvent();
      const nextVisibility = visible ? 'visible' : 'hidden';
      if (documentVisibilityState !== nextVisibility) {
        documentVisibilityState = nextVisibility;
        vixenDocument.dispatchEvent(new Event('visibilitychange'));
      }
      const nextFocus = Boolean(focused);
      if (documentHasFocus !== nextFocus) {
        if (!nextFocus) finishActiveTextComposition();
        documentHasFocus = nextFocus;
        if (typeof globalThis.dispatchEvent === 'function') {
          globalThis.dispatchEvent(new Event(nextFocus ? 'focus' : 'blur'));
        }
      }
      return true;
    },
    configurable: false,
  });
  const vixenSelection = new VixenSelection();
  vixenSelection.__vixenRestore(data.selection);
  if (typeof globalThis.window === 'undefined') {
    Object.defineProperty(globalThis, 'window', {
      value: globalThis,
      writable: true,
      configurable: true,
    });
  }
  const locationObject = {
    get href() { return currentUrl; },
    set href(value) { this.assign(value); },
    get origin() { return new URL(currentUrl).origin; },
    get protocol() { return new URL(currentUrl).protocol; },
    get host() { return new URL(currentUrl).host; },
    get hostname() { return new URL(currentUrl).hostname; },
    get port() { return new URL(currentUrl).port; },
    get pathname() { return new URL(currentUrl).pathname; },
    get search() { return new URL(currentUrl).search; },
    get hash() { return new URL(currentUrl).hash; },
    assign(value) {
      const url = resolveNavigationUrl(value, data.baseURI || currentUrl);
      queueNavigationAction({ type: 'navigate', url, replace: false });
      currentUrl = url;
    },
    replace(value) {
      const url = resolveNavigationUrl(value, data.baseURI || currentUrl);
      queueNavigationAction({ type: 'navigate', url, replace: true });
      currentUrl = url;
    },
    reload() { queueNavigationAction({ type: 'navigate', url: currentUrl, replace: true }); },
    toString() { return currentUrl; },
  };
  const historyObject = {
    get length() { return historyLength; },
    get state() { return cloneHistoryState(historyState); },
    get scrollRestoration() { return historyScrollRestoration; },
    set scrollRestoration(value) {
      const keyword = String(value);
      if ((keyword === 'auto' || keyword === 'manual') && keyword !== historyScrollRestoration) {
        historyScrollRestoration = keyword;
        queueNavigationAction({ type: 'history-scroll-restoration', value: keyword });
      }
    },
    pushState(state, unused = '', url = undefined) {
      const nextUrl = url === undefined || url === null ? currentUrl : resolveNavigationUrl(url, data.baseURI || currentUrl);
      assertSameOriginNavigation(nextUrl);
      historyState = cloneHistoryState(state);
      currentUrl = nextUrl;
      historyIndex += 1;
      historyLength = historyIndex + 1;
      queueNavigationAction({ type: 'history-push', url: nextUrl, stateJson: historyStateJson(historyState), title: String(unused || '') });
    },
    replaceState(state, unused = '', url = undefined) {
      const nextUrl = url === undefined || url === null ? currentUrl : resolveNavigationUrl(url, data.baseURI || currentUrl);
      assertSameOriginNavigation(nextUrl);
      historyState = cloneHistoryState(state);
      currentUrl = nextUrl;
      queueNavigationAction({ type: 'history-replace', url: nextUrl, stateJson: historyStateJson(historyState), title: String(unused || '') });
    },
    go(delta = 0) { queueNavigationAction({ type: 'history-traverse', delta: Number(delta) || 0 }); },
    back() { this.go(-1); },
    forward() { this.go(1); },
  };
  Object.defineProperty(globalThis, 'location', {
    value: locationObject,
    writable: true,
    configurable: true,
  });
  Object.defineProperty(globalThis, 'history', {
    value: historyObject,
    writable: true,
    configurable: true,
  });
  Object.defineProperty(globalThis, 'document', {
    value: vixenDocument,
    writable: true,
    configurable: true,
  });
  Object.defineProperty(globalThis, '__vixenEventPathForTarget', {
    value: eventPathForTarget,
    writable: true,
    configurable: true,
  });
  Object.defineProperty(globalThis, '__vixenRunDefaultAction', {
    value: runDomDefaultAction,
    writable: true,
    configurable: true,
  });
  Object.defineProperty(globalThis, '__vixenNodeContains', {
    value: nodeContains,
    writable: true,
    configurable: true,
  });
  Object.defineProperty(globalThis, '__vixenMakeNodeList', {
    value(nodes) { return new VixenNodeList(nodes || []); },
    writable: true,
    configurable: true,
  });
  Object.defineProperty(globalThis, '__vixenDrainNavigationActions', {
    value() { return navigationActions.splice(0, navigationActions.length); },
    writable: true,
    configurable: true,
  });
  Object.defineProperty(globalThis, 'getSelection', {
    value() { return vixenSelection; },
    writable: true,
    configurable: true,
  });
  Object.defineProperties(globalThis, {
    scrollX: { get() { return topLevelScrollX; }, configurable: true },
    scrollY: { get() { return topLevelScrollY; }, configurable: true },
    pageXOffset: { get() { return topLevelScrollX; }, configurable: true },
    pageYOffset: { get() { return topLevelScrollY; }, configurable: true },
    scroll: { value: windowScrollTo, writable: true, configurable: true },
    scrollTo: { value: windowScrollTo, writable: true, configurable: true },
    scrollBy: { value: windowScrollBy, writable: true, configurable: true },
  });
  globalThis.innerWidth = Math.max(1, Number(data.viewportWidth) || 800);
  globalThis.innerHeight = Math.max(1, Number(data.viewportHeight) || 600);
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_and_element_data_cross_op_boundaries_and_bootstrap_stays_ascii() {
        assert!(DOM_API_BOOTSTRAP.is_ascii());
        assert!(DOM_API_BOOTSTRAP.contains("op_vixen_dom_element_snapshot"));
        assert!(DOM_API_BOOTSTRAP.contains("op_vixen_dom_element_text"));
        assert!(DOM_API_BOOTSTRAP.contains("op_vixen_dom_element_attribute"));
        assert!(DOM_API_BOOTSTRAP.contains("op_vixen_dom_element_tokens"));
        assert!(DOM_API_BOOTSTRAP.contains("op_vixen_dom_element_dataset"));
        assert!(DOM_API_BOOTSTRAP.contains("op_vixen_dom_element_rect"));
        assert!(DOM_API_BOOTSTRAP.contains("op_vixen_dom_image_current_src"));
        assert!(DOM_API_BOOTSTRAP.contains("op_vixen_dom_set_element_text"));
        assert!(DOM_API_BOOTSTRAP.contains("op_vixen_dom_set_element_attr"));
        assert!(DOM_API_BOOTSTRAP.contains("op_vixen_dom_set_element_inner_html"));
        assert!(DOM_API_BOOTSTRAP.contains("op_vixen_dom_set_control_value"));
        assert!(DOM_API_BOOTSTRAP.contains("op_vixen_dom_set_contenteditable_state"));
        assert!(DOM_API_BOOTSTRAP.contains("op_vixen_dom_set_root_scroll"));
        assert!(DOM_API_BOOTSTRAP.contains("op_vixen_document_cookie_set"));
        assert!(!DOM_API_BOOTSTRAP.contains("data.elements"));

        let page = Page::from_html(
            "file:///dom-op-snapshot.html",
            "<html><head><title>é—😀</title></head><body><p id='lead' data-emoji='é'>body é—😀</p></body></html>",
        )
        .unwrap();
        let host = dom_host_state(&page, DomMutationSink::default(), None).unwrap();
        let snapshot = &host.snapshot;
        let body = host
            .elements
            .iter()
            .find(|record| record.tag == "body")
            .unwrap();
        let lead = host
            .elements
            .iter()
            .find(|record| record.id.as_deref() == Some("lead"))
            .unwrap();
        let lead_value = element_record_value(lead);

        assert_eq!(snapshot["title"].as_str(), Some("é—😀"));
        assert_eq!(snapshot["bodyNodeId"].as_u64(), Some(body.node_id as u64));
        assert!(snapshot.get("elements").is_none());
        assert_eq!(lead_value["tag"].as_str(), Some("p"));
        assert_eq!(lead_value["id"].as_str(), Some("lead"));
        assert_eq!(lead_value["textContent"].as_str(), Some("body é—😀"));
        assert!(lead_value.get("attrs").is_none());
        assert!(lead_value.get("classTokens").is_none());
        assert!(lead_value.get("dataset").is_none());
        assert_eq!(record_attr(lead, "DATA-EMOJI").as_deref(), Some("é"));
        assert_eq!(lead.text_content, "body é—😀");
        assert_eq!(lead.dataset[0].0, "emoji");
        assert_eq!(lead.dataset[0].1, "é");
    }

    #[test]
    fn element_geometry_requires_the_synchronous_renderer() {
        assert!(DOM_API_BOOTSTRAP.contains("getBoundingClientRect"));
        assert!(DOM_API_BOOTSTRAP.contains("getClientRects"));
        assert!(DOM_API_BOOTSTRAP.contains("elementFromPoint"));
        assert!(DOM_API_BOOTSTRAP.contains("elementsFromPoint"));
        assert!(DOM_API_BOOTSTRAP.contains("scrollIntoView"));

        let page = Page::from_html(
            "file:///dom-rect-op.html",
            "<style>#box { width: 40px; height: 20px; }</style><main><div id='box'>Box</div></main>",
        )
        .unwrap();
        let host = dom_host_state(&page, DomMutationSink::default(), None).unwrap();
        let box_record = host
            .elements
            .iter()
            .find(|record| record.id.as_deref() == Some("box"))
            .unwrap();
        assert!(box_record.bbox.is_none());
        assert!(element_record_value(box_record)["bbox"].is_null());
    }

    #[test]
    fn selector_behavior_crosses_finer_grained_ops() {
        assert!(DOM_API_BOOTSTRAP.contains("op_vixen_dom_query_selector_all"));
        assert!(DOM_API_BOOTSTRAP.contains("op_vixen_dom_get_element_by_id"));
        assert!(DOM_API_BOOTSTRAP.contains("op_vixen_dom_element_matches"));
        assert!(DOM_API_BOOTSTRAP.contains("parseSimpleSelector"));
        assert!(DOM_API_BOOTSTRAP.contains("recordMatches"));

        let page = Page::from_html(
            "file:///dom-selector-op.html",
            "<html><body><p id='lead' class='note callout'>Hello</p><p class='note'>Other</p></body></html>",
        )
        .unwrap();
        let host = dom_host_state(&page, DomMutationSink::default(), None).unwrap();
        let lead_id = host
            .elements
            .iter()
            .find(|record| record.id.as_deref() == Some("lead"))
            .unwrap()
            .node_id;

        assert_eq!(
            query_selector_node_ids(&host, "#lead").unwrap(),
            vec![lead_id]
        );
        assert_eq!(query_selector_node_ids(&host, ".note").unwrap().len(), 2);
        assert_eq!(query_selector_node_ids(&host, "p").unwrap().len(), 2);
        assert_eq!(query_selector_node_ids(&host, "p.note").unwrap().len(), 2);
        assert_eq!(
            query_selector_node_ids(&host, "p#lead.note").unwrap(),
            vec![lead_id]
        );
        assert_eq!(
            query_selector_node_ids(&host, "p[id='lead'], iframe[sandbox]").unwrap(),
            vec![lead_id]
        );
        assert!(query_selector_node_ids(&host, "body > p").is_err());

        let selector = parse_simple_selector_list(".callout").unwrap();
        let lead = host
            .elements
            .iter()
            .find(|record| record.node_id == lead_id)
            .unwrap();
        assert!(record_matches_any(lead, &selector));
    }

    #[test]
    fn repeated_element_scroll_mutations_coalesce_until_a_structural_barrier() {
        let sink = DomMutationSink::default();
        for top in 0..10_000 {
            sink.push(DomMutation::SetElementScroll {
                node_id: 7,
                element_id: Some("scroller".to_owned()),
                tag: "div".to_owned(),
                x: 0.0,
                y: f64::from(top),
            });
        }
        sink.push(DomMutation::SetTextContent {
            node_id: 3,
            value: "changed".to_owned(),
        });
        for top in 0..10_000 {
            sink.push(DomMutation::SetElementScroll {
                node_id: 7,
                element_id: Some("scroller".to_owned()),
                tag: "div".to_owned(),
                x: 0.0,
                y: f64::from(top),
            });
        }

        assert_eq!(
            sink.take(),
            vec![
                DomMutation::SetElementScroll {
                    node_id: 7,
                    element_id: Some("scroller".to_owned()),
                    tag: "div".to_owned(),
                    x: 0.0,
                    y: 9_999.0,
                },
                DomMutation::SetTextContent {
                    node_id: 3,
                    value: "changed".to_owned(),
                },
                DomMutation::SetElementScroll {
                    node_id: 7,
                    element_id: Some("scroller".to_owned()),
                    tag: "div".to_owned(),
                    x: 0.0,
                    y: 9_999.0,
                },
            ]
        );
    }
}
