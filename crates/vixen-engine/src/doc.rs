//! HTML parsing — Phase 3 step 1 (docs/PLAN.md): `html5ever` parse into the
//! reference-counted DOM (`markup5ever_rcdom`). This module owns the parse
//! and simple tree projections (title, visible text, element count, dump).
//!
//! Selector matching is wired through `vixen-engine::style_dom`; the full
//! cascade-driving document model (`TNode` / `TElement` / `TDocument` plus
//! Stylo traversal) lands in the next Phase 3 slice. Until then this
//! `Document` is the parse tree behind `vixen_engine::page::Page`, the
//! headless CLI, and the WPT snapshot surface.

use std::cell::Ref;
use std::rc::Rc;

use html5ever::parse_document;
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use tendril::stream::TendrilSink;

/// A parsed HTML document (owns the `RcDom`).
pub struct Document {
    /// Public to siblings in this crate (style_dom walks it). Hidden from
    /// the public API by the crate boundary.
    pub(crate) dom: RcDom,
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("HTML parse error: {0}")]
    Parse(String),
}

impl Document {
    /// Parse an HTML string.
    pub fn parse(html: &str) -> Result<Self, ParseError> {
        // `parse_document(...).one(...)` is infallible (the parser recovers
        // from any input); `ParseError` is reserved for future strict modes.
        let dom = parse_document(RcDom::default(), Default::default()).one(html);
        Ok(Self { dom })
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
        let mut out = Vec::new();
        walk(&self.dom.document, &mut |node| {
            if let NodeData::Element { name, .. } = &node.data
                && name.local.as_ref() == "style"
            {
                out.push(text_content_of(node));
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
