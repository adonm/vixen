//! Vertical page pipeline facade — the small integration seam every large
//! browser milestone should extend.
//!
//! Today this owns the loaded URL + parsed [`crate::doc::Document`] and exposes
//! the common inspection operations used by headless and the WPT harness
//! (snapshot, text, selector query, diagnostics). Phase 3 cascade, Phase 4
//! layout, and Phase 5 paint should extend this type in order rather than land
//! as more isolated pure modules.
//!
//! The intended growth path is deliberately boring:
//! `Page::from_html` → `compute_styles()` → `layout()` → `build_display_list()`
//! → `render(&dyn vixen_api::GlContext)`. Each step adds state behind this
//! facade and is proven by a `just gate-*` command.

#![forbid(unsafe_code)]

use vixen_api::{ElementInfo, EngineDiagnostic, EngineInspector, PageSnapshot};

use crate::display_list::{
    BackgroundAttachment, BackgroundBox, Color, DisplayListBuilder, DrawItem, PaintCommand,
    PaintStats, Rect, TextRun, dump_paint_commands, dump_paint_stats,
};
use crate::doc::{Document, ParseError};
use crate::layout_tree::{
    LayoutNode, LayoutNodeKind, LayoutTree, build_layout_tree, dump_layout_tree,
    line_boxes_from_tree,
};
use crate::line_layout::{LineBox, dump_line_boxes};
use crate::style_cascade::AuthorStylesheet;
use crate::style_dom::{ElementRelation, Selector};

mod interaction;
pub use interaction::FormSubmissionSnapshot;

/// A loaded page at the current vertical integration boundary.
pub struct Page {
    url: String,
    document: Document,
    author_stylesheet: AuthorStylesheet,
    diagnostics: Vec<EngineDiagnostic>,
}

/// Errors produced while constructing a [`Page`].
#[derive(Debug, thiserror::Error)]
pub enum PageError {
    #[error(transparent)]
    Parse(#[from] ParseError),
}

impl Page {
    /// Build a page from already-loaded HTML. The fetch/source loader is owned
    /// by the caller for now (`vixen-headless` reads `file://`; the future
    /// network path will call into `vixen-net` before this boundary).
    pub fn from_html(url: impl Into<String>, html: &str) -> Result<Self, PageError> {
        let document = Document::parse(html)?;
        let author_stylesheet = AuthorStylesheet::from_blocks(&document.style_blocks());
        Ok(Self {
            url: url.into(),
            document,
            author_stylesheet,
            diagnostics: Vec::new(),
        })
    }

    /// The loaded URL, as authored by the caller.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The parsed document. Kept available for narrow integration seams while
    /// existing code migrates to the facade methods.
    pub fn document(&self) -> &Document {
        &self.document
    }

    /// DOM tree dump (`vixen-headless --dump-dom`).
    pub fn dump_dom(&self) -> String {
        self.document.dump()
    }

    /// Visible text extraction (`vixen-headless --extract-text`).
    pub fn text_content(&self) -> String {
        self.document.body_text_content()
    }

    /// Minimal DOM-backed JS expression projection for host-binding smoke
    /// checks while full SpiderMonkey DOM objects are still landing. This is a
    /// deliberately tiny, fail-closed subset: callers get `None` for unsupported
    /// expressions and can fall back to the real JS runtime.
    pub fn evaluate_dom_expression(&self, expr: &str) -> Option<Result<String, String>> {
        let expr = expr.trim();
        match expr {
            "document.title" => return Some(Ok(self.document.title().unwrap_or_default())),
            "document.URL" | "location.href" | "window.location.href" => {
                return Some(Ok(self.url.clone()));
            }
            "document.body.textContent" | "document.body.innerText" => {
                return Some(Ok(self.document.body_text_content()));
            }
            "document.forms.length" => {
                return Some(self.query_selector_all("form").map(|m| m.len().to_string()));
            }
            _ => {}
        }

        if let Some(result) = self.document_member_expr(expr) {
            return Some(result);
        }

        if let Some(selector) = query_selector_all_length_expr(expr) {
            return Some(
                self.query_selector_all(&selector)
                    .map(|m| m.len().to_string()),
            );
        }

        if let Some(result) = self.query_selector_dom_member_expr(expr) {
            return Some(result);
        }

        if let Some(result) = self.get_element_by_id_dom_member_expr(expr) {
            return Some(result);
        }

        None
    }

    fn document_member_expr(&self, expr: &str) -> Option<Result<String, String>> {
        if let Some(member) = expr.strip_prefix("document.documentElement") {
            return Some(self.first_selector_member_value(":root", member));
        }
        if let Some(member) = expr.strip_prefix("document.head") {
            return Some(self.first_selector_member_value("head", member));
        }
        if let Some(member) = expr.strip_prefix("document.body") {
            return Some(self.first_selector_member_value("body", member));
        }
        if let Some(name) = collection_length_arg(expr, "document.getElementsByTagName(") {
            if !is_simple_ident(&name) && name != "*" {
                return Some(Err(
                    "getElementsByTagName smoke only accepts simple tags".into()
                ));
            }
            return Some(self.query_selector_all(&name).map(|m| m.len().to_string()));
        }
        if let Some(name) = collection_length_arg(expr, "document.getElementsByClassName(") {
            if !is_simple_ident(&name) {
                return Some(Err(
                    "getElementsByClassName smoke only accepts simple classes".into(),
                ));
            }
            let selector = format!(".{name}");
            return Some(
                self.query_selector_all(&selector)
                    .map(|m| m.len().to_string()),
            );
        }
        None
    }

    fn query_selector_dom_member_expr(&self, expr: &str) -> Option<Result<String, String>> {
        let rest = expr.strip_prefix("document.querySelector(")?;
        let (selector, member) = parse_single_string_arg_call(rest)?;
        if member == " === null" {
            return Some(
                self.query_selector_all(&selector)
                    .map(|m| m.is_empty().to_string()),
            );
        }
        if member == " !== null" {
            return Some(
                self.query_selector_all(&selector)
                    .map(|m| (!m.is_empty()).to_string()),
            );
        }
        Some(self.first_selector_member_value(&selector, member))
    }

    fn get_element_by_id_dom_member_expr(&self, expr: &str) -> Option<Result<String, String>> {
        let rest = expr.strip_prefix("document.getElementById(")?;
        let (id, member) = parse_single_string_arg_call(rest)?;
        if !is_simple_id_selector(&id) {
            return Some(Err("getElementById smoke only accepts simple ids".into()));
        }
        let selector = format!("#{id}");
        if member == " === null" {
            return Some(
                self.query_selector_all(&selector)
                    .map(|m| m.is_empty().to_string()),
            );
        }
        if member == " !== null" {
            return Some(
                self.query_selector_all(&selector)
                    .map(|m| (!m.is_empty()).to_string()),
            );
        }
        Some(self.first_selector_member_value(&selector, member))
    }

    fn first_selector_member_value(&self, selector: &str, member: &str) -> Result<String, String> {
        let Some(info) = self.query_selector_all(selector)?.into_iter().next() else {
            return Err("DOM eval selector matched nothing".into());
        };
        self.element_member_value(info, member)
    }

    fn first_selector_method_value(
        &self,
        info: &ElementInfo,
        member: &str,
    ) -> Result<String, String> {
        if let Some(arg) = method_string_arg(member, ".getAttribute(") {
            return Ok(info
                .attributes
                .iter()
                .find(|(name, _)| name == &arg)
                .map(|(_, value)| value.clone())
                .unwrap_or_else(|| "null".to_owned()));
        }
        if let Some(arg) = method_string_arg(member, ".hasAttribute(") {
            return Ok(info
                .attributes
                .iter()
                .any(|(name, _)| name == &arg)
                .to_string());
        }
        if let Some(arg) = method_string_arg(member, ".matches(") {
            let selector = Selector::parse(&arg).map_err(|e| e.to_string())?;
            return Ok(self
                .document
                .matches_selector(info.node_id, &selector)
                .to_string());
        }
        if let Some(result) = self.related_element_member_value(info, member) {
            return result;
        }
        Err("unsupported DOM eval member expression".into())
    }

    fn related_element_member_value(
        &self,
        info: &ElementInfo,
        member: &str,
    ) -> Option<Result<String, String>> {
        let (relation, rest) = if let Some(rest) = member.strip_prefix(".parentElement") {
            (ElementRelation::Parent, rest)
        } else if let Some(rest) = member.strip_prefix(".firstElementChild") {
            (ElementRelation::FirstChild, rest)
        } else if let Some(rest) = member.strip_prefix(".lastElementChild") {
            (ElementRelation::LastChild, rest)
        } else if let Some(rest) = member.strip_prefix(".previousElementSibling") {
            (ElementRelation::PreviousSibling, rest)
        } else if let Some(rest) = member.strip_prefix(".nextElementSibling") {
            (ElementRelation::NextSibling, rest)
        } else {
            return None;
        };

        let related = self
            .document
            .related_element_by_node_id(info.node_id, relation)
            .map(|element| element.into_element_info());
        match (related, rest) {
            (None, " === null") => Some(Ok("true".into())),
            (Some(_), " === null") => Some(Ok("false".into())),
            (None, " !== null") => Some(Ok("false".into())),
            (Some(_), " !== null") => Some(Ok("true".into())),
            (None, _) => Some(Err("DOM eval relation matched nothing".into())),
            (Some(related), _) => Some(self.element_member_value(related, rest)),
        }
    }

    fn element_member_value(&self, info: ElementInfo, member: &str) -> Result<String, String> {
        match member {
            ".id" => Ok(info.id.unwrap_or_default()),
            ".className" => Ok(info.classes.join(" ")),
            ".tagName" => Ok(info.tag.to_ascii_uppercase()),
            ".textContent" | ".innerText" => Ok(self
                .document
                .element_text_content(info.node_id)
                .unwrap_or(info.text)),
            ".childElementCount" | ".children.length" => Ok(self
                .document
                .element_child_count(info.node_id)
                .unwrap_or_default()
                .to_string()),
            ".value" => Ok(element_attr(&info, "value").unwrap_or_default()),
            ".name" => Ok(element_attr(&info, "name").unwrap_or_default()),
            ".type" => Ok(element_attr(&info, "type").unwrap_or_else(|| default_type(&info))),
            ".placeholder" => Ok(element_attr(&info, "placeholder").unwrap_or_default()),
            ".htmlFor" => Ok(element_attr(&info, "for").unwrap_or_default()),
            ".checked" => Ok(element_has_attr(&info, "checked").to_string()),
            ".disabled" => Ok(element_has_attr(&info, "disabled").to_string()),
            ".required" => Ok(element_has_attr(&info, "required").to_string()),
            ".readOnly" => Ok(element_has_attr(&info, "readonly").to_string()),
            ".selected" => Ok(element_has_attr(&info, "selected").to_string()),
            ".multiple" => Ok(element_has_attr(&info, "multiple").to_string()),
            ".method" => Ok(element_attr(&info, "method")
                .unwrap_or_else(|| "get".into())
                .to_ascii_lowercase()),
            ".enctype" => Ok(element_attr(&info, "enctype")
                .unwrap_or_else(|| "application/x-www-form-urlencoded".into())
                .to_ascii_lowercase()),
            ".action" => Ok(element_attr(&info, "action").unwrap_or_default()),
            _ => self.first_selector_method_value(&info, member),
        }
    }

    /// First Vixen-owned layout tree slice: styled DOM projected into an
    /// arena-backed tree with stable layout-node ids.
    pub fn layout_tree(&self, viewport: (u32, u32)) -> LayoutTree {
        build_layout_tree(&self.document, viewport, |node_id| {
            self.computed_style_for_viewport(node_id, viewport)
        })
    }

    /// Deterministic layout-tree dump (`vixen-headless --dump-layout-tree`).
    pub fn dump_layout_tree(&self, viewport: (u32, u32)) -> String {
        dump_layout_tree(&self.layout_tree(viewport))
    }

    /// First executable Phase 4 line slice: the layout tree's visible text
    /// wrapped into stable line boxes for a viewport.
    pub fn layout_lines(&self, viewport: (u32, u32)) -> Vec<LineBox> {
        line_boxes_from_tree(&self.layout_tree(viewport))
    }

    /// Text dump for `vixen-headless --dump-lines`.
    pub fn dump_lines(&self, viewport: (u32, u32)) -> String {
        dump_line_boxes(&self.layout_lines(viewport), viewport)
    }

    /// First executable Phase 5 paint slice: convert the Page-backed layout tree
    /// into the single invariant-enforced display-list command stream.
    pub fn display_list(&self, viewport: (u32, u32)) -> Vec<PaintCommand> {
        let viewport_rect = Rect::new(0.0, 0.0, viewport.0 as f32, viewport.1 as f32);
        let tree = self.layout_tree(viewport);
        let mut builder = DisplayListBuilder::new();
        builder.push(viewport_background_item(viewport_rect));
        for node in &tree.nodes {
            if node.id == tree.root {
                continue;
            }
            if let Some(item) = layout_node_draw_item(&tree, node, viewport_rect) {
                builder.push(item);
            }
        }
        builder.build()
    }

    /// Text dump for `vixen-headless --dump-display-list`.
    pub fn dump_display_list(&self, viewport: (u32, u32)) -> String {
        dump_paint_commands(&self.display_list(viewport), viewport)
    }

    /// Aggregate display-list stats for `vixen-headless --paint-stats`.
    pub fn paint_stats(&self, viewport: (u32, u32)) -> PaintStats {
        PaintStats::from_commands(&self.display_list(viewport))
    }

    /// Text dump for `vixen-headless --paint-stats`.
    pub fn dump_paint_stats(&self, viewport: (u32, u32)) -> String {
        dump_paint_stats(&self.display_list(viewport), viewport)
    }

    /// Coarse page snapshot at a viewport.
    pub fn snapshot(&self, viewport: (u32, u32)) -> PageSnapshot {
        PageSnapshot {
            url: self.url.clone(),
            title: self.document.title(),
            viewport,
            text_content: self.document.body_text_content(),
            element_count: self.document.element_count(),
        }
    }

    /// Query selector facade over the current DOM-backed selector surface.
    pub fn query_selector_all(&self, selector: &str) -> Result<Vec<ElementInfo>, String> {
        self.query_selector_all_in_viewport(selector, (800, 600))
    }

    /// Query selector facade with layout metadata resolved at `viewport`.
    pub fn query_selector_all_in_viewport(
        &self,
        selector: &str,
        viewport: (u32, u32),
    ) -> Result<Vec<ElementInfo>, String> {
        let parsed = Selector::parse(selector).map_err(|e| e.to_string())?;
        let layout_tree = self.layout_tree(viewport);
        Ok(self
            .document
            .query_all(&parsed)
            .into_iter()
            .map(|m| ElementInfo {
                bbox: layout_bbox_for_node(&layout_tree, m.node_id),
                node_id: m.node_id,
                tag: m.tag,
                id: m.id,
                classes: m.classes,
                attributes: m.attributes,
                text: m.text,
            })
            .collect())
    }

    /// Computed style facade for the current Phase 3 vertical slice: author
    /// `<style>` rules matched via Stylo selectors, plus inline declarations,
    /// cascaded by importance, specificity, and source order. Missing elements
    /// return an empty vector rather than fabricating UA/default values.
    pub fn computed_style(&self, node_id: usize) -> Vec<(String, String)> {
        self.author_stylesheet
            .computed_style(&self.document, node_id)
    }

    /// Computed style for layout/paint paths whose cascade depends on the
    /// requested viewport (`@media`). The public WPT/headless computed-style
    /// query keeps using the default 800×600 viewport for stable assertions.
    pub fn computed_style_for_viewport(
        &self,
        node_id: usize,
        viewport: (u32, u32),
    ) -> Vec<(String, String)> {
        self.author_stylesheet
            .computed_style_for_viewport(&self.document, node_id, viewport)
    }

    /// Diagnostics accumulated by pipeline stages.
    pub fn diagnostics(&self) -> Vec<EngineDiagnostic> {
        self.diagnostics.clone()
    }
}

fn viewport_background_item(rect: Rect) -> DrawItem {
    DrawItem {
        order: 0,
        z_index: 0,
        visibility: crate::display_list::Visibility::Visible,
        opacity: 1.0,
        clip: None,
        is_viewport_background: true,
        border_box: rect,
        padding_box: rect,
        content_box: rect,
        background_clip: BackgroundBox::BorderBox,
        background_origin: BackgroundBox::BorderBox,
        background_attachment: BackgroundAttachment::Scroll,
        background: Some(Color::WHITE),
        text: None,
    }
}

fn layout_node_draw_item(tree: &LayoutTree, node: &LayoutNode, viewport: Rect) -> Option<DrawItem> {
    let clip = clip_for_node(tree, node, viewport)?;
    match node.kind {
        LayoutNodeKind::Viewport => None,
        LayoutNodeKind::Block => node
            .style
            .background_color
            .map(|color| background_item(node, color, clip)),
        LayoutNodeKind::Inline => None,
        LayoutNodeKind::Text => node
            .text
            .as_ref()
            .filter(|text| !text.is_empty())
            .map(|text| text_item(node, text, inherited_text_color(tree, node), clip)),
    }
}

fn clip_for_node(tree: &LayoutTree, node: &LayoutNode, viewport: Rect) -> Option<Rect> {
    let mut clip = viewport;
    let mut parent = node.parent;
    while let Some(id) = parent {
        let ancestor = tree.node(id);
        if ancestor.style.overflow.clips_contents() {
            clip = clip.intersect(ancestor.boxes.padding)?;
        }
        parent = ancestor.parent;
    }
    Some(clip)
}

fn background_item(node: &LayoutNode, color: Color, clip: Rect) -> DrawItem {
    DrawItem {
        order: node.id.index() as u32 + 1,
        z_index: 0,
        visibility: crate::display_list::Visibility::Visible,
        opacity: 1.0,
        clip: Some(clip),
        is_viewport_background: false,
        border_box: node.boxes.border,
        padding_box: node.boxes.padding,
        content_box: node.boxes.content,
        background_clip: BackgroundBox::BorderBox,
        background_origin: BackgroundBox::PaddingBox,
        background_attachment: BackgroundAttachment::Scroll,
        background: Some(color),
        text: None,
    }
}

fn text_item(node: &LayoutNode, text: &str, color: Color, clip: Rect) -> DrawItem {
    DrawItem {
        order: node.id.index() as u32 + 1,
        z_index: 0,
        visibility: crate::display_list::Visibility::Visible,
        opacity: 1.0,
        clip: Some(clip),
        is_viewport_background: false,
        border_box: node.boxes.border,
        padding_box: node.boxes.padding,
        content_box: node.boxes.content,
        background_clip: BackgroundBox::BorderBox,
        background_origin: BackgroundBox::ContentBox,
        background_attachment: BackgroundAttachment::Scroll,
        background: None,
        text: Some(TextRun {
            color,
            text: text.to_owned(),
        }),
    }
}

fn inherited_text_color(tree: &LayoutTree, node: &LayoutNode) -> Color {
    let mut parent = node.parent;
    while let Some(id) = parent {
        let parent_node = tree.node(id);
        if parent_node.dom_node_id.is_some() {
            return parent_node.style.color;
        }
        parent = parent_node.parent;
    }
    Color::BLACK
}

fn layout_bbox_for_node(tree: &LayoutTree, node_id: usize) -> Option<(f64, f64, f64, f64)> {
    tree.nodes
        .iter()
        .find(|node| node.dom_node_id == Some(node_id))
        .map(|node| {
            (
                node.rect.x as f64,
                node.rect.y as f64,
                node.rect.w as f64,
                node.rect.h as f64,
            )
        })
}

fn query_selector_all_length_expr(expr: &str) -> Option<String> {
    let inner = expr
        .strip_prefix("document.querySelectorAll(")?
        .strip_suffix(").length")?;
    js_string_literal(inner)
}

fn collection_length_arg(expr: &str, prefix: &str) -> Option<String> {
    let inner = expr.strip_prefix(prefix)?.strip_suffix(").length")?;
    js_string_literal(inner)
}

fn element_attr(info: &ElementInfo, name: &str) -> Option<String> {
    info.attributes
        .iter()
        .find(|(attr_name, _)| attr_name == name)
        .map(|(_, value)| value.clone())
}

fn element_has_attr(info: &ElementInfo, name: &str) -> bool {
    info.attributes
        .iter()
        .any(|(attr_name, _)| attr_name == name)
}

fn default_type(info: &ElementInfo) -> String {
    match info.tag.as_str() {
        "input" => "text".into(),
        "button" => "submit".into(),
        _ => String::new(),
    }
}

fn parse_single_string_arg_call(input: &str) -> Option<(String, &str)> {
    let bytes = input.as_bytes();
    let quote = *bytes.first()?;
    if !matches!(quote, b'\'' | b'\"') {
        return None;
    }
    let mut end_quote = None;
    for (idx, byte) in bytes.iter().copied().enumerate().skip(1) {
        if byte == b'\\' {
            return None;
        }
        if byte == quote {
            end_quote = Some(idx);
            break;
        }
    }
    let end_quote = end_quote?;
    if bytes.get(end_quote + 1).copied() != Some(b')') {
        return None;
    }
    let arg = &input[1..end_quote];
    let rest = &input[end_quote + 2..];
    Some((arg.to_owned(), rest))
}

fn method_string_arg(member: &str, prefix: &str) -> Option<String> {
    let inner = member.strip_prefix(prefix)?.strip_suffix(')')?;
    js_string_literal(inner)
}

fn is_simple_id_selector(id: &str) -> bool {
    is_simple_ident(id)
}

fn is_simple_ident(ident: &str) -> bool {
    let mut chars = ident.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
}

fn js_string_literal(input: &str) -> Option<String> {
    let input = input.trim();
    let bytes = input.as_bytes();
    let quote = *bytes.first()?;
    if bytes.len() < 2 || !matches!(quote, b'\'' | b'\"') || bytes.last().copied() != Some(quote) {
        return None;
    }
    let inner = &input[1..input.len() - 1];
    // This compatibility slice only accepts plain quoted selectors. Escaped JS
    // strings will be handled by the real DOM-bound JS runtime.
    if inner.contains('\\') {
        return None;
    }
    Some(inner.to_owned())
}

impl EngineInspector for Page {
    fn inspect_element_at(&self, x: f64, y: f64) -> Option<ElementInfo> {
        self.element_at((800, 600), x, y)
    }

    fn capture_snapshot(&self, vw: u32, vh: u32) -> PageSnapshot {
        self.snapshot((vw, vh))
    }

    fn computed_style_for_element(&self, node_id: usize) -> Vec<(String, String)> {
        self.computed_style(node_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_snapshot_carries_url_title_text_and_count() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<html><head><title>T</title></head><body><p>Hi</p></body></html>",
        )
        .unwrap();
        let snap = page.snapshot((1024, 768));
        assert_eq!(snap.url, "file:///fixture.html");
        assert_eq!(snap.title.as_deref(), Some("T"));
        assert_eq!(snap.viewport, (1024, 768));
        assert!(snap.text_content.contains("Hi"));
        assert!(!snap.text_content.contains('T'));
        assert_eq!(snap.element_count, 5);
    }

    #[test]
    fn page_text_content_excludes_head_and_title() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<html><head><title>Hidden</title></head><body><p>Visible</p></body></html>",
        )
        .unwrap();

        assert_eq!(page.text_content(), "Visible");
        assert_eq!(page.snapshot((800, 600)).text_content, "Visible");
    }

    #[test]
    fn page_queries_selectors_through_single_facade() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<html><body><p id='a' class='x'>one</p><p>two</p></body></html>",
        )
        .unwrap();
        let matches = page.query_selector_all("p.x").unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id.as_deref(), Some("a"));
        assert_eq!(matches[0].tag, "p");
        assert!(page.query_selector_all("p >").is_err());
    }

    #[test]
    fn selector_bboxes_use_requested_viewport() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<style>body { margin: 0; } #wide { height: 10px; }</style><div id='wide'>x</div>",
        )
        .unwrap();

        let narrow = page
            .query_selector_all_in_viewport("#wide", (200, 100))
            .unwrap();
        let wide = page
            .query_selector_all_in_viewport("#wide", (400, 100))
            .unwrap();

        assert_eq!(narrow[0].bbox.unwrap().2, 200.0);
        assert_eq!(wide[0].bbox.unwrap().2, 400.0);
    }

    #[test]
    fn page_evaluates_dom_expression_subset() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<html><head><title>T</title></head><body><p class='x'>one</p><p>two</p></body></html>",
        )
        .unwrap();
        assert_eq!(
            page.evaluate_dom_expression("document.title"),
            Some(Ok("T".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelectorAll('p').length"),
            Some(Ok("2".into()))
        );
        assert!(
            page.evaluate_dom_expression("document.querySelectorAll('p >').length")
                .is_some_and(|result| result.is_err())
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelectorAll(').length"),
            None
        );
        assert_eq!(page.evaluate_dom_expression("1+2"), None);
    }

    #[test]
    fn page_evaluates_dom_query_member_subset() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<main id='root'><p id='lead' class='note primary' data-role='intro'>Lead <b id='bold'>Beta</b></p><section id='empty'></section><form id='form'></form></main>",
        )
        .unwrap();

        assert_eq!(
            page.evaluate_dom_expression("document.documentElement.tagName"),
            Some(Ok("HTML".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.body.tagName"),
            Some(Ok("BODY".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.forms.length"),
            Some(Ok("1".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.getElementsByTagName('p').length"),
            Some(Ok("1".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.getElementsByClassName('note').length"),
            Some(Ok("1".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#lead').id"),
            Some(Ok("lead".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#lead').className"),
            Some(Ok("note primary".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#lead').tagName"),
            Some(Ok("P".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#lead').textContent"),
            Some(Ok("Lead Beta".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#root').children.length"),
            Some(Ok("3".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#root').firstElementChild.id"),
            Some(Ok("lead".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#root').lastElementChild.id"),
            Some(Ok("form".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#bold').parentElement.id"),
            Some(Ok("lead".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "document.querySelector('#empty').previousElementSibling.id"
            ),
            Some(Ok("lead".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#empty').nextElementSibling.id"),
            Some(Ok("form".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#form').method"),
            Some(Ok("get".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "document.querySelector('#lead').getAttribute('data-role')"
            ),
            Some(Ok("intro".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#lead').hasAttribute('hidden')"),
            Some(Ok("false".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#lead').matches('p.note')"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('.missing') === null"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.getElementById('empty').tagName"),
            Some(Ok("SECTION".into()))
        );
    }

    #[test]
    fn page_evaluates_form_reflected_property_subset() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<form id='f' method='POST' enctype='multipart/form-data' action='/submit'>\
                <label id='l' for='email'>Email</label>\
                <input id='email' name='email' required placeholder='you@example.test' value='a@example.test'>\
                <input id='agree' type='checkbox' checked disabled>\
                <textarea id='bio' readonly>bio</textarea>\
                <select id='roles' multiple><option id='admin' selected>Admin</option></select>\
                <button id='save'>Save</button>\
             </form>",
        )
        .unwrap();

        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#f').method"),
            Some(Ok("post".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#f').enctype"),
            Some(Ok("multipart/form-data".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#f').action"),
            Some(Ok("/submit".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#l').htmlFor"),
            Some(Ok("email".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#email').type"),
            Some(Ok("text".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#email').name"),
            Some(Ok("email".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#email').value"),
            Some(Ok("a@example.test".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#email').required"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#agree').checked"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#agree').disabled"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#bio').readOnly"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#roles').multiple"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#admin').selected"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#save').type"),
            Some(Ok("submit".into()))
        );
    }

    #[test]
    fn computed_style_projects_inline_declarations() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<p id='x' style='Color: red; display: grid; color: blue !important; color: green; --Token: A:B'>x</p>",
        )
        .unwrap();
        let node_id = page.query_selector_all("#x").unwrap()[0].node_id;
        let styles = page.computed_style(node_id);
        assert_eq!(
            styles,
            vec![
                ("color".to_owned(), "blue".to_owned()),
                ("display".to_owned(), "grid".to_owned()),
                ("--Token".to_owned(), "A:B".to_owned()),
            ]
        );
    }

    #[test]
    fn computed_style_ignores_nested_delimiters() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<p id='x' style=\"background-image: url('a;b:c'); content: 'x;y';\">x</p>",
        )
        .unwrap();
        let node_id = page.query_selector_all("#x").unwrap()[0].node_id;
        let styles = page.computed_style(node_id);
        assert_eq!(
            styles,
            vec![
                ("background-image".to_owned(), "url('a;b:c')".to_owned()),
                ("content".to_owned(), "'x;y'".to_owned()),
            ]
        );
    }

    #[test]
    fn computed_style_fails_closed_for_missing_inline_style() {
        let page = Page::from_html("file:///fixture.html", "<p style='color:red'>x</p>").unwrap();
        assert!(page.computed_style(usize::MAX).is_empty());
        assert!(
            page.computed_style(page.query_selector_all("html").unwrap()[0].node_id)
                .is_empty()
        );
    }

    #[test]
    fn computed_style_cascades_author_stylesheet_rules() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<style>p { color: red; display: block } #x { color: blue }</style><p id='x'>x</p>",
        )
        .unwrap();
        let node_id = page.query_selector_all("#x").unwrap()[0].node_id;
        let styles = page.computed_style(node_id);
        assert_eq!(style_value(&styles, "display"), Some("block"));
        assert_eq!(style_value(&styles, "color"), Some("blue"));
    }

    #[test]
    fn computed_style_cascades_flex_longhands() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<style>#grow { flex-grow: 2; flex-basis: 0px; }</style><div id='grow'>x</div>",
        )
        .unwrap();
        let node_id = page.query_selector_all("#grow").unwrap()[0].node_id;
        let styles = page.computed_style(node_id);
        assert_eq!(style_value(&styles, "flex-grow"), Some("2"));
        assert_eq!(style_value(&styles, "flex-basis"), Some("0px"));
    }

    #[test]
    fn computed_style_author_important_beats_inline_normal() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<style>#x { color: red !important }</style><p id='x' style='color: blue'>x</p>",
        )
        .unwrap();
        let node_id = page.query_selector_all("#x").unwrap()[0].node_id;
        assert_eq!(
            style_value(&page.computed_style(node_id), "color"),
            Some("red")
        );
    }

    #[test]
    fn dump_lines_wraps_body_text_and_excludes_title() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<html><head><title>Hidden title</title></head><body><p>one two three four</p></body></html>",
        )
        .unwrap();
        let dump = page.dump_lines((56, 200));
        assert!(dump.contains("# line-boxes viewport=56x200 count=4"));
        assert!(dump.contains("line 1:"));
        assert!(dump.contains("text=\"one\""));
        assert!(!dump.contains("Hidden title"));
    }

    #[test]
    fn dump_layout_tree_runs_behind_page_facade() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<style>#drop { display: none }</style><main id='root'><p>Keep</p><p id='drop'>Drop</p></main>",
        )
        .unwrap();
        let dump = page.dump_layout_tree((120, 200));
        assert!(dump.contains("# layout-tree viewport=120x200"));
        assert!(dump.contains("tag=main id=root"));
        assert!(dump.contains("text=\"Keep\""));
        assert!(!dump.contains("Drop"));
        assert!(!dump.contains("id=drop"));
    }

    #[test]
    fn layout_tree_consumes_author_box_model_styles() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<style>#box { width: 40px; height: 20px; padding: 5px; border-width: 2px; }</style><div id='box'>x</div>",
        )
        .unwrap();
        let tree = page.layout_tree((120, 200));
        let node = tree
            .nodes
            .iter()
            .find(|node| node.html_id.as_deref() == Some("box"))
            .unwrap();
        assert_eq!(node.rect, Rect::new(8.0, 8.0, 54.0, 34.0));
        assert_eq!(node.boxes.content, Rect::new(15.0, 15.0, 40.0, 20.0));
    }

    #[test]
    fn dump_display_list_builds_background_and_text_commands() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<html><head><title>Hidden title</title></head><body><p>one two three four</p></body></html>",
        )
        .unwrap();
        let dump = page.dump_display_list((56, 200));
        assert!(dump.contains("# display-list viewport=56x200 count=2"));
        assert!(dump.contains("cmd 0: background x=0.0 y=0.0 w=56.0 h=200.0"));
        assert!(dump.contains("cmd 1: text"));
        assert!(dump.contains("text=\"one two three four\""));
        assert!(!dump.contains("Hidden title"));
    }

    #[test]
    fn display_list_uses_layout_tree_boxes_and_author_colours() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<style>#box { width: 40px; height: 20px; padding: 5px; border-width: 2px; background-color: #3366ff; color: white; }</style><div id='box'>Hi</div>",
        )
        .unwrap();
        let dump = page.dump_display_list((120, 200));
        assert!(dump.contains("cmd 1: background x=8.0 y=8.0 w=54.0 h=34.0 color=#3366ffff"));
        assert!(dump.contains("cmd 2: text x=15.0 y=15.0"));
        assert!(dump.contains("color=#ffffffff text=\"Hi\""));
    }

    #[test]
    fn display_list_inherits_text_colour_and_currentcolor_background() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<style>body { margin: 0; } #parent { color: #123456; } #child { background-color: currentcolor; }</style><div id='parent'><span>Nested</span><div id='child'>Box</div></div>",
        )
        .unwrap();
        let dump = page.dump_display_list((120, 200));
        assert!(dump.contains("color=#123456ff text=\"Nested\""));
        assert!(
            dump.lines()
                .any(|line| line.contains("background") && line.contains("color=#123456ff")),
            "expected an inherited currentcolor background in:\n{dump}"
        );
        assert!(dump.contains("color=#123456ff text=\"Box\""));
    }

    #[test]
    fn display_list_clips_descendants_to_overflow_scrollport() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<style>body { margin: 0; } #clip { width: 40px; height: 10px; overflow: hidden; }</style><div id='clip'>Overflowing text</div>",
        )
        .unwrap();
        let dump = page.dump_display_list((120, 80));
        assert!(
            dump.contains("cmd 1: text x=0.0 y=0.0 w=40.0 h=10.0"),
            "{dump}"
        );
        assert!(dump.contains("text=\"Overflowing text\""));
    }

    #[test]
    fn paint_stats_summarise_display_list() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<html><head><title>Hidden title</title></head><body><p>one two</p></body></html>",
        )
        .unwrap();
        let stats = page.paint_stats((56, 200));
        assert_eq!(stats.backgrounds, 1);
        assert_eq!(stats.text_runs, 1);
        assert_eq!(stats.commands, 2);
        let dump = page.dump_paint_stats((56, 200));
        assert!(dump.contains("# paint-stats viewport=56x200 commands=2"));
        assert!(dump.contains("text-runs=1"));
    }

    fn style_value<'a>(styles: &'a [(String, String)], property: &str) -> Option<&'a str> {
        styles
            .iter()
            .find(|(name, _)| name == property)
            .map(|(_, value)| value.as_str())
    }
}
