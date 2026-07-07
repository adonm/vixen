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
use crate::page::Page;

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
    class_tokens: Vec<String>,
    rel_tokens: Vec<String>,
    sandbox_tokens: Vec<String>,
    dataset: Vec<(String, String)>,
}

enum SimpleSelector {
    All,
    Id(String),
    Class(String),
    Tag(String),
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
    let body_node_id = records
        .iter()
        .find(|record| record.tag.eq_ignore_ascii_case("body"))
        .map(|record| record.node_id);

    Ok(DomHostState {
        snapshot: json!({
            "title": page.document().title().unwrap_or_default(),
            "url": page.url(),
            "documentElementNodeId": document_element_node_id,
            "bodyNodeId": body_node_id,
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
        class_tokens: dom_token_list(info, "class"),
        rel_tokens: dom_token_list(info, "rel"),
        sandbox_tokens: dom_token_list(info, "sandbox"),
        dataset: dataset_pairs(info),
    }
}

fn element_record_value(record: &DomElementRecord) -> deno_core::serde_json::Value {
    json!({
        "nodeId": record.node_id,
        "tag": &record.tag,
        "id": &record.id,
        "className": record.classes.join(" "),
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
    let parsed = parse_simple_selector(selector)?;
    Ok(host
        .elements
        .iter()
        .filter(|record| record_matches(record, &parsed))
        .map(|record| record.node_id)
        .collect())
}

fn parse_simple_selector(selector: &str) -> Result<SimpleSelector, String> {
    let raw = selector.trim();
    if raw == "*" {
        return Ok(SimpleSelector::All);
    }
    if let Some(id) = raw.strip_prefix('#')
        && is_simple_dom_name(id)
    {
        return Ok(SimpleSelector::Id(id.to_owned()));
    }
    if let Some(class) = raw.strip_prefix('.')
        && is_simple_dom_name(class)
    {
        return Ok(SimpleSelector::Class(class.to_owned()));
    }
    if is_simple_tag_name(raw) {
        return Ok(SimpleSelector::Tag(raw.to_ascii_lowercase()));
    }
    Err(format!(
        "Vixen DOM host currently supports simple #id, .class, tag, and * selectors: {raw}"
    ))
}

fn is_simple_dom_name(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn is_simple_tag_name(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphabetic()
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn record_matches(record: &DomElementRecord, selector: &SimpleSelector) -> bool {
    match selector {
        SimpleSelector::All => true,
        SimpleSelector::Id(id) => record.id.as_deref() == Some(id.as_str()),
        SimpleSelector::Class(class) => record.classes.iter().any(|name| name == class),
        SimpleSelector::Tag(tag) => record.tag.eq_ignore_ascii_case(tag),
    }
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
  } = Deno.core.ops;
  const data = op_vixen_dom_snapshot();
  const elementObjects = new Map();

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

  function wrapElementByNodeId(nodeId) {
    if (nodeId === null || nodeId === undefined) return null;
    if (!elementObjects.has(nodeId)) {
      elementObjects.set(nodeId, new VixenElement(nodeId));
    }
    return elementObjects.get(nodeId);
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

  function cachedElementObject(element, key, make) {
    if (!Object.prototype.hasOwnProperty.call(element, key)) {
      Object.defineProperty(element, key, {
        value: make(),
        configurable: false,
      });
    }
    return element[key];
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
    get textContent() { return elementText(this.__vixenNodeId); }
    get innerText() { return elementText(this.__vixenNodeId); }
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
    matches(selector) { return elementMatches(this.__vixenNodeId, selector); }
  }

  const vixenDocument = {};
  Object.defineProperties(vixenDocument, {
    title: { get() { return data.title; }, enumerable: true, configurable: true },
    URL: { get() { return data.url; }, enumerable: true, configurable: true },
    documentURI: { get() { return data.url; }, enumerable: true, configurable: true },
    readyState: { get() { return 'complete'; }, enumerable: true, configurable: true },
    body: {
      get() { return wrapElementByNodeId(data.bodyNodeId); },
      enumerable: true,
      configurable: true,
    },
    documentElement: {
      get() { return wrapElementByNodeId(data.documentElementNodeId); },
      enumerable: true,
      configurable: true,
    },
  });
  Object.defineProperties(vixenDocument, {
    querySelector: {
      value(selector) { return wrapElementByNodeId(findAllNodeIds(selector)[0]); },
      enumerable: true,
      configurable: true,
    },
    querySelectorAll: {
      value(selector) { return findAllNodeIds(selector).map(wrapElementByNodeId); },
      enumerable: true,
      configurable: true,
    },
    getElementById: {
      value(id) { return wrapElementByNodeId(op_vixen_dom_get_element_by_id(String(id))); },
      enumerable: true,
      configurable: true,
    },
  });
  Object.defineProperty(globalThis, 'document', {
    value: vixenDocument,
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
        assert!(query_selector_node_ids(&host, "p.note").is_err());

        let selector = parse_simple_selector(".callout").unwrap();
        let lead = host
            .elements
            .iter()
            .find(|record| record.node_id == lead_id)
            .unwrap();
        assert!(record_matches(lead, &selector));
    }
}
