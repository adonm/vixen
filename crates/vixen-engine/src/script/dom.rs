//! DOM snapshot host extension for the JS runtime.
//!
//! This module is the Phase 6 bridge from [`crate::page::Page`] snapshots into
//! a JS global. It deliberately exposes a small, read-only, fail-closed subset
//! while the full DOM/WebIDL binding layer is still landing.

#![forbid(unsafe_code)]

use std::borrow::Cow;
use std::sync::Arc;

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
    ],
    options = {
        host: Arc<DomHostState>,
    },
    state = |state, options| {
        state.put(DomHost(options.host))
    },
);

pub(super) fn extension(page: &Page) -> Result<Extension, EngineError> {
    let host = dom_host_state(page).map_err(|err| {
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
    json!({ "ok": true, "value": &record.text_content })
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

fn dom_host_state(page: &Page) -> Result<DomHostState, String> {
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
            "documentElementNodeId": document_element_node_id,
            "headNodeId": head_node_id,
            "bodyNodeId": body_node_id,
            "activeElementNodeId": body_node_id,
            "scrollingElementNodeId": document_element_node_id,
            "collections": {
                "forms": forms,
                "images": images,
                "links": links,
                "scripts": scripts,
            },
        }),
        elements: records,
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
        "innerHTML": &record.inner_html,
        "outerHTML": &record.outer_html,
        "parentNodeId": record.parent_node_id,
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
  } = Deno.core.ops;
  const webidl = globalThis.__vixenWebidl;
  const data = op_vixen_dom_snapshot();
  const elementObjects = new Map();
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

  function unwrapDomOp(result) {
    if (!result.ok) throw new TypeError(result.message);
    return result;
  }

  function findAllNodeIds(selector) {
    const result = unwrapDomOp(op_vixen_dom_query_selector_all(String(selector)));
    return result.nodeIds;
  }

  function elementMatches(nodeId, selector) {
    return unwrapDomOp(op_vixen_dom_element_matches(nodeId, String(selector))).matches;
  }

  function elementText(nodeId) {
    return unwrapDomOp(op_vixen_dom_element_text(nodeId)).value;
  }

  function elementAttribute(nodeId, name) {
    return unwrapDomOp(op_vixen_dom_element_attribute(nodeId, String(name))).value;
  }

  function elementTokens(nodeId, attribute) {
    return unwrapDomOp(op_vixen_dom_element_tokens(nodeId, attribute)).tokens;
  }

  function elementDataset(nodeId) {
    return unwrapDomOp(op_vixen_dom_element_dataset(nodeId)).pairs;
  }

  function elementRect(nodeId) {
    return unwrapDomOp(op_vixen_dom_element_rect(nodeId)).rect;
  }

  function formEntries(nodeId) {
    return unwrapDomOp(op_vixen_dom_form_entries(nodeId)).entries;
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

  function interfaceNameForTag(tag) {
    return htmlElementInterfaceByTag.get(String(tag).toLowerCase()) || 'HTMLElement';
  }

  function makeElementObject(record) {
    const ctor = webidl.interfaceConstructor(interfaceNameForTag(record.tag));
    const element = Object.create(ctor.prototype);
    Object.defineProperties(element, {
      __vixenNodeId: { value: record.nodeId, enumerable: false },
      __vixenRecord: { value: record, enumerable: false },
    });
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
    constructor(tokens) {
      Object.defineProperty(this, '__vixenTokens', {
        value: Object.freeze(tokens.slice()),
        enumerable: false,
      });
      for (let i = 0; i < this.__vixenTokens.length; i++) {
        Object.defineProperty(this, String(i), {
          value: this.__vixenTokens[i],
          enumerable: true,
          configurable: true,
        });
      }
    }
    get length() { return this.__vixenTokens.length; }
    get value() { return this.__vixenTokens.join(' '); }
    item(index) {
      const n = Number(index);
      const token = Number.isInteger(n) && n >= 0 ? this.__vixenTokens[n] : undefined;
      return token === undefined ? null : token;
    }
    contains(token) { return this.__vixenTokens.includes(validateToken(token)); }
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

  webidl.adoptInterface('Attr', VixenAttr);
  webidl.adoptInterface('NamedNodeMap', VixenNamedNodeMap);
  webidl.adoptInterface('NodeList', VixenNodeList);
  webidl.adoptInterface('HTMLCollection', VixenHTMLCollection);
  webidl.adoptInterface('DOMTokenList', VixenDOMTokenList);
  webidl.adoptInterface('DOMStringMap', VixenDOMStringMap);
  webidl.adoptInterface('DOMRectReadOnly', VixenDOMRectReadOnly);
  webidl.adoptInterface('DOMRectList', VixenDOMRectList);

  function cachedElementObject(element, key, make) {
    if (!Object.prototype.hasOwnProperty.call(element, key)) {
      Object.defineProperty(element, key, {
        value: make(),
        configurable: false,
      });
    }
    return element[key];
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

  class VixenElement {
    constructor(nodeId) {
      Object.defineProperty(this, '__vixenNodeId', {
        value: nodeId,
        enumerable: false,
      });
    }
    get id() { return elementRecord(this).id || ''; }
    get className() { return elementRecord(this).className; }
    get tagName() { return elementRecord(this).tag.toUpperCase(); }
    get nodeName() { return this.tagName; }
    get localName() { return elementRecord(this).tag; }
    get nodeType() { return 1; }
    get isConnected() { return true; }
    get ownerDocument() { return vixenDocument; }
    get parentNode() { return wrapElementByNodeId(elementRecord(this).parentNodeId); }
    get parentElement() { return this.parentNode; }
    get childNodes() { return new VixenNodeList(elementRecord(this).childElementNodeIds); }
    get children() { return cachedElementObject(this, '__vixenChildren', () => new VixenHTMLCollection(elementRecord(this).childElementNodeIds)); }
    get firstChild() { return wrapElementByNodeId(elementRecord(this).firstElementChildNodeId); }
    get lastChild() { return wrapElementByNodeId(elementRecord(this).lastElementChildNodeId); }
    get firstElementChild() { return this.firstChild; }
    get lastElementChild() { return this.lastChild; }
    get childElementCount() { return elementRecord(this).childElementNodeIds.length; }
    get previousSibling() { return wrapElementByNodeId(elementRecord(this).previousElementSiblingNodeId); }
    get nextSibling() { return wrapElementByNodeId(elementRecord(this).nextElementSiblingNodeId); }
    get previousElementSibling() { return this.previousSibling; }
    get nextElementSibling() { return this.nextSibling; }
    get textContent() { return elementText(this.__vixenNodeId); }
    get innerText() { return elementText(this.__vixenNodeId); }
    get innerHTML() { return elementRecord(this).innerHTML; }
    get outerHTML() { return elementRecord(this).outerHTML; }
    get attributes() { return cachedElementObject(this, '__vixenAttributes', () => new VixenNamedNodeMap(this)); }
    get classList() {
      return cachedElementObject(this, '__vixenClassList', () => new VixenDOMTokenList(elementTokens(this.__vixenNodeId, 'class')));
    }
    get relList() {
      return cachedElementObject(this, '__vixenRelList', () => new VixenDOMTokenList(elementTokens(this.__vixenNodeId, 'rel')));
    }
    get sandbox() {
      return cachedElementObject(this, '__vixenSandboxList', () => new VixenDOMTokenList(elementTokens(this.__vixenNodeId, 'sandbox')));
    }
    get dataset() {
      return cachedElementObject(this, '__vixenDataset', () => new VixenDOMStringMap(elementDataset(this.__vixenNodeId)));
    }
    getAttribute(name) {
      return elementAttribute(this.__vixenNodeId, name);
    }
    hasAttribute(name) { return elementAttribute(this.__vixenNodeId, name) !== null; }
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
    getBoundingClientRect() { return new VixenDOMRectReadOnly(elementRect(this.__vixenNodeId)); }
    getClientRects() { return makeDOMRectList(elementRect(this.__vixenNodeId)); }
  }

  webidl.adoptInterface('Element', VixenElement);

  const elementImplementationMembers = [
    'id', 'className', 'tagName', 'nodeName', 'localName', 'nodeType', 'isConnected',
    'ownerDocument', 'parentNode', 'parentElement', 'childNodes', 'children', 'firstChild',
    'lastChild', 'firstElementChild', 'lastElementChild', 'childElementCount', 'previousSibling',
    'nextSibling', 'previousElementSibling', 'nextElementSibling', 'textContent', 'innerText',
    'innerHTML', 'outerHTML', 'attributes', 'classList', 'relList', 'sandbox', 'dataset',
    'getAttribute', 'hasAttribute', 'getAttributeNames', 'hasAttributes', 'matches', 'closest',
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
    get URL() { return data.url; }
    get documentURI() { return data.url; }
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
    get activeElement() { return wrapElementByNodeId(data.activeElementNodeId); }
    get scrollingElement() { return wrapElementByNodeId(data.scrollingElementNodeId); }
    get forms() { return new VixenHTMLCollection(data.collections.forms); }
    get images() { return new VixenHTMLCollection(data.collections.images); }
    get links() { return new VixenHTMLCollection(data.collections.links); }
    get scripts() { return new VixenHTMLCollection(data.collections.scripts); }
    hasFocus() { return true; }
    querySelector(selector) { return this.querySelectorAll(selector).item(0); }
    querySelectorAll(selector) { return new VixenNodeList(findAllNodeIds(selector)); }
    getElementById(id) { return wrapElementByNodeId(op_vixen_dom_get_element_by_id(String(id))); }
    getElementsByTagName(tagName) { return new VixenHTMLCollection(findAllNodeIds(String(tagName) === '*' ? '*' : String(tagName))); }
    getElementsByClassName(className) { return new VixenHTMLCollection(findAllNodeIds('.' + String(className))); }
    createRange() { return new VixenRange(); }
    getSelection() { return vixenSelection; }
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
    get href() { return data.url; },
    toString() { return data.url; },
  };
  Object.defineProperty(globalThis, 'location', {
    value: locationObject,
    writable: true,
    configurable: true,
  });
  Object.defineProperty(globalThis, 'document', {
    value: vixenDocument,
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
        assert!(!DOM_API_BOOTSTRAP.contains("data.elements"));

        let page = Page::from_html(
            "file:///dom-op-snapshot.html",
            "<html><head><title>é—😀</title></head><body><p id='lead' data-emoji='é'>body é—😀</p></body></html>",
        )
        .unwrap();
        let host = dom_host_state(&page).unwrap();
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
        assert!(lead_value.get("textContent").is_none());
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

        let page = Page::from_html(
            "file:///dom-rect-op.html",
            "<style>#box { width: 40px; height: 20px; }</style><main><div id='box'>Box</div></main>",
        )
        .unwrap();
        let host = dom_host_state(&page).unwrap();
        let box_record = host
            .elements
            .iter()
            .find(|record| record.id.as_deref() == Some("box"))
            .unwrap();
        let rect = rect_value(box_record.bbox.expect("box has layout bbox"));

        assert_eq!(rect["x"].as_f64(), Some(8.0));
        assert_eq!(rect["width"].as_f64(), Some(40.0));
        assert_eq!(rect["height"].as_f64(), Some(20.0));
    }

    #[test]
    fn selector_behavior_crosses_finer_grained_ops() {
        assert!(DOM_API_BOOTSTRAP.contains("op_vixen_dom_query_selector_all"));
        assert!(DOM_API_BOOTSTRAP.contains("op_vixen_dom_get_element_by_id"));
        assert!(DOM_API_BOOTSTRAP.contains("op_vixen_dom_element_matches"));
        assert!(!DOM_API_BOOTSTRAP.contains("parseSimpleSelector"));
        assert!(!DOM_API_BOOTSTRAP.contains("recordMatches"));

        let page = Page::from_html(
            "file:///dom-selector-op.html",
            "<html><body><p id='lead' class='note callout'>Hello</p><p class='note'>Other</p></body></html>",
        )
        .unwrap();
        let host = dom_host_state(&page).unwrap();
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
