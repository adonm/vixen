//! HTML parsing — Phase 3 step 1 (docs/PLAN.md): `html5ever` parse into the
//! reference-counted DOM (`markup5ever_rcdom`). This module owns the parse
//! and simple tree projections (title, visible text, element count, dump).
//!
//! Selector matching is wired through `vixen-engine::style_dom`; the full
//! cascade-driving document model (`TNode` / `TElement` / `TDocument` plus
//! Stylo traversal) lands in the next Phase 3 slice. Until then this
//! `Document` is the parse tree behind `vixen_engine::page::Page`, the
//! headless CLI, and the WPT snapshot surface.

use std::cell::{Ref, RefCell};
use std::rc::Rc;

use html5ever::parse_document;
use markup5ever::{Attribute, LocalName, QualName, ns};
use markup5ever_rcdom::{Handle, Node, NodeData, RcDom};
use tendril::{StrTendril, stream::TendrilSink};

/// A parsed HTML document (owns the `RcDom`).
pub struct Document {
    /// Public to siblings in this crate (style_dom walks it). Hidden from
    /// the public API by the crate boundary.
    pub(crate) dom: RcDom,
}

/// Incremental owner-thread HTML parser used by BrowserCore navigation work.
pub(crate) struct DocumentParser {
    parser: html5ever::Parser<RcDom>,
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("HTML parse error: {0}")]
    Parse(String),
}

/// An inline classic `<script>` block ready for the JS execution boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineScript {
    /// Raw text content of the script element, in DOM/source order.
    pub source: String,
    /// CSP nonce, if one was authored on the `<script>` element.
    pub nonce: Option<String>,
}

/// An external classic `<script src>` block ready for the subresource boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalScript {
    /// Authored `src` attribute. Callers resolve this against the document base
    /// URL before CSP and network policy checks.
    pub src: String,
    /// CSP nonce, if one was authored on the `<script>` element.
    pub nonce: Option<String>,
}

/// Document-ordered events relevant to classic script execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentScriptItem {
    /// `<meta http-equiv="Content-Security-Policy" content="...">`.
    CspMeta(String),
    /// Inline classic `<script>` without `src` and with a JavaScript type.
    InlineClassicScript(InlineScript),
    /// External classic `<script src>` with a JavaScript type.
    ExternalClassicScript(ExternalScript),
}

/// Document-ordered author-style items used by the core resource loader and
/// authoritative cascade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentStyleItem {
    /// `<meta http-equiv="Content-Security-Policy" content="...">`.
    CspMeta(String),
    /// Raw text from an inline `<style>` element.
    InlineStyle(String),
    /// A parser-discovered non-alternate `<link rel="stylesheet" href>`.
    ExternalStylesheet { index: usize, href: String },
}

impl Document {
    /// Parse an HTML string.
    pub fn parse(html: &str) -> Result<Self, ParseError> {
        let mut parser = DocumentParser::new();
        parser.process(html);
        parser.finish()
    }

    /// The document `<title>` text, if any.
    pub fn title(&self) -> Option<String> {
        let mut out = None;
        walk(&self.dom.document, &mut |node| {
            if out.is_some() {
                return;
            }
            if let NodeData::Element { name, .. } = &node.data
                && name.local.as_ref() == "title"
            {
                out = Some(text_of(node));
            }
        });
        out
    }

    /// Set or create the document `<title>` text.
    pub fn set_title(&mut self, value: &str) -> Result<(), String> {
        if let Some(title) = first_element_by_tag(&self.dom.document, "title") {
            replace_element_text_content(&title, value);
            return Ok(());
        }

        let head = first_element_by_tag(&self.dom.document, "head")
            .ok_or_else(|| "document has no <head> element".to_owned())?;
        let title_dom = parse_document(RcDom::default(), Default::default())
            .one("<!doctype html><html><head><title></title></head><body></body></html>");
        let title = first_element_by_tag(&title_dom.document, "title")
            .ok_or_else(|| "title template parser produced no <title>".to_owned())?;
        replace_element_text_content(&title, value);
        title.parent.set(None);
        title.parent.set(Some(Rc::downgrade(&head)));
        head.children.borrow_mut().push(title);
        Ok(())
    }

    /// Concatenated visible text (text nodes, in document order). Comments
    /// and doctypes are excluded.
    pub fn text_content(&self) -> String {
        let mut buf = String::new();
        walk(&self.dom.document, &mut |node| {
            if let NodeData::Text { contents } = &node.data {
                buf.push_str(&contents.borrow());
            }
        });
        buf
    }

    /// Concatenated text under `<body>`, excluding `<head>/<title>` content.
    /// Used by the Phase 4 line-layout slice; falls back to full text when
    /// html5ever cannot find/synthesise a body.
    pub fn body_text_content(&self) -> String {
        let mut body = None;
        walk(&self.dom.document, &mut |node| {
            if body.is_some() {
                return;
            }
            if let NodeData::Element { name, .. } = &node.data
                && name.local.as_ref() == "body"
            {
                body = Some(Rc::clone(node));
            }
        });
        body.map(|node| text_content_of(&node))
            .unwrap_or_else(|| self.text_content())
    }

    /// Raw text contents of author `<style>` blocks, in document order.
    pub fn style_blocks(&self) -> Vec<String> {
        self.style_execution_items()
            .into_iter()
            .filter_map(|item| match item {
                DocumentStyleItem::InlineStyle(source) => Some(source),
                DocumentStyleItem::CspMeta(_) | DocumentStyleItem::ExternalStylesheet { .. } => {
                    None
                }
            })
            .collect()
    }

    /// Document-ordered CSP-meta, inline-style, and external stylesheet items.
    pub fn style_execution_items(&self) -> Vec<DocumentStyleItem> {
        let mut out = Vec::new();
        let mut external_index = 0;
        walk(&self.dom.document, &mut |node| {
            let NodeData::Element { name, attrs, .. } = &node.data else {
                return;
            };
            let tag = name.local.as_ref();
            if tag == "meta" {
                if let Some(policy) = csp_meta_policy(&attrs.borrow()) {
                    out.push(DocumentStyleItem::CspMeta(policy));
                }
                return;
            }
            if tag == "style" {
                out.push(DocumentStyleItem::InlineStyle(text_content_of(node)));
                return;
            }
            if tag != "link" {
                return;
            }
            let attrs = attrs.borrow();
            let rel = attrs
                .iter()
                .find(|attr| attr.name.local.as_ref() == "rel")
                .map(|attr| attr.value.as_ref())
                .unwrap_or_default();
            let is_stylesheet = rel
                .split_ascii_whitespace()
                .any(|token| token.eq_ignore_ascii_case("stylesheet"));
            let is_alternate = rel
                .split_ascii_whitespace()
                .any(|token| token.eq_ignore_ascii_case("alternate"));
            if !is_stylesheet || is_alternate {
                return;
            }
            let media_supported = attrs
                .iter()
                .find(|attr| attr.name.local.as_ref() == "media")
                .map(|attr| attr.value.trim())
                .is_none_or(|media| {
                    media.is_empty()
                        || media.eq_ignore_ascii_case("all")
                        || media.eq_ignore_ascii_case("screen")
                });
            let type_supported = attrs
                .iter()
                .find(|attr| attr.name.local.as_ref() == "type")
                .map(|attr| attr.value.trim())
                .is_none_or(|kind| kind.is_empty() || kind.eq_ignore_ascii_case("text/css"));
            if !media_supported || !type_supported {
                return;
            }
            if let Some(href) = attrs
                .iter()
                .find(|attr| attr.name.local.as_ref() == "href")
                .map(|attr| attr.value.to_string())
                .filter(|href| !href.trim().is_empty())
            {
                out.push(DocumentStyleItem::ExternalStylesheet {
                    index: external_index,
                    href,
                });
                external_index += 1;
            }
        });
        out
    }

    /// Inline classic `<script>` blocks, in document order.
    ///
    /// External scripts (`src`) and non-classic script types (`module`,
    /// `importmap`, JSON data blocks, etc.) are intentionally excluded; fetching
    /// external scripts is a separate network/CSP/MIME trust boundary.
    pub fn inline_classic_scripts(&self) -> Vec<InlineScript> {
        self.script_execution_items()
            .into_iter()
            .filter_map(|item| match item {
                DocumentScriptItem::InlineClassicScript(script) => Some(script),
                DocumentScriptItem::CspMeta(_) | DocumentScriptItem::ExternalClassicScript(_) => {
                    None
                }
            })
            .collect()
    }

    /// True when the document contains an inline or external classic script.
    pub fn has_classic_scripts(&self) -> bool {
        self.script_execution_items().into_iter().any(|item| {
            matches!(
                item,
                DocumentScriptItem::InlineClassicScript(_)
                    | DocumentScriptItem::ExternalClassicScript(_)
            )
        })
    }

    /// Document-ordered CSP-meta and classic-script items. Callers run through
    /// this sequence to apply meta CSP before later scripts.
    pub fn script_execution_items(&self) -> Vec<DocumentScriptItem> {
        let mut out = Vec::new();
        walk(&self.dom.document, &mut |node| {
            let NodeData::Element { name, attrs, .. } = &node.data else {
                return;
            };
            let tag = name.local.as_ref();
            if tag == "meta" {
                let policy = {
                    let attrs = attrs.borrow();
                    let is_csp = attrs.iter().any(|attr| {
                        attr.name.local.as_ref() == "http-equiv"
                            && attr
                                .value
                                .trim()
                                .eq_ignore_ascii_case("Content-Security-Policy")
                    });
                    if is_csp {
                        attrs
                            .iter()
                            .find(|attr| attr.name.local.as_ref() == "content")
                            .map(|attr| attr.value.to_string())
                    } else {
                        None
                    }
                };
                if let Some(policy) = policy {
                    out.push(DocumentScriptItem::CspMeta(policy));
                }
                return;
            }

            if tag == "script" {
                let script_item = {
                    let attrs = attrs.borrow();
                    let src = attrs
                        .iter()
                        .find(|attr| attr.name.local.as_ref() == "src")
                        .map(|attr| attr.value.to_string());
                    let script_type = attrs
                        .iter()
                        .find(|attr| attr.name.local.as_ref() == "type")
                        .map(|attr| attr.value.to_string());
                    if is_classic_script_type(script_type.as_deref()) {
                        let nonce = attrs
                            .iter()
                            .find(|attr| attr.name.local.as_ref() == "nonce")
                            .map(|attr| attr.value.to_string());
                        Some((src, nonce))
                    } else {
                        None
                    }
                };
                if let Some((src, nonce)) = script_item {
                    if let Some(src) = src {
                        out.push(DocumentScriptItem::ExternalClassicScript(ExternalScript {
                            src,
                            nonce,
                        }));
                    } else {
                        out.push(DocumentScriptItem::InlineClassicScript(InlineScript {
                            source: text_content_of(node),
                            nonce,
                        }));
                    }
                }
            }
        });
        out
    }

    /// Number of element nodes (a coarse `min-nodes`/`dom-nodes-range` signal).
    pub fn element_count(&self) -> usize {
        let mut n = 0;
        walk(&self.dom.document, &mut |node| {
            if matches!(node.data, NodeData::Element { .. }) {
                n += 1;
            }
        });
        n
    }

    /// Indented tree dump (used by `vixen-headless --dump-dom`).
    pub fn dump(&self) -> String {
        let mut buf = String::new();
        dump_node(&self.dom.document, 0, &mut buf);
        buf
    }

    /// Replace an element's children with a single text node, mirroring the DOM
    /// `Element.textContent = ...` mutation at the current Page boundary.
    pub fn set_element_text_content(&mut self, node_id: usize, value: &str) -> Result<(), String> {
        let element = self
            .element_handle_by_node_id(node_id)
            .ok_or_else(|| format!("DOM mutation target not found: node {node_id}"))?;

        let mut new_children = Vec::new();
        if !value.is_empty() {
            new_children.push(Node::new(NodeData::Text {
                contents: RefCell::new(value.into()),
            }));
        }
        replace_children(&element, new_children);

        Ok(())
    }

    /// Set or replace an HTML attribute on an element by stable node id.
    pub fn set_element_attribute(
        &mut self,
        node_id: usize,
        name: &str,
        value: &str,
    ) -> Result<(), String> {
        let name = normalize_html_attribute_name(name)?;
        let element = self
            .element_handle_by_node_id(node_id)
            .ok_or_else(|| format!("DOM mutation target not found: node {node_id}"))?;
        let NodeData::Element { attrs, .. } = &element.data else {
            return Err(format!(
                "DOM mutation target is not an element: node {node_id}"
            ));
        };

        let mut attrs = attrs.borrow_mut();
        if let Some(attr) = attrs
            .iter_mut()
            .find(|attr| attr.name.local.as_ref() == name.as_str())
        {
            attr.value = StrTendril::from(value);
        } else {
            attrs.push(Attribute {
                name: html_attribute_qual_name(&name),
                value: StrTendril::from(value),
            });
        }

        Ok(())
    }

    /// Remove an HTML attribute from an element by stable node id.
    pub fn remove_element_attribute(&mut self, node_id: usize, name: &str) -> Result<(), String> {
        let name = normalize_html_attribute_name(name)?;
        let element = self
            .element_handle_by_node_id(node_id)
            .ok_or_else(|| format!("DOM mutation target not found: node {node_id}"))?;
        let NodeData::Element { attrs, .. } = &element.data else {
            return Err(format!(
                "DOM mutation target is not an element: node {node_id}"
            ));
        };

        attrs
            .borrow_mut()
            .retain(|attr| attr.name.local.as_ref() != name.as_str());

        Ok(())
    }

    /// Replace an element's child subtree with an HTML fragment, mirroring the
    /// structural DOM mutation boundary (`innerHTML`, `appendChild`,
    /// `replaceChildren`) used by the JS runtime host bridge.
    pub fn set_element_inner_html(&mut self, node_id: usize, html: &str) -> Result<(), String> {
        let element = self
            .element_handle_by_node_id(node_id)
            .ok_or_else(|| format!("DOM mutation target not found: node {node_id}"))?;
        match &element.data {
            NodeData::Element { .. } => {}
            _ => {
                return Err(format!(
                    "DOM mutation target is not an element: node {node_id}"
                ));
            }
        };

        let fragment_dom = parse_document(RcDom::default(), Default::default())
            .one(format!("<!doctype html><html><body>{html}</body></html>"));
        let body = first_element_by_tag(&fragment_dom.document, "body")
            .ok_or_else(|| "DOM mutation fragment parser produced no body".to_owned())?;
        let mut new_children = {
            let mut body_children = body.children.borrow_mut();
            let children = body_children.clone();
            body_children.clear();
            children
        };
        for child in &new_children {
            child.parent.set(None);
        }
        replace_children(&element, std::mem::take(&mut new_children));

        Ok(())
    }

    /// Commit a live form-control value. Falls back from the page-realm node id
    /// to id/name/tag matching because structural mutations can shift the
    /// document-order node ids used by the current lightweight DOM bridge.
    pub fn set_form_control_value(
        &mut self,
        node_id: usize,
        element_id: Option<&str>,
        control_name: Option<&str>,
        tag: &str,
        value: &str,
    ) -> Result<(), String> {
        let tag = tag.trim().to_ascii_lowercase();
        if !matches!(tag.as_str(), "input" | "textarea") {
            return Err(format!("unsupported form control value target: {tag}"));
        }
        let element_id = element_id.filter(|value| !value.is_empty());
        let control_name = control_name.filter(|value| !value.is_empty());
        let element = self
            .element_handle_by_node_id(node_id)
            .filter(|node| control_identity_matches(node, &tag, element_id, control_name))
            .or_else(|| find_control_by_identity(&self.dom.document, &tag, element_id, None))
            .or_else(|| find_control_by_identity(&self.dom.document, &tag, None, control_name))
            .ok_or_else(|| format!("DOM form control target not found: node {node_id}"))?;

        match element_tag_name(&element).as_deref() {
            Some("input") => set_html_attribute_on_element(&element, "value", value),
            Some("textarea") => {
                replace_element_text_content(&element, value);
                Ok(())
            }
            _ => Err(format!(
                "DOM form control target is not editable: node {node_id}"
            )),
        }
    }

    fn element_handle_by_node_id(&self, node_id: usize) -> Option<Handle> {
        let idx = node_id.checked_sub(1)?;
        let mut current = 0;
        let mut found = None;
        walk(&self.dom.document, &mut |node| {
            if found.is_some() {
                return;
            }
            if matches!(node.data, NodeData::Element { .. }) {
                if current == idx {
                    found = Some(Rc::clone(node));
                }
                current += 1;
            }
        });
        found
    }
}

fn csp_meta_policy(attrs: &[Attribute]) -> Option<String> {
    let is_csp = attrs.iter().any(|attr| {
        attr.name.local.as_ref() == "http-equiv"
            && attr
                .value
                .trim()
                .eq_ignore_ascii_case("Content-Security-Policy")
    });
    is_csp.then(|| {
        attrs
            .iter()
            .find(|attr| attr.name.local.as_ref() == "content")
            .map(|attr| attr.value.to_string())
    })?
}

impl DocumentParser {
    pub(crate) fn new() -> Self {
        Self {
            parser: parse_document(RcDom::default(), Default::default()),
        }
    }

    pub(crate) fn process(&mut self, html: &str) {
        self.parser.process(StrTendril::from(html));
    }

    pub(crate) fn finish(self) -> Result<Document, ParseError> {
        // html5ever recovers from arbitrary HTML input. ParseError remains the
        // stable boundary for future strict parser modes.
        Ok(Document {
            dom: self.parser.finish(),
        })
    }
}

fn normalize_html_attribute_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("DOM mutation attribute name must not be empty".to_owned());
    }
    if name.bytes().any(|byte| {
        byte.is_ascii_whitespace() || matches!(byte, b'\0' | b'"' | b'\'' | b'>' | b'/' | b'=')
    }) {
        return Err(format!("DOM mutation attribute name is invalid: {name}"));
    }
    Ok(name.to_ascii_lowercase())
}

fn html_attribute_qual_name(name: &str) -> QualName {
    QualName::new(None, ns!(), LocalName::from(name))
}

fn set_html_attribute_on_element(element: &Handle, name: &str, value: &str) -> Result<(), String> {
    let name = normalize_html_attribute_name(name)?;
    let NodeData::Element { attrs, .. } = &element.data else {
        return Err("DOM mutation target is not an element".to_owned());
    };

    let mut attrs = attrs.borrow_mut();
    if let Some(attr) = attrs
        .iter_mut()
        .find(|attr| attr.name.local.as_ref() == name.as_str())
    {
        attr.value = StrTendril::from(value);
    } else {
        attrs.push(Attribute {
            name: html_attribute_qual_name(&name),
            value: StrTendril::from(value),
        });
    }
    Ok(())
}

fn replace_element_text_content(element: &Handle, value: &str) {
    let mut new_children = Vec::new();
    if !value.is_empty() {
        new_children.push(Node::new(NodeData::Text {
            contents: RefCell::new(value.into()),
        }));
    }
    replace_children(element, new_children);
}

fn replace_children(parent: &Handle, new_children: Vec<Handle>) {
    let mut children = parent.children.borrow_mut();
    for child in children.iter() {
        child.parent.set(None);
    }
    children.clear();
    for child in new_children {
        child.parent.set(Some(Rc::downgrade(parent)));
        children.push(child);
    }
}

fn element_tag_name(node: &Handle) -> Option<String> {
    match &node.data {
        NodeData::Element { name, .. } => Some(name.local.to_string()),
        _ => None,
    }
}

fn element_attr_value(node: &Handle, wanted: &str) -> Option<String> {
    let NodeData::Element { attrs, .. } = &node.data else {
        return None;
    };
    attrs
        .borrow()
        .iter()
        .find(|attr| attr.name.local.as_ref() == wanted)
        .map(|attr| attr.value.to_string())
}

fn control_identity_matches(
    node: &Handle,
    tag: &str,
    element_id: Option<&str>,
    control_name: Option<&str>,
) -> bool {
    if element_tag_name(node).as_deref() != Some(tag) {
        return false;
    }
    if let Some(id) = element_id {
        return element_attr_value(node, "id").as_deref() == Some(id);
    }
    if let Some(name) = control_name {
        return element_attr_value(node, "name").as_deref() == Some(name);
    }
    true
}

fn find_control_by_identity(
    root: &Handle,
    tag: &str,
    element_id: Option<&str>,
    control_name: Option<&str>,
) -> Option<Handle> {
    let mut found = None;
    walk(root, &mut |node| {
        if found.is_some() {
            return;
        }
        if control_identity_matches(node, tag, element_id, control_name) {
            found = Some(Rc::clone(node));
        }
    });
    found
}

fn first_element_by_tag(root: &Handle, tag: &str) -> Option<Handle> {
    let mut found = None;
    walk(root, &mut |node| {
        if found.is_some() {
            return;
        }
        if let NodeData::Element { name, .. } = &node.data
            && name.local.as_ref() == tag
        {
            found = Some(Rc::clone(node));
        }
    });
    found
}

/// First text child of `node`, trimmed.
fn text_of(node: &Handle) -> String {
    let mut s = String::new();
    for child in node.children.borrow().iter() {
        if let NodeData::Text { contents } = &child.data {
            s.push_str(&contents.borrow());
        }
    }
    s.trim().to_owned()
}

fn text_content_of(node: &Handle) -> String {
    let mut buf = String::new();
    walk(node, &mut |node| {
        if let NodeData::Text { contents } = &node.data {
            buf.push_str(&contents.borrow());
        }
    });
    buf
}

fn is_classic_script_type(script_type: Option<&str>) -> bool {
    let Some(script_type) = script_type else {
        return true;
    };
    let essence = script_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if essence.is_empty() {
        return true;
    }
    matches!(
        essence.as_str(),
        "application/ecmascript"
            | "application/javascript"
            | "application/x-ecmascript"
            | "application/x-javascript"
            | "text/ecmascript"
            | "text/javascript"
            | "text/javascript1.0"
            | "text/javascript1.1"
            | "text/javascript1.2"
            | "text/javascript1.3"
            | "text/javascript1.4"
            | "text/javascript1.5"
            | "text/jscript"
            | "text/livescript"
            | "text/x-ecmascript"
            | "text/x-javascript"
    )
}

fn children_of(node: &Handle) -> Ref<'_, Vec<Handle>> {
    node.children.borrow()
}

/// Pre-order DFS walk; `f` runs on every node (document root included).
fn walk<F: FnMut(&Handle)>(root: &Handle, f: &mut F) {
    f(root);
    let children: Vec<Handle> = children_of(root).clone();
    for child in &children {
        walk(child, f);
    }
}

fn dump_node(node: &Handle, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    match &node.data {
        NodeData::Document => out.push_str(&format!("{indent}#document\n")),
        NodeData::Doctype { name, .. } => out.push_str(&format!("{indent}<!doctype {}>\n", name)),
        NodeData::Text { contents } => {
            let borrowed = contents.borrow();
            let t = borrowed.trim_end_matches(['\n', '\r', ' ']);
            if !t.is_empty() {
                out.push_str(&format!("{indent}\"{t}\"\n"));
            }
        }
        NodeData::Comment { contents } => {
            out.push_str(&format!("{indent}<!-- {} -->\n", contents.trim()))
        }
        NodeData::Element { name, attrs, .. } => {
            let tag = name.local.as_ref();
            let attrs = attrs.borrow();
            let attr_str = if attrs.is_empty() {
                String::new()
            } else {
                let pairs: Vec<String> = attrs
                    .iter()
                    .map(|a| format!("{}=\"{}\"", a.name.local, a.value))
                    .collect();
                format!(" {}", pairs.join(" "))
            };
            out.push_str(&format!("{indent}<{tag}{attr_str}>\n"));
        }
        NodeData::ProcessingInstruction { target, contents } => {
            out.push_str(&format!("{indent}<?{target} {contents}>\n"))
        }
    }
    for child in node.children.borrow().iter() {
        // Clone the Rc to avoid holding the borrow across recursion.
        let child = Rc::clone(child);
        dump_node(&child, depth + 1, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_title_text_and_count() {
        let doc = Document::parse(
            "<html><head><title>Hello</title></head>\
             <body><p>One <b>two</b></p><!-- c --><p>Three</p></body></html>",
        )
        .unwrap();
        assert_eq!(doc.title().as_deref(), Some("Hello"));
        // Visible text excludes the comment.
        let text = doc.text_content();
        assert!(text.contains("One") && text.contains("two") && text.contains("Three"));
        assert!(!text.contains("c"));
        // Elements: html, head, title, body, p, b, p = 7.
        assert_eq!(doc.element_count(), 7);
    }

    #[test]
    fn incremental_parser_preserves_tokens_across_chunks() {
        let mut parser = DocumentParser::new();
        for chunk in [
            "<!doctype html><tit",
            "le>Incremental</title><main>caf",
            "e\u{301}</main>",
        ] {
            parser.process(chunk);
        }
        let doc = parser.finish().unwrap();

        assert_eq!(doc.title().as_deref(), Some("Incremental"));
        assert_eq!(doc.body_text_content(), "cafe\u{301}");
    }

    #[test]
    fn body_text_excludes_head_and_title() {
        let doc = Document::parse(
            "<html><head><title>Hidden title</title></head><body><p>Visible <b>body</b></p></body></html>",
        )
        .unwrap();
        let body = doc.body_text_content();
        assert!(body.contains("Visible"));
        assert!(body.contains("body"));
        assert!(!body.contains("Hidden title"));
    }

    #[test]
    fn style_blocks_are_collected_in_document_order() {
        let doc = Document::parse(
            "<html><head><style>p { color: red }</style><style>.x { display: grid }</style></head><body></body></html>",
        )
        .unwrap();
        let blocks = doc.style_blocks();
        assert_eq!(blocks.len(), 2);
        assert!(blocks[0].contains("color: red"));
        assert!(blocks[1].contains("display: grid"));
    }

    #[test]
    fn style_items_keep_csp_inline_and_external_order() {
        let doc = Document::parse(
            "<meta http-equiv='Content-Security-Policy' content=\"style-src 'self'\">\
             <style>body { color: red }</style>\
             <link rel='icon stylesheet' href='/first.css'>\
             <link rel='alternate stylesheet' href='/alternate.css'>\
             <link rel='stylesheet' media='print' href='/print.css'>\
             <link rel='stylesheet' type='text/plain' href='/plain.css'>\
             <link rel='stylesheet' href='/second.css'>",
        )
        .unwrap();

        assert_eq!(
            doc.style_execution_items(),
            vec![
                DocumentStyleItem::CspMeta("style-src 'self'".to_owned()),
                DocumentStyleItem::InlineStyle("body { color: red }".to_owned()),
                DocumentStyleItem::ExternalStylesheet {
                    index: 0,
                    href: "/first.css".to_owned(),
                },
                DocumentStyleItem::ExternalStylesheet {
                    index: 1,
                    href: "/second.css".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn inline_classic_scripts_are_collected_in_document_order() {
        let doc = Document::parse(
            "<script>globalThis.a = 1;</script>\
             <script type='text/javascript; charset=utf-8' nonce='abc'>globalThis.b = 2;</script>\
             <script type='module'>globalThis.moduleRan = true;</script>\
             <script type='application/json'>{\"not\":\"code\"}</script>\
             <script src='/app.js'>globalThis.externalFallback = true;</script>\
             <script type='application/javascript'>globalThis.c = 3;</script>",
        )
        .unwrap();

        let scripts = doc.inline_classic_scripts();
        assert_eq!(scripts.len(), 3);
        assert!(scripts[0].source.contains("a = 1"));
        assert_eq!(scripts[0].nonce, None);
        assert!(scripts[1].source.contains("b = 2"));
        assert_eq!(scripts[1].nonce.as_deref(), Some("abc"));
        assert!(scripts[2].source.contains("c = 3"));
    }

    #[test]
    fn script_execution_items_include_meta_csp_in_order() {
        let doc = Document::parse(
            "<script>before()</script>\
             <meta http-equiv='Content-Security-Policy' content=\"script-src 'self'\">\
             <script src='/after.js' nonce='n'></script>\
             <script nonce='n'>after()</script>",
        )
        .unwrap();

        let items = doc.script_execution_items();
        assert_eq!(items.len(), 4);
        assert!(doc.has_classic_scripts());
        assert!(matches!(
            &items[0],
            DocumentScriptItem::InlineClassicScript(script) if script.source.contains("before")
        ));
        assert_eq!(
            items[1],
            DocumentScriptItem::CspMeta("script-src 'self'".to_owned())
        );
        assert!(matches!(
            &items[2],
            DocumentScriptItem::ExternalClassicScript(script)
                if script.src == "/after.js" && script.nonce.as_deref() == Some("n")
        ));
        assert!(matches!(
            &items[3],
            DocumentScriptItem::InlineClassicScript(script)
                if script.source.contains("after") && script.nonce.as_deref() == Some("n")
        ));
    }

    #[test]
    fn dump_renders_tree() {
        let doc = Document::parse("<html><body><p>hi</p></body></html>").unwrap();
        let dump = doc.dump();
        assert!(dump.contains("<html"));
        assert!(dump.contains("<body"));
        assert!(dump.contains("<p"));
        assert!(dump.contains("\"hi\""));
    }

    #[test]
    fn empty_and_garbage_input_does_not_panic() {
        // Empty input still synthesises the implicit html/head/body shell.
        assert_eq!(Document::parse("").unwrap().element_count(), 3);
        // Garbage HTML is still parsed (html5ever is highly permissive).
        let doc = Document::parse("<<<>>>not html<<<<").unwrap();
        let _ = doc.dump();
    }
}
