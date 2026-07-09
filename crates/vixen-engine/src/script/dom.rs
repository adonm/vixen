//! DOM snapshot host extension for the JS runtime.
//!
//! This module is the Phase 6 bridge from [`crate::page::Page`] snapshots into
//! a JS global. It deliberately exposes a small, fail-closed subset while the
//! full DOM/WebIDL binding layer is still landing; `Element.textContent` is the
//! first mutating slice and is committed back to the authoritative [`Page`].

#![forbid(unsafe_code)]

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use deno_core::serde_json::json;
use deno_core::{Extension, ExtensionFileSource, OpState};

use vixen_api::ElementInfo;

use crate::class_list::DomTokenList;
use crate::dataset::collect_dataset;
use crate::engine_error::{EngineError, codes};
use crate::form_submission::{FormEntry, FormEntryValue};
use crate::page::Page;
use crate::style_dom::ElementRelation;

struct DomHost(Arc<DomHostState>);

struct DomHostState {
    snapshot: deno_core::serde_json::Value,
    elements: Vec<DomElementRecord>,
    text_overrides: Mutex<HashMap<usize, String>>,
    mutations: DomMutationSink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
        self.0
            .lock()
            .expect("DOM mutation sink poisoned")
            .push(mutation);
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
        op_vixen_dom_form_entries,
        op_vixen_dom_set_document_title,
        op_vixen_dom_set_element_text,
        op_vixen_dom_set_element_attr,
        op_vixen_dom_remove_element_attr,
        op_vixen_dom_set_element_inner_html,
        op_vixen_dom_set_control_value,
    ],
    options = {
        host: Arc<DomHostState>,
    },
    state = |state, options| {
        state.put(DomHost(options.host))
    },
);

pub(super) fn extension(page: &Page, mutations: DomMutationSink) -> Result<Extension, EngineError> {
    let host = dom_host_state(page, mutations).map_err(|err| {
        EngineError::script(
            codes::SCRIPT_EVAL,
            format!("failed to build DOM host snapshot: {err}"),
        )
    })?;
    let mut extension = vixen_dom::init(Arc::new(host));
    extension.js_files = Cow::Owned(vec![ExtensionFileSource::new_computed(
        "ext:vixen_dom/bootstrap.js",
        Arc::<str>::from(DOM_API_BOOTSTRAP),
    )]);
    Ok(extension)
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
    let host = state.borrow::<DomHost>();
    let Some(record) = element_record_by_node_id(&host.0, node_id as usize) else {
        return missing_element_result(node_id);
    };
    json!({ "ok": true, "rect": record.bbox.map(rect_value) })
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

fn dom_host_state(page: &Page, mutations: DomMutationSink) -> Result<DomHostState, String> {
    let elements = page.query_selector_all("*")?;
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

    Ok(DomHostState {
        snapshot: json!({
            "title": page.document().title().unwrap_or_default(),
            "url": page.url(),
            "baseURI": document_base_uri(page)?,
            "bodyTextContent": page.document().body_text_content(),
            "documentElementNodeId": document_element_node_id,
            "headNodeId": head_node_id,
            "bodyNodeId": body_node_id,
            "activeElementNodeId": body_node_id,
            "scrollingElementNodeId": document_element_node_id,
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
    })
}

fn element_record(page: &Page, info: &ElementInfo) -> DomElementRecord {
    let text_content = page
        .document()
        .element_text_content(info.node_id)
        .unwrap_or_else(|| info.text.clone());

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
    let id = info.id.as_deref()?;
    page.form_submission(id)
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
    op_vixen_dom_form_entries,
    op_vixen_dom_set_document_title,
    op_vixen_dom_set_element_text,
    op_vixen_dom_set_element_attr,
    op_vixen_dom_remove_element_attr,
    op_vixen_dom_set_element_inner_html,
    op_vixen_dom_set_control_value,
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
  const navigationActions = [];

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
    navigationActions.push(action);
  }

  function unwrapDomOp(result) {
    if (!result.ok) throw new TypeError(result.message);
    return result;
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
    if (record && Object.prototype.hasOwnProperty.call(record, 'bbox')) return normalizedElementRect(record, record.bbox);
    if (record && Number(nodeId) < 0) return normalizedElementRect(record, { x: 0, y: 0, width: 0, height: 0 });
    return normalizedElementRect(record, unwrapDomOp(op_vixen_dom_element_rect(nodeId)).rect);
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
    const hits = [];
    for (const nodeId of knownElementIds()) {
      const record = recordForElementNodeId(nodeId);
      if (!record || record.isConnected === false) continue;
      if (rectContainsPoint(elementRect(nodeId), px, py)) hits.push(nodeId);
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
    return String(selector).split(',').map((part) => parseSimpleSelector(part.trim()));
  }

  function parseSimpleSelector(raw) {
    if (!raw || /[\s>+~:]/.test(raw)) throw new TypeError('unsupported selector');
    let rest = raw;
    const selector = { tag: null, id: null, classes: [], attrs: [] };
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
        const end = rest.indexOf(']');
        if (end === -1) throw new TypeError('unsupported selector');
        const body = rest.slice(1, end).trim();
        const eq = body.indexOf('=');
        if (eq === -1) selector.attrs.push([body.toLowerCase(), null]);
        else {
          const name = body.slice(0, eq).trim().toLowerCase();
          let value = body.slice(eq + 1).trim();
          if ((value.startsWith('"') && value.endsWith('"')) || (value.startsWith("'") && value.endsWith("'"))) {
            value = value.slice(1, -1);
          }
          selector.attrs.push([name, value]);
        }
        rest = rest.slice(end + 1);
      } else {
        throw new TypeError('unsupported selector');
      }
    }
    if (!selector.tag && !selector.id && selector.classes.length === 0 && selector.attrs.length === 0 && raw !== '*') {
      throw new TypeError('unsupported selector');
    }
    return selector;
  }

  function recordMatchesAny(record, selectors) {
    return selectors.some((selector) => recordMatches(record, selector));
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
    return true;
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
    value(nodeId, type, init = {}) {
      const target = wrapElementByNodeId(Number(nodeId));
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
      return target.dispatchEvent(event);
    },
    configurable: true,
  });

  function keyboardEventTarget() {
    return wrapElementByNodeId(activeElementNodeId) || vixenDocument.body || vixenDocument;
  }

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
      return target.dispatchEvent(event);
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
    if (target === vixenDocument) return [vixenDocument];
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

  function setReflectedAttribute(element, name, value) {
    setElementAttribute(element.__vixenNodeId, name, String(value));
  }

  function reflectedFormMethod(element) {
    const value = reflectedAttribute(element, 'method').trim().toLowerCase();
    return value === 'post' || value === 'dialog' ? value : 'get';
  }

  function reflectedFormEnctype(element) {
    const value = reflectedAttribute(element, 'enctype').trim().toLowerCase();
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
    if (!isTextEditableControl(element)) return false;
    const input = String(text);
    if (input === '') return false;
    const value = controlValue(element);
    const [start, end] = controlSelection(element);
    const next = value.slice(0, start) + input + value.slice(end);
    const caret = start + input.length;
    applyControlValue(element, next, caret, caret, 'insertText', input, true);
    return true;
  }

  function deleteTextFromControl(element, direction) {
    if (!isTextEditableControl(element)) return false;
    const value = controlValue(element);
    let [start, end] = controlSelection(element);
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
    applyControlValue(element, next, start, start, inputType, null, true);
    return true;
  }

  function moveControlCaret(element, key) {
    if (!isTextEditableControl(element)) return false;
    const value = controlValue(element);
    const [start, end] = controlSelection(element);
    let next = end;
    if (key === 'ArrowLeft') next = Math.max(0, start - 1);
    else if (key === 'ArrowRight') next = Math.min(value.length, end + 1);
    else if (key === 'Home') next = 0;
    else if (key === 'End') next = value.length;
    else return false;
    setControlSelection(element, next, next);
    return true;
  }

  function handleKeyboardDefault(target, event) {
    if (!target || typeof target.__vixenNodeId !== 'number' || event.type !== 'keydown') return;
    if (!isTextEditableControl(target)) return;
    const key = String(event.key || '');
    if ((event.ctrlKey || event.metaKey) && key.toLowerCase() === 'a') {
      setControlSelection(target, 0, controlValue(target).length);
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
      if (elementTag(target) === 'textarea') insertTextIntoControl(target, '\n');
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
    const parent = option.parentElement;
    if (selected && parent && elementTag(parent) === 'select' && !parent.multiple) {
      for (const sibling of optionElements(parent)) {
        if (sibling !== option) sibling.removeAttribute('selected');
      }
    }
    if (selected) option.setAttribute('selected', '');
    else option.removeAttribute('selected');
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
    const submitEvent = new Event('submit', { bubbles: true, cancelable: true, composed: true });
    Object.defineProperty(submitEvent, 'submitter', { value: submitter || null, configurable: true });
    if (form.dispatchEvent(submitEvent)) {
      const action = elementAttribute(form.__vixenNodeId, 'action') || currentUrl;
      queueNavigationAction({
        type: 'form-submit',
        formId: form.id || '',
        formNodeId: form.__vixenNodeId,
        submitterId: submitter ? (submitter.id || '') : '',
        action: resolveNavigationUrl(action, data.baseURI || currentUrl),
        method: (elementAttribute(form.__vixenNodeId, 'method') || 'get').toLowerCase(),
      });
    }
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
      return;
    }
    if (tag === 'button') {
      const type = elementType(target) || 'submit';
      if (type === 'submit') submitFormDefault(findOwnerForm(target), target);
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
    get clientWidth() {
      if (this === vixenDocument.documentElement || this === vixenDocument.body) return Number(globalThis.innerWidth) || 0;
      const rect = elementRect(this.__vixenNodeId);
      return rect ? Math.max(0, Math.trunc(Number(rect.width) || 0)) : 0;
    }
    get clientHeight() {
      if (this === vixenDocument.documentElement || this === vixenDocument.body) return Number(globalThis.innerHeight) || 0;
      const rect = elementRect(this.__vixenNodeId);
      return rect ? Math.max(0, Math.trunc(Number(rect.height) || 0)) : 0;
    }
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
    get method() { return reflectedFormMethod(this); }
    set method(value) { setReflectedAttribute(this, 'method', value); }
    get enctype() { return reflectedFormEnctype(this); }
    set enctype(value) { setReflectedAttribute(this, 'enctype', value); }
    get encoding() { return this.enctype; }
    set encoding(value) { this.enctype = value; }
    get action() { return reflectedAttribute(this, 'action'); }
    set action(value) { setReflectedAttribute(this, 'action', value); }
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
    get value() { return controlValue(this); }
    set value(value) { setControlValue(this, value); }
    get defaultValue() { return ensureControlState(this).defaultValue; }
    set defaultValue(value) {
      const text = String(value);
      const record = ensureControlState(this);
      record.defaultValue = text;
      if (elementTag(this) === 'input') setElementAttribute(this.__vixenNodeId, 'value', text);
      else if (elementTag(this) === 'textarea') setElementText(this.__vixenNodeId, text);
    }
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
    select() { if (isTextEditableControl(this)) setControlSelection(this, 0, controlValue(this).length); }
    get checked() { return this.hasAttribute('checked'); }
    set checked(value) { if (Boolean(value)) this.setAttribute('checked', ''); else this.removeAttribute('checked'); }
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
      activeElementNodeId = this.__vixenNodeId;
      if (old) {
        old.dispatchEvent(new Event('focusout', { bubbles: true, composed: true }));
        old.dispatchEvent(new Event('blur', { composed: true }));
      }
      this.dispatchEvent(new Event('focusin', { bubbles: true, composed: true }));
      this.dispatchEvent(new Event('focus', { composed: true }));
    }
    blur() {
      if (activeElementNodeId !== this.__vixenNodeId) return;
      activeElementNodeId = null;
      this.dispatchEvent(new Event('focusout', { bubbles: true, composed: true }));
      this.dispatchEvent(new Event('blur', { composed: true }));
    }
    scrollIntoView() {}
    getBoundingClientRect() { return new VixenDOMRectReadOnly(elementRect(this.__vixenNodeId)); }
    getClientRects() { return makeDOMRectList(elementRect(this.__vixenNodeId)); }
  }

  webidl.adoptInterface('Element', VixenElement);

  const elementImplementationMembers = [
    'id', 'className', 'tagName', 'nodeName', 'localName', 'namespaceURI', 'prefix', 'nodeType', 'isConnected',
    'ownerDocument', 'parentNode', 'parentElement', 'childNodes', 'children', 'firstChild',
    'lastChild', 'firstElementChild', 'lastElementChild', 'childElementCount', 'previousSibling',
    'nextSibling', 'previousElementSibling', 'nextElementSibling', 'clientWidth', 'clientHeight', 'textContent', 'innerText', 'text',
    'innerHTML', 'outerHTML', 'attributes', 'classList', 'relList', 'sandbox', 'dataset', 'style', 'sheet',
    'hidden', 'title', 'lang', 'dir', 'type', 'name', 'method', 'enctype', 'encoding', 'action', 'disabled', 'readOnly', 'required', 'multiple',
    'placeholder', 'autocomplete', 'htmlFor', 'control', 'labels', 'contentEditable', 'isContentEditable',
    'options', 'selectedOptions', 'selectedIndex', 'length', 'size', 'label', 'selected', 'index',
    'files', 'value', 'defaultValue', 'selectionStart', 'selectionEnd', 'setSelectionRange', 'select', 'checked',
    'getAttribute', 'hasAttribute', 'setAttribute', 'removeAttribute', 'toggleAttribute',
    'getAttributeNames', 'hasAttributes', 'matches', 'closest', 'appendChild', 'removeChild',
    'insertBefore', 'replaceChildren', 'append', 'prepend', 'click', 'focus', 'blur',
    'scrollIntoView',
    'querySelector', 'querySelectorAll', 'getElementsByTagName', 'getElementsByClassName',
    'getBoundingClientRect', 'getClientRects',
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

  class VixenRange {
    constructor() {
      defineWritableValue(this, 'startContainer', vixenDocument);
      defineWritableValue(this, 'endContainer', vixenDocument);
      defineWritableValue(this, 'startOffset', 0);
      defineWritableValue(this, 'endOffset', 0);
      defineWritableValue(this, 'commonAncestorContainer', vixenDocument);
    }
    get collapsed() { return this.startContainer === this.endContainer && this.startOffset === this.endOffset; }
    setStart(node, offset) { this.startContainer = node; this.startOffset = Number(offset) || 0; }
    setEnd(node, offset) { this.endContainer = node; this.endOffset = Number(offset) || 0; }
    collapse(toStart = false) {
      if (toStart) {
        this.endContainer = this.startContainer;
        this.endOffset = this.startOffset;
      } else {
        this.startContainer = this.endContainer;
        this.startOffset = this.endOffset;
      }
    }
    selectNode(node) { this.startContainer = node.parentNode || vixenDocument; this.endContainer = this.startContainer; this.startOffset = 0; this.endOffset = 1; }
    selectNodeContents(node) { this.startContainer = node; this.endContainer = node; this.startOffset = 0; this.endOffset = node.childNodes ? node.childNodes.length : 0; }
    cloneRange() { const range = new VixenRange(); range.startContainer = this.startContainer; range.endContainer = this.endContainer; range.startOffset = this.startOffset; range.endOffset = this.endOffset; return range; }
    detach() {}
    deleteContents() {}
    extractContents() { return null; }
    cloneContents() { return null; }
    insertNode() {}
    surroundContents() {}
    isPointInRange() { return false; }
    comparePoint() { return 0; }
    intersectsNode() { return false; }
    toString() { return ''; }
  }

  class VixenSelection {
    get anchorNode() { return null; }
    get anchorOffset() { return 0; }
    get focusNode() { return null; }
    get focusOffset() { return 0; }
    get isCollapsed() { return true; }
    get rangeCount() { return 0; }
    get type() { return 'None'; }
    get direction() { return 'none'; }
    getRangeAt() { throw new TypeError('Selection has no ranges'); }
    addRange() {}
    removeRange() {}
    removeAllRanges() {}
    empty() {}
    collapse() {}
    setPosition() {}
    collapseToStart() {}
    collapseToEnd() {}
    extend() {}
    selectAllChildren() {}
    deleteFromDocument() {}
    containsNode() { return false; }
    toString() { return ''; }
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
    get visibilityState() { return 'visible'; }
    get hidden() { return false; }
    get referrer() { return ''; }
    get defaultView() { return globalThis; }
    get location() { return globalThis.location; }
    get documentElement() { return wrapElementByNodeId(data.documentElementNodeId); }
    get head() { return wrapElementByNodeId(data.headNodeId); }
    get body() { return wrapElementByNodeId(data.bodyNodeId); }
    get activeElement() { return wrapElementByNodeId(activeElementNodeId); }
    get scrollingElement() { return wrapElementByNodeId(data.scrollingElementNodeId); }
    get forms() { return new VixenHTMLCollection(data.collections.forms); }
    get images() { return new VixenHTMLCollection(data.collections.images); }
    get links() { return new VixenHTMLCollection(data.collections.links); }
    get scripts() { return new VixenHTMLCollection(data.collections.scripts); }
    getRootNode() { return this; }
    contains(target) { return nodeContains(this, target); }
    hasFocus() { return true; }
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
    createTextNode(data) { return new VixenText(String(data)); }
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
  const vixenSelection = new VixenSelection();
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
      if (keyword === 'auto' || keyword === 'manual') historyScrollRestoration = keyword;
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
        assert!(DOM_API_BOOTSTRAP.contains("op_vixen_dom_set_element_text"));
        assert!(DOM_API_BOOTSTRAP.contains("op_vixen_dom_set_element_attr"));
        assert!(DOM_API_BOOTSTRAP.contains("op_vixen_dom_set_element_inner_html"));
        assert!(DOM_API_BOOTSTRAP.contains("op_vixen_dom_set_control_value"));
        assert!(!DOM_API_BOOTSTRAP.contains("data.elements"));

        let page = Page::from_html(
            "file:///dom-op-snapshot.html",
            "<html><head><title>é—😀</title></head><body><p id='lead' data-emoji='é'>body é—😀</p></body></html>",
        )
        .unwrap();
        let host = dom_host_state(&page, DomMutationSink::default()).unwrap();
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
    fn element_geometry_crosses_dom_op() {
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
        let host = dom_host_state(&page, DomMutationSink::default()).unwrap();
        let box_record = host
            .elements
            .iter()
            .find(|record| record.id.as_deref() == Some("box"))
            .unwrap();
        let rect = rect_value(box_record.bbox.expect("box has layout bbox"));

        assert_eq!(rect["x"].as_f64(), Some(8.0));
        assert_eq!(rect["width"].as_f64(), Some(40.0));
        assert_eq!(rect["height"].as_f64(), Some(20.0));
        assert_eq!(element_record_value(box_record)["bbox"], rect);
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
        let host = dom_host_state(&page, DomMutationSink::default()).unwrap();
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
}
