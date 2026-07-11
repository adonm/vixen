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

use vixen_api::{
    ACCESSIBILITY_MAX_NODES, ACCESSIBILITY_MAX_STRING_BYTES, AccessibilityNode, AccessibilityRect,
    AccessibilitySnapshot, BrowsingContextId, DocumentId, ElementInfo, EngineDiagnostic,
    EngineInspector, PageSnapshot,
};
use vixen_net::csp::ContentSecurityPolicy;

use crate::display_list::{
    BackgroundAttachment, BackgroundBox, Color, DisplayListBuilder, DrawItem, PaintCommand,
    PaintStats, Rect, TextRun, dump_paint_commands, dump_paint_stats,
};
use crate::doc::{Document, DocumentParser, ParseError};
use crate::history::{HistoryEntry, SessionHistory};
use crate::layout_tree::{
    LayoutFragment, LayoutFragmentKind, LayoutTree, build_layout_tree, dump_layout_tree,
    layout_fragments_from_tree, line_boxes_from_tree,
};
use crate::line_layout::{LineBox, dump_line_boxes};
use crate::style_cascade::AuthorStylesheet;
use crate::style_dom::{AccessibilityElement, Selector};
use crate::whatwg_url::{parse as parse_url, parse_with_base as parse_url_with_base};

mod interaction;
pub use interaction::{FormSubmissionSnapshot, PageSelection};

/// A loaded page at the current vertical integration boundary.
pub struct Page {
    url: String,
    document: Document,
    history: SessionHistory,
    csp: ContentSecurityPolicy,
    author_stylesheet: AuthorStylesheet,
    diagnostics: Vec<EngineDiagnostic>,
    focused_element_node_id: Option<usize>,
    accessibility_mutation_epoch: u64,
    selection: Option<PageSelection>,
}

/// Incremental page construction retained on BrowserCore's owner thread.
pub(crate) struct PageParser {
    url: String,
    html: String,
    offset: usize,
    document: Option<DocumentParser>,
    csp: ContentSecurityPolicy,
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
        Self::from_document(
            url.into(),
            Document::parse(html)?,
            ContentSecurityPolicy::new(),
        )
    }

    /// Build a page from already-loaded HTML plus response headers. Enforcing
    /// `Content-Security-Policy` headers are captured here so inline script
    /// execution starts with network-delivered policy before document meta CSP.
    pub fn from_html_with_headers<'a, I>(
        url: impl Into<String>,
        html: &str,
        headers: I,
    ) -> Result<Self, PageError>
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        let csp = ContentSecurityPolicy::from_headers(headers);
        Self::from_document(url.into(), Document::parse(html)?, csp)
    }

    fn from_document(
        url: String,
        document: Document,
        csp: ContentSecurityPolicy,
    ) -> Result<Self, PageError> {
        let author_stylesheet = AuthorStylesheet::from_blocks(&document.style_blocks());
        let history = initial_session_history(&url);
        Ok(Self {
            url,
            document,
            history,
            csp,
            author_stylesheet,
            diagnostics: Vec::new(),
            focused_element_node_id: None,
            accessibility_mutation_epoch: 1,
            selection: None,
        })
    }

    /// The loaded URL, as authored by the caller.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The current session-history model associated with this page snapshot.
    pub fn session_history(&self) -> &SessionHistory {
        &self.history
    }

    /// Replace the session-history model after a navigation/history host hook.
    /// The loaded URL is kept in sync with the current history entry.
    pub fn set_session_history(&mut self, history: SessionHistory) {
        if let Some(url) = history.url() {
            self.url = url.to_owned();
        }
        self.history = history;
    }

    /// Resolve `input` against the document base URL as an absolute URL string.
    pub fn resolve_url(&self, input: &str) -> Option<String> {
        resolve_url_string(input, &self.document_base_uri())
    }

    /// History state serialised by the current JS host hook, if any.
    pub fn history_state_json(&self) -> Option<String> {
        self.history
            .state()
            .map(|state| String::from_utf8_lossy(state).into_owned())
    }

    /// The page-owned focused element, if focus has moved away from the
    /// document's default body element in the current browsing session.
    pub fn focused_element_node_id(&self) -> Option<usize> {
        self.focused_element_node_id
    }

    /// Persist the focused element across runtime realm replacement.
    pub fn set_focused_element_node_id(&mut self, node_id: Option<usize>) {
        if self.focused_element_node_id == node_id {
            return;
        }
        self.focused_element_node_id = node_id;
        self.bump_accessibility_mutation_epoch();
    }

    /// The page-owned single-range selection projection.
    pub fn selection(&self) -> Option<PageSelection> {
        self.selection
    }

    /// Persist document/element selection boundaries across runtime realms.
    pub fn set_selection(&mut self, selection: Option<PageSelection>) {
        self.selection = selection;
    }

    /// The parsed document. Kept available for narrow integration seams while
    /// existing code migrates to the facade methods.
    pub fn document(&self) -> &Document {
        &self.document
    }

    /// Apply the first runtime-backed DOM mutation slice: `Element.textContent`
    /// writes replace the element's child subtree in the authoritative Page DOM,
    /// so later layout/paint/headless/CDP reads see the changed document.
    pub fn set_element_text_content(&mut self, node_id: usize, value: &str) -> Result<(), String> {
        self.document.set_element_text_content(node_id, value)?;
        self.focused_element_node_id = None;
        self.bump_accessibility_mutation_epoch();
        self.refresh_author_stylesheet();
        Ok(())
    }

    /// Set or replace an element attribute in the authoritative Page DOM.
    pub fn set_element_attribute(
        &mut self,
        node_id: usize,
        name: &str,
        value: &str,
    ) -> Result<(), String> {
        self.document.set_element_attribute(node_id, name, value)?;
        self.bump_accessibility_mutation_epoch();
        self.refresh_author_stylesheet();
        Ok(())
    }

    /// Remove an element attribute in the authoritative Page DOM.
    pub fn remove_element_attribute(&mut self, node_id: usize, name: &str) -> Result<(), String> {
        self.document.remove_element_attribute(node_id, name)?;
        self.bump_accessibility_mutation_epoch();
        self.refresh_author_stylesheet();
        Ok(())
    }

    /// Set the document title from a runtime-produced `document.title` or
    /// `document.write()` mutation.
    pub fn set_title(&mut self, value: &str) -> Result<(), String> {
        self.document.set_title(value)?;
        self.focused_element_node_id = None;
        self.bump_accessibility_mutation_epoch();
        Ok(())
    }

    /// Replace an element's child subtree from a runtime-produced HTML fragment.
    pub fn set_element_inner_html(&mut self, node_id: usize, html: &str) -> Result<(), String> {
        self.document.set_element_inner_html(node_id, html)?;
        self.focused_element_node_id = None;
        self.bump_accessibility_mutation_epoch();
        self.refresh_author_stylesheet();
        Ok(())
    }

    /// Commit a live `<input>`/`<textarea>` value from the JS realm. The primary
    /// key is the page-realm node id; id/name/tag are fallbacks for structural
    /// mutations that inserted elements before the control in document order.
    pub fn set_form_control_value(
        &mut self,
        node_id: usize,
        element_id: Option<&str>,
        name: Option<&str>,
        tag: &str,
        value: &str,
    ) -> Result<(), String> {
        self.document
            .set_form_control_value(node_id, element_id, name, tag, value)?;
        self.bump_accessibility_mutation_epoch();
        self.refresh_author_stylesheet();
        Ok(())
    }

    fn refresh_author_stylesheet(&mut self) {
        self.author_stylesheet = AuthorStylesheet::from_blocks(&self.document.style_blocks());
    }

    fn bump_accessibility_mutation_epoch(&mut self) {
        self.accessibility_mutation_epoch = self.accessibility_mutation_epoch.saturating_add(1);
    }

    /// Enforcing CSP delivered with the document response headers.
    pub fn csp(&self) -> &ContentSecurityPolicy {
        &self.csp
    }

    /// DOM tree dump (`vixen-headless --dump-dom`).
    pub fn dump_dom(&self) -> String {
        self.document.dump()
    }

    /// Visible text extraction (`vixen-headless --extract-text`).
    pub fn text_content(&self) -> String {
        self.document.body_text_content()
    }

    /// Count non-overlapping visible-text matches for the shell find bar.
    /// Empty queries deliberately return zero so UI callers can clear find state
    /// without special-casing at the trust boundary.
    pub fn find_text_count(&self, query: &str, case_sensitive: bool) -> u32 {
        count_text_matches(&self.text_content(), query, case_sensitive)
    }

    pub(crate) fn document_base_uri(&self) -> String {
        let Some(base) = self
            .query_selector_all("base[href]")
            .ok()
            .and_then(|matches| matches.into_iter().next())
            .and_then(|info| element_attr(&info, "href"))
        else {
            return self.url.clone();
        };
        resolve_url_string(&base, &self.url).unwrap_or(base)
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

    /// Milestone 2 layout/paint seam: positioned fragments projected from the
    /// layout tree. The renderer consumes this surface instead of re-walking
    /// layout nodes directly.
    pub fn layout_fragments(&self, viewport: (u32, u32)) -> Vec<LayoutFragment> {
        layout_fragments_from_tree(&self.layout_tree(viewport))
    }

    /// Text dump for `vixen-headless --dump-lines`.
    pub fn dump_lines(&self, viewport: (u32, u32)) -> String {
        dump_line_boxes(&self.layout_lines(viewport), viewport)
    }

    /// First executable Phase 5 paint slice: convert Page-backed layout
    /// fragments into the single invariant-enforced display-list command stream.
    pub fn display_list(&self, viewport: (u32, u32)) -> Vec<PaintCommand> {
        let viewport_rect = Rect::new(0.0, 0.0, viewport.0 as f32, viewport.1 as f32);
        let mut builder = DisplayListBuilder::new();
        builder.push(viewport_background_item(viewport_rect));
        for fragment in self.layout_fragments(viewport) {
            builder.push(fragment_draw_item(&fragment));
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

    /// Build the authoritative, bounded semantic projection from this Page's
    /// current DOM, focus state, and viewport-specific layout.
    pub fn accessibility_snapshot(
        &self,
        context_id: BrowsingContextId,
        document_id: DocumentId,
        viewport: (u32, u32),
    ) -> AccessibilitySnapshot {
        let layout = self.layout_tree(viewport);
        let bounds = layout
            .nodes
            .iter()
            .filter_map(|node| {
                node.dom_node_id.map(|node_id| {
                    (
                        node_id,
                        AccessibilityRect {
                            x: f64::from(node.rect.x),
                            y: f64::from(node.rect.y),
                            width: f64::from(node.rect.w),
                            height: f64::from(node.rect.h),
                        },
                    )
                })
            })
            .collect::<std::collections::HashMap<_, _>>();
        let (elements, truncated) = self.document.accessibility_elements(
            ACCESSIBILITY_MAX_NODES,
            ACCESSIBILITY_MAX_STRING_BYTES,
            |node_id| bounds.contains_key(&node_id),
        );
        let mut nodes = Vec::with_capacity(elements.len());
        for element in elements {
            let Some(role) = accessibility_role(&element) else {
                continue;
            };
            let disabled =
                element.disabled || aria_bool(element.aria_disabled.as_deref()) == Some(true);
            let focusable = !disabled && accessibility_focusable(&element);
            let checked = if element.aria_checked.is_some() {
                aria_bool(element.aria_checked.as_deref())
            } else {
                matches!(role.as_str(), "checkbox" | "radio").then_some(element.checked)
            };
            let selected = aria_bool(element.aria_selected.as_deref()).unwrap_or(element.selected);
            let expanded = aria_bool(element.aria_expanded.as_deref());
            let value = accessibility_value(&element, &role);
            let actions =
                if !disabled && matches!(role.as_str(), "button" | "link" | "checkbox" | "radio") {
                    vec!["tap".to_owned()]
                } else {
                    Vec::new()
                };
            nodes.push(AccessibilityNode {
                id: element.node_id,
                role,
                label: element.label,
                value,
                bbox: bounds.get(&element.node_id).copied(),
                focused: self.focused_element_node_id == Some(element.node_id),
                disabled,
                checked,
                selected,
                expanded,
                hidden: false,
                focusable,
                actions,
            });
        }
        let mut snapshot = AccessibilitySnapshot {
            context_id,
            document_id,
            source_generation: self.accessibility_mutation_epoch,
            generation: 1,
            viewport,
            nodes,
            truncated,
        };
        snapshot.refresh_generation();
        snapshot
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

impl PageParser {
    pub(crate) fn with_headers<'a, I>(url: String, html: String, headers: I) -> Self
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        Self {
            url,
            html,
            offset: 0,
            document: Some(DocumentParser::new()),
            csp: ContentSecurityPolicy::from_headers(headers),
        }
    }

    /// Process at most `max_bytes` of source and return the completed page.
    pub(crate) fn advance(&mut self, max_bytes: usize) -> Result<Option<Page>, PageError> {
        assert!(max_bytes > 0, "page parser work budget must be non-zero");
        if self.offset < self.html.len() {
            let mut end = self.offset.saturating_add(max_bytes).min(self.html.len());
            while end > self.offset && !self.html.is_char_boundary(end) {
                end -= 1;
            }
            if end == self.offset {
                end = self.html[self.offset..]
                    .char_indices()
                    .nth(1)
                    .map(|(next, _)| self.offset + next)
                    .unwrap_or(self.html.len());
            }
            self.document
                .as_mut()
                .expect("incomplete page parser has a document parser")
                .process(&self.html[self.offset..end]);
            self.offset = end;
            if self.offset < self.html.len() {
                return Ok(None);
            }
        }

        let document = self
            .document
            .take()
            .expect("completed page parser is advanced only once")
            .finish()?;
        Ok(Some(Page::from_document(
            self.url.clone(),
            document,
            self.csp.clone(),
        )?))
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

fn fragment_draw_item(fragment: &LayoutFragment) -> DrawItem {
    match &fragment.kind {
        LayoutFragmentKind::Background { color, boxes } => DrawItem {
            order: fragment.order,
            z_index: 0,
            visibility: crate::display_list::Visibility::Visible,
            opacity: 1.0,
            clip: fragment.clip,
            is_viewport_background: false,
            border_box: boxes.border,
            padding_box: boxes.padding,
            content_box: boxes.content,
            background_clip: BackgroundBox::BorderBox,
            background_origin: BackgroundBox::PaddingBox,
            background_attachment: BackgroundAttachment::Scroll,
            background: Some(*color),
            text: None,
        },
        LayoutFragmentKind::Text { color, text } => DrawItem {
            order: fragment.order,
            z_index: 0,
            visibility: crate::display_list::Visibility::Visible,
            opacity: 1.0,
            clip: fragment.clip,
            is_viewport_background: false,
            border_box: fragment.rect,
            padding_box: fragment.rect,
            content_box: fragment.rect,
            background_clip: BackgroundBox::BorderBox,
            background_origin: BackgroundBox::ContentBox,
            background_attachment: BackgroundAttachment::Scroll,
            background: None,
            text: Some(TextRun {
                color: *color,
                text: text.clone(),
            }),
        },
    }
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

fn accessibility_role(element: &AccessibilityElement) -> Option<String> {
    if let Some(role) = element
        .role
        .as_deref()
        .and_then(|role| role.split_whitespace().next())
    {
        return Some(role.to_ascii_lowercase());
    }
    let role = match element.tag.as_str() {
        "a" if element.href => "link",
        "button" => "button",
        "input" => match element
            .input_type
            .as_deref()
            .unwrap_or("text")
            .to_ascii_lowercase()
            .as_str()
        {
            "hidden" => return None,
            "checkbox" => "checkbox",
            "radio" => "radio",
            "button" | "submit" | "reset" | "image" => "button",
            "range" => "slider",
            "number" => "spinbutton",
            "search" => "searchbox",
            _ => "textbox",
        },
        "select" if element.multiple => "listbox",
        "select" => "combobox",
        "option" => "option",
        "textarea" => "textbox",
        "img" => "image",
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "heading",
        "ul" | "ol" => "list",
        "li" => "listitem",
        "main" => "main",
        "nav" => "navigation",
        "header" => "banner",
        "footer" => "contentinfo",
        "form" => "form",
        _ if element.text.is_empty()
            && element.aria_label.as_deref().is_none_or(str::is_empty)
            && element.tabindex.is_none()
            && !element.contenteditable =>
        {
            return None;
        }
        _ => "generic",
    };
    Some(role.to_owned())
}

fn accessibility_value(element: &AccessibilityElement, role: &str) -> Option<String> {
    if !matches!(
        role,
        "textbox" | "searchbox" | "combobox" | "listbox" | "slider" | "spinbutton"
    ) {
        return None;
    }
    element.value.clone().or_else(|| {
        matches!(element.tag.as_str(), "textarea" | "select")
            .then(|| element.text.clone())
            .filter(|value| !value.is_empty())
    })
}

fn accessibility_focusable(element: &AccessibilityElement) -> bool {
    let tabindex_focusable = element
        .tabindex
        .as_deref()
        .and_then(|value| value.parse::<i32>().ok())
        .is_some_and(|value| value >= 0);
    tabindex_focusable
        || element.contenteditable
        || matches!(
            element.tag.as_str(),
            "button" | "input" | "select" | "textarea"
        )
        || (element.tag == "a" && element.href)
}

fn aria_bool(value: Option<&str>) -> Option<bool> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn count_text_matches(haystack: &str, needle: &str, case_sensitive: bool) -> u32 {
    if needle.is_empty() {
        return 0;
    }
    if case_sensitive {
        return haystack.matches(needle).count().min(u32::MAX as usize) as u32;
    }
    let haystack = haystack.to_lowercase();
    let needle = needle.to_lowercase();
    haystack.matches(&needle).count().min(u32::MAX as usize) as u32
}

fn initial_session_history(url: &str) -> SessionHistory {
    SessionHistory::new(HistoryEntry::navigation(url))
}

fn resolve_url_string(input: &str, base: &str) -> Option<String> {
    if let Ok(url) = parse_url(input) {
        return Some(url.serialize());
    }
    let base = parse_url(base).ok()?;
    parse_url_with_base(input, &base)
        .ok()
        .map(|url| url.serialize())
}

fn element_attr(info: &ElementInfo, name: &str) -> Option<String> {
    info.attributes
        .iter()
        .find(|(attr_name, _)| attr_name == name)
        .map(|(_, value)| value.clone())
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
    fn accessibility_snapshot_projects_dom_semantics_focus_and_layout() {
        let mut page = Page::from_html(
            "file:///accessibility.html",
            r#"<!doctype html>
                <style>html,body{margin:0}button,input,img,[role]{display:block;width:120px;height:24px}</style>
                <button aria-label="Save" disabled aria-expanded="true">Ignored</button>
                <input type="checkbox" aria-label="Remember" checked>
                <img alt="Vixen logo">
                <div role="tab" title="Settings" tabindex="0" aria-selected="true"></div>
                <div aria-hidden="true"><button>Secret</button></div>"#,
        )
        .unwrap();
        let checkbox_id = page
            .query_selector_all("input")
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
            .node_id;
        page.set_focused_element_node_id(Some(checkbox_id));

        let snapshot = page.accessibility_snapshot(
            BrowsingContextId::new(7).unwrap(),
            DocumentId::new(9).unwrap(),
            (320, 240),
        );
        assert_eq!(snapshot.context_id.get(), 7);
        assert_eq!(snapshot.document_id.get(), 9);
        assert_eq!(snapshot.viewport, (320, 240));
        assert!(!snapshot.truncated);
        assert!(!snapshot.nodes.iter().any(|node| node.label == "Secret"));

        let button = snapshot
            .nodes
            .iter()
            .find(|node| node.label == "Save")
            .unwrap();
        assert_eq!(button.role, "button");
        assert!(button.disabled);
        assert_eq!(button.expanded, Some(true));
        assert!(!button.focusable);
        assert!(button.actions.is_empty());
        let bounds = button.bbox.unwrap();
        assert_eq!(bounds.x, 0.0);
        assert_eq!(bounds.width, 120.0);
        assert_eq!(bounds.height, 24.0);

        let checkbox = snapshot
            .nodes
            .iter()
            .find(|node| node.label == "Remember")
            .unwrap();
        assert_eq!(checkbox.role, "checkbox");
        assert_eq!(checkbox.checked, Some(true));
        assert!(checkbox.focused);
        assert!(checkbox.focusable);
        assert_eq!(checkbox.actions, ["tap"]);

        let image = snapshot
            .nodes
            .iter()
            .find(|node| node.label == "Vixen logo")
            .unwrap();
        assert_eq!(image.role, "image");
        let tab = snapshot
            .nodes
            .iter()
            .find(|node| node.label == "Settings")
            .unwrap();
        assert_eq!(tab.role, "tab");
        assert!(tab.selected);
        assert!(tab.focusable);
    }

    #[test]
    fn accessibility_snapshot_truncates_nodes_and_utf8_safely() {
        let mut html = format!("<button aria-label=\"{}\">long</button>", "🦊".repeat(200));
        for index in 0..ACCESSIBILITY_MAX_NODES + 8 {
            html.push_str(&format!("<button>button {index}</button>"));
        }
        let page = Page::from_html("file:///bounded.html", &html).unwrap();
        let snapshot = page.accessibility_snapshot(
            BrowsingContextId::new(1).unwrap(),
            DocumentId::new(1).unwrap(),
            (800, 600),
        );

        assert_eq!(snapshot.nodes.len(), ACCESSIBILITY_MAX_NODES);
        assert!(snapshot.truncated);
        let long = snapshot
            .nodes
            .iter()
            .find(|node| node.role == "button" && node.label.starts_with('🦊'))
            .unwrap();
        assert_eq!(long.label.len(), ACCESSIBILITY_MAX_STRING_BYTES);
        assert!(long.label.is_char_boundary(long.label.len()));
    }

    #[test]
    fn accessibility_names_use_idrefs_and_native_labels_without_hidden_text() {
        let page = Page::from_html(
            "file:///names.html",
            r#"<!doctype html>
                <style>.not-rendered{display:none}</style>
                <span id="first">First <b>name</b><i hidden>hidden</i></span>
                <span id="second" aria-label="Second"></span>
                <button aria-labelledby="first first missing second">Fallback</button>
                <span id="hidden-reference" hidden>Hidden <b>reference</b></span>
                <button aria-labelledby="hidden-reference">Hidden fallback</button>
                <button>Visible <span hidden>leak</span></button>
                <span id="cycle-a" aria-labelledby="cycle-b"></span>
                <span id="cycle-b" aria-labelledby="cycle-a">Cycle B</span>
                <button aria-labelledby="cycle-a">Cycle fallback</button>
                <button>Nested <span>button</span></button>
                <label for="named">For label <b aria-hidden="true">secret</b><i class="not-rendered">paint</i></label>
                <input id="named">
                <label>Wrapped <b hidden>secret</b><textarea></textarea></label>
                <div><div>Leaf text</div></div>
                <main><span>Main leaf</span></main>
                <div role="presentation">Container <span>Presented leaf</span></div>"#,
        )
        .unwrap();

        let snapshot = page.accessibility_snapshot(
            BrowsingContextId::new(1).unwrap(),
            DocumentId::new(1).unwrap(),
            (800, 600),
        );
        let button = snapshot
            .nodes
            .iter()
            .find(|node| node.role == "button")
            .unwrap();
        assert_eq!(button.label, "First name Second");
        assert!(
            snapshot
                .nodes
                .iter()
                .any(|node| node.role == "button" && node.label == "Cycle B")
        );
        assert!(
            snapshot
                .nodes
                .iter()
                .any(|node| node.role == "button" && node.label == "Nested button")
        );
        assert!(
            snapshot
                .nodes
                .iter()
                .any(|node| node.role == "button" && node.label == "Hidden reference")
        );
        assert!(
            snapshot
                .nodes
                .iter()
                .any(|node| node.role == "button" && node.label == "Visible")
        );
        let textboxes = snapshot
            .nodes
            .iter()
            .filter(|node| node.role == "textbox")
            .collect::<Vec<_>>();
        assert_eq!(textboxes.len(), 2);
        assert!(textboxes.iter().any(|node| node.label == "For label"));
        assert!(textboxes.iter().any(|node| node.label == "Wrapped"));
        assert_eq!(
            snapshot
                .nodes
                .iter()
                .filter(|node| node.role == "generic" && node.label == "Leaf text")
                .count(),
            1
        );
        assert!(
            snapshot
                .nodes
                .iter()
                .any(|node| node.label == "Presented leaf")
        );
        assert!(
            snapshot
                .nodes
                .iter()
                .any(|node| node.role == "main" && node.label.is_empty())
        );
        assert!(
            snapshot
                .nodes
                .iter()
                .any(|node| node.role == "generic" && node.label == "Main leaf")
        );
        assert!(
            !snapshot
                .nodes
                .iter()
                .any(|node| node.role == "generic" && node.label == "button")
        );
        assert!(!snapshot.nodes.iter().any(|node| {
            node.label.contains("hidden")
                || node.label.contains("secret")
                || node.label.contains("paint")
                || node.label.contains("Container")
        }));
    }

    #[test]
    fn accessibility_generation_is_stable_and_changes_with_projection() {
        let mut page = Page::from_html(
            "file:///generation.html",
            "<style>button{width:100px}</style><button id='target'>Before</button>",
        )
        .unwrap();
        let context_id = BrowsingContextId::new(1).unwrap();
        let document_id = DocumentId::new(1).unwrap();
        let first = page.accessibility_snapshot(context_id, document_id, (800, 600));
        let identical = page.accessibility_snapshot(context_id, document_id, (800, 600));
        assert_ne!(first.generation, 0);
        assert_eq!(first.generation, identical.generation);

        let target = page.query_selector_all("button").unwrap()[0].node_id;
        page.set_focused_element_node_id(Some(target));
        let focused = page.accessibility_snapshot(context_id, document_id, (800, 600));
        assert_ne!(first.generation, focused.generation);

        page.set_element_attribute(target, "aria-label", "After")
            .unwrap();
        let renamed = page.accessibility_snapshot(context_id, document_id, (800, 600));
        assert_ne!(focused.generation, renamed.generation);

        let resized = page.accessibility_snapshot(context_id, document_id, (400, 600));
        assert_ne!(renamed.generation, resized.generation);
    }

    #[test]
    fn accessibility_source_generation_tracks_mutations_and_clears_structural_focus() {
        let mut page = Page::from_html(
            "file:///mutations.html",
            "<div id='root'><button id='target'>Same</button></div>",
        )
        .unwrap();
        let context_id = BrowsingContextId::new(1).unwrap();
        let document_id = DocumentId::new(1).unwrap();
        let root = page.query_selector_all("#root").unwrap()[0].node_id;
        let target = page.query_selector_all("#target").unwrap()[0].node_id;
        page.set_focused_element_node_id(Some(target));
        let focused = page.accessibility_snapshot(context_id, document_id, (800, 600));
        page.set_focused_element_node_id(Some(target));
        let repeated_focus = page.accessibility_snapshot(context_id, document_id, (800, 600));
        assert_eq!(focused.source_generation, repeated_focus.source_generation);

        page.set_element_inner_html(root, "<button id='target'>Same</button>")
            .unwrap();
        assert_eq!(page.focused_element_node_id(), None);
        let replaced = page.accessibility_snapshot(context_id, document_id, (800, 600));
        assert!(replaced.source_generation > focused.source_generation);
        assert_ne!(replaced.generation, focused.generation);

        page.set_element_inner_html(root, "<button id='target'>Same</button>")
            .unwrap();
        let identically_replaced = page.accessibility_snapshot(context_id, document_id, (800, 600));
        assert!(identically_replaced.source_generation > replaced.source_generation);
        assert_ne!(identically_replaced.generation, replaced.generation);

        let replacement_target = page.query_selector_all("#target").unwrap()[0].node_id;
        page.set_focused_element_node_id(Some(replacement_target));
        page.set_element_text_content(root, "Same").unwrap();
        assert_eq!(page.focused_element_node_id(), None);
        let text_replaced = page.accessibility_snapshot(context_id, document_id, (800, 600));
        assert!(text_replaced.source_generation > identically_replaced.source_generation);
    }

    #[test]
    fn accessibility_roles_and_inherited_disabledness_are_conservative() {
        let page = Page::from_html(
            "file:///roles.html",
            r#"<!doctype html>
                <button role="invalid presentation">Native button</button>
                <div role="invalid checkbox button" aria-label="Chosen role"></div>
                <div role="invalid" tabindex="0">Invalid fallback</div>
                <div role="none" tabindex="0">Focusable conflict</div>
                <img role="presentation" alt="Decorative">
                <fieldset disabled>
                  <legend><input aria-label="Legend control"></legend>
                  <input aria-label="Fieldset control">
                </fieldset>
                <select><optgroup disabled><option>Disabled option</option></optgroup></select>"#,
        )
        .unwrap();
        let snapshot = page.accessibility_snapshot(
            BrowsingContextId::new(1).unwrap(),
            DocumentId::new(1).unwrap(),
            (800, 600),
        );

        assert!(
            snapshot
                .nodes
                .iter()
                .any(|node| node.role == "button" && node.label == "Native button")
        );
        assert!(
            snapshot
                .nodes
                .iter()
                .any(|node| node.role == "checkbox" && node.label == "Chosen role")
        );
        assert!(snapshot.nodes.iter().any(|node| {
            node.role == "generic" && node.label == "Invalid fallback" && node.focusable
        }));
        assert!(snapshot.nodes.iter().any(|node| {
            node.role == "generic" && node.label == "Focusable conflict" && node.focusable
        }));
        assert!(!snapshot.nodes.iter().any(|node| node.label == "Decorative"));
        for label in ["Legend control", "Fieldset control", "Disabled option"] {
            let node = snapshot
                .nodes
                .iter()
                .find(|node| node.label == label)
                .unwrap();
            assert!(node.disabled, "{label} should inherit disabledness");
            assert!(node.actions.is_empty());
        }
    }

    #[test]
    fn page_parser_advances_on_utf8_boundaries() {
        let html = "<!doctype html><title>Chunked</title><main>fox \u{1f98a}</main>".to_owned();
        let mut parser =
            PageParser::with_headers("https://example.test/".to_owned(), html, std::iter::empty());
        let page = loop {
            if let Some(page) = parser.advance(3).unwrap() {
                break page;
            }
        };

        assert_eq!(page.url(), "https://example.test/");
        assert_eq!(page.document().title().as_deref(), Some("Chunked"));
        assert_eq!(page.text_content(), "fox \u{1f98a}");
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
    fn page_counts_visible_text_matches_for_find() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<html><head><title>Rust hidden</title></head><body><p>Rust rust rustic</p><p>Trust Rust</p></body></html>",
        )
        .unwrap();

        assert_eq!(page.find_text_count("Rust", true), 2);
        assert_eq!(page.find_text_count("rust", false), 5);
        assert_eq!(page.find_text_count("hidden", false), 0);
        assert_eq!(page.find_text_count("", false), 0);
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
        assert!(dump.contains("# display-list viewport=56x200 count=5"));
        assert!(dump.contains("cmd 0: background x=0.0 y=0.0 w=56.0 h=200.0"));
        assert!(dump.contains("cmd 1: text"));
        assert!(dump.contains("text=\"one\""));
        assert!(dump.contains("text=\"four\""));
        assert!(!dump.contains("Hidden title"));
    }

    #[test]
    fn layout_fragments_split_wrapped_text_for_paint() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<style>body { margin: 0; } #wrap { width: 40px; }</style><div id='wrap'>alpha beta gamma</div>",
        )
        .unwrap();
        let fragments = page.layout_fragments((120, 80));
        let texts: Vec<_> = fragments
            .iter()
            .filter_map(|fragment| match &fragment.kind {
                LayoutFragmentKind::Text { text, .. } => Some((text.as_str(), fragment.rect.y)),
                LayoutFragmentKind::Background { .. } => None,
            })
            .collect();
        assert_eq!(texts, vec![("alpha", 0.0), ("beta", 19.2), ("gamma", 38.4)]);
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
        assert!(dump.contains("text=\"Overf\""));
        assert!(!dump.contains("text=\"lowin\""));
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
        assert_eq!(stats.text_runs, 2);
        assert_eq!(stats.commands, 3);
        let dump = page.dump_paint_stats((56, 200));
        assert!(dump.contains("# paint-stats viewport=56x200 commands=3"));
        assert!(dump.contains("text-runs=2"));
    }

    fn style_value<'a>(styles: &'a [(String, String)], property: &str) -> Option<&'a str> {
        styles
            .iter()
            .find(|(name, _)| name == property)
            .map(|(_, value)| value.as_str())
    }
}
