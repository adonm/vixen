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
use vixen_net::csp::ContentSecurityPolicy;

use crate::display_list::{
    BackgroundAttachment, BackgroundBox, Color, DisplayListBuilder, DrawItem, PaintCommand,
    PaintStats, Rect, TextRun, dump_paint_commands, dump_paint_stats,
};
use crate::doc::{Document, InlineScript, ParseError};
use crate::history::{HistoryEntry, SessionHistory};
use crate::layout_tree::{
    LayoutFragment, LayoutFragmentKind, LayoutTree, build_layout_tree, dump_layout_tree,
    layout_fragments_from_tree, line_boxes_from_tree,
};
use crate::line_layout::{LineBox, dump_line_boxes};
use crate::style_cascade::AuthorStylesheet;
use crate::style_dom::Selector;
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
    selection: Option<PageSelection>,
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
        let url = url.into();
        let document = Document::parse(html)?;
        let author_stylesheet = AuthorStylesheet::from_blocks(&document.style_blocks());
        let history = initial_session_history(&url);
        Ok(Self {
            url,
            document,
            history,
            csp: ContentSecurityPolicy::new(),
            author_stylesheet,
            diagnostics: Vec::new(),
            focused_element_node_id: None,
            selection: None,
        })
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
        let url = url.into();
        let document = Document::parse(html)?;
        let csp = ContentSecurityPolicy::from_headers(headers);
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
        self.focused_element_node_id = node_id;
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
        self.refresh_author_stylesheet();
        Ok(())
    }

    /// Remove an element attribute in the authoritative Page DOM.
    pub fn remove_element_attribute(&mut self, node_id: usize, name: &str) -> Result<(), String> {
        self.document.remove_element_attribute(node_id, name)?;
        self.refresh_author_stylesheet();
        Ok(())
    }

    /// Set the document title from a runtime-produced `document.title` or
    /// `document.write()` mutation.
    pub fn set_title(&mut self, value: &str) -> Result<(), String> {
        self.document.set_title(value)
    }

    /// Replace an element's child subtree from a runtime-produced HTML fragment.
    pub fn set_element_inner_html(&mut self, node_id: usize, html: &str) -> Result<(), String> {
        self.document.set_element_inner_html(node_id, html)?;
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
        self.refresh_author_stylesheet();
        Ok(())
    }

    fn refresh_author_stylesheet(&mut self) {
        self.author_stylesheet = AuthorStylesheet::from_blocks(&self.document.style_blocks());
    }

    /// Enforcing CSP delivered with the document response headers.
    pub fn csp(&self) -> &ContentSecurityPolicy {
        &self.csp
    }

    /// Inline classic scripts in document order. Full page-script execution also
    /// handles external classic scripts through the JS/runtime boundary.
    pub fn inline_classic_scripts(&self) -> Vec<InlineScript> {
        self.document.inline_classic_scripts()
    }

    /// True when the page contains at least one inline or external classic
    /// script that should cross the script execution boundary.
    pub fn has_classic_scripts(&self) -> bool {
        self.document.has_classic_scripts()
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

    /// Legacy compatibility hook for old smoke harnesses.
    ///
    /// Browser-visible evaluation now runs through [`crate::script::JsRuntime`]
    /// host objects. Keep this method as a fail-closed shim for callers that
    /// still probe it, but do not add new Page string projections here.
    pub fn evaluate_dom_expression(&self, expr: &str) -> Option<Result<String, String>> {
        let _ = expr;
        None
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
    fn page_does_not_project_document_runtime_values() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<html><head><base href='https://example.com/app/page'><title>T</title></head><body><p class='x'>one</p><p>two</p></body></html>",
        )
        .unwrap();

        for expr in [
            "document.title",
            "document.documentURI",
            "document.baseURI",
            "document.hasFocus()",
            "document.querySelectorAll('p').length",
            "document.querySelectorAll('p >').length",
            "document.querySelectorAll(').length",
        ] {
            assert_eq!(page.evaluate_dom_expression(expr), None, "{expr}");
        }
        assert_eq!(page.evaluate_dom_expression("1+2"), None);
    }

    #[test]
    fn page_does_not_project_computed_style_runtime_values() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<style>p { color: red; margin-left: 4px; } #copy { color: blue; font-size: 20px !important; --Token: A:B; }</style><p id='copy' style='font-size: 18px; margin-left: 10px'>Text</p>",
        )
        .unwrap();

        for expr in [
            "getComputedStyle(document.querySelector('#copy')).color",
            "getComputedStyle(document.querySelector('#copy')).fontSize",
            "window.getComputedStyle(document.querySelector('#copy')).getPropertyValue('margin-left')",
            "getComputedStyle(document.querySelector('#copy')).getPropertyValue('--Token')",
        ] {
            assert_eq!(page.evaluate_dom_expression(expr), None, "{expr}");
        }
    }

    #[test]
    fn page_does_not_project_cssom_runtime_values() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<style>#box { width: 40px; height: 20px; }</style><main><div id='box'>Box</div></main>",
        )
        .unwrap();

        for expr in [
            "CSS.supports('display', 'grid')",
            "CSS.supports('(unknown-prop: yes)')",
            "document.styleSheets.length",
            "document.styleSheets[0].cssRules.length",
            "document.styleSheets[0].disabled",
            "document.styleSheets[0].href === null",
            "document.styleSheets[0].ownerNode.tagName",
            "document.styleSheets[0].cssRules[0].selectorText",
            "document.styleSheets[0].cssRules[0].style.length",
            "document.styleSheets[0].cssRules[0].style.getPropertyValue('width')",
            "document.styleSheets[0].cssRules[0].style[1]",
        ] {
            assert_eq!(page.evaluate_dom_expression(expr), None, "{expr}");
        }
    }

    #[test]
    fn page_does_not_project_dom_geometry_values() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<style>#box { width: 40px; height: 20px; }</style><main><div id='box'>Box</div></main>",
        )
        .unwrap();

        for expr in [
            "document.querySelector('#box').getBoundingClientRect().x",
            "document.querySelector('#box').getBoundingClientRect().width",
            "document.querySelector('#box').getBoundingClientRect().right",
            "document.querySelector('#box').getClientRects().length",
        ] {
            assert_eq!(page.evaluate_dom_expression(expr), None, "{expr}");
        }
    }

    #[test]
    fn page_does_not_project_geometry_constructor_values() {
        let page = Page::from_html("file:///fixture.html", "<main>geometry</main>").unwrap();

        for expr in [
            "new DOMPoint(1,2,3,4).z",
            "DOMPoint.fromPoint({x:5,y:6}).w",
            "new DOMRect(10,20,-5,7).left",
            "DOMRect.fromRect({x:1,y:2,width:3,height:4}).bottom",
            "DOMQuad.fromRect({x:1,y:2,width:3,height:4}).p3.x",
            "DOMQuad.fromRect({x:1,y:2,width:3,height:4}).getBounds().height",
            "new DOMMatrix().is2D",
            "new DOMMatrix([1,0,0,1,5,6]).e",
            "new DOMMatrix().translate(10,20).transformPoint(new DOMPoint(1,2)).y",
            "new DOMMatrix().scale(2,3).transformPoint(new DOMPoint(5,5)).x",
        ] {
            assert_eq!(page.evaluate_dom_expression(expr), None, "{expr}");
        }
    }

    #[test]
    fn page_does_not_project_dom_query_member_values() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<main id='root'><p id='lead' class='note primary' data-role='intro'>Lead <b id='bold'>Beta</b></p><section id='empty'></section><form id='form'></form></main>",
        )
        .unwrap();

        for expr in [
            "document.documentElement.tagName",
            "document.body.tagName",
            "document.activeElement.tagName",
            "document.forms.length",
            "document.getElementsByTagName('p').length",
            "document.getElementsByClassName('note').length",
            "document.querySelector('#lead').id",
            "document.querySelector('#lead').className",
            "document.querySelector('#lead').tagName",
            "document.querySelector('#lead').nodeName",
            "document.querySelector('#lead').localName",
            "document.querySelector('#lead').nodeType",
            "document.querySelector('#lead').isConnected",
            "document.querySelector('#lead').ownerDocument === document",
            "document.querySelector('#lead').textContent",
            "document.querySelector('#root').children.length",
            "document.querySelector('#root').firstElementChild.id",
            "document.querySelector('#root').lastElementChild.id",
            "document.querySelector('#bold').parentElement.id",
            "document.querySelector('#empty').previousElementSibling.id",
            "document.querySelector('#empty').nextElementSibling.id",
            "document.querySelector('#form').method",
            "document.querySelector('#lead').getAttribute('data-role')",
            "document.querySelector('#lead').hasAttribute('hidden')",
            "document.querySelector('#lead').matches('p.note')",
            "document.querySelector('#bold').closest('main').id",
            "document.querySelector('#lead').closest('p.note') !== null",
            "document.querySelector('#bold').closest('.missing') === null",
            "document.querySelector('.missing') === null",
            "document.getElementById('empty').tagName",
        ] {
            assert_eq!(page.evaluate_dom_expression(expr), None, "{expr}");
        }
    }

    #[test]
    fn page_does_not_project_form_reflected_property_values() {
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

        for expr in [
            "document.querySelector('#f').method",
            "document.querySelector('#f').enctype",
            "document.querySelector('#f').action",
            "document.querySelector('#l').htmlFor",
            "document.querySelector('#email').type",
            "document.querySelector('#email').name",
            "document.querySelector('#email').value",
            "document.querySelector('#email').required",
            "document.querySelector('#agree').checked",
            "document.querySelector('#agree').disabled",
            "document.querySelector('#bio').readOnly",
            "document.querySelector('#roles').multiple",
            "document.querySelector('#admin').selected",
            "document.querySelector('#save').type",
        ] {
            assert_eq!(page.evaluate_dom_expression(expr), None, "{expr}");
        }
    }

    #[test]
    fn page_does_not_project_form_data_runtime_values() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<form id='contact'>\
               <input name='name' value='Ada'>\
               <input name='skip' value='no' disabled>\
               <textarea name='body'>Hello, world!</textarea>\
               <select name='urgency'><option value='low'>Low</option><option value='normal' selected>Normal</option></select>\
               <input type='checkbox' name='newsletter' value='yes' checked>\
               <input type='radio' name='format' value='html' checked>\
               <input type='radio' name='format' value='text'>\
             </form>\
             <form id='upload'><input type='file' name='attachment'></form>",
        )
        .unwrap();

        for expr in [
            "new FormData(document.querySelector('#contact')).get('name')",
            "new FormData(document.querySelector('#contact')).get('body')",
            "new FormData(document.querySelector('#contact')).get('urgency')",
            "new FormData(document.querySelector('#contact')).getAll('format').length",
            "new FormData(document.querySelector('#contact')).has('skip')",
            "new FormData(document.querySelector('#contact')).get('missing') === null",
            "new FormData(document.querySelector('#contact')).entries().next().value[0]",
            "new FormData(document.querySelector('#contact')).entries().next().value[1]",
            "new FormData(document.querySelector('#contact')).keys().next().value",
            "new FormData(document.querySelector('#contact')).values().next().value",
            "new FormData(document.getElementById('upload')).get('attachment').type",
            "new FormData(document.getElementById('upload')).get('attachment').size",
        ] {
            assert_eq!(page.evaluate_dom_expression(expr), None, "{expr}");
        }
    }

    #[test]
    fn page_does_not_project_token_list_or_dataset_values() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<html><head><link id='theme' rel='stylesheet alternate'></head>\
             <body><div id='dupes' class='a b a c b' data-user-id='42' data-api-base='/v1'>x</div></body></html>",
        )
        .unwrap();

        for expr in [
            "document.querySelector('#dupes').classList.length",
            "document.querySelector('#dupes').classList.item(1)",
            "document.querySelector('#dupes').classList.contains('a')",
            "document.querySelector('#theme').relList.contains('alternate')",
            "document.querySelector('#dupes').dataset.userId",
            "document.querySelector('#dupes').dataset['apiBase']",
        ] {
            assert_eq!(page.evaluate_dom_expression(expr), None, "{expr}");
        }
    }

    #[test]
    fn page_does_not_project_range_selection_runtime_values() {
        let page = Page::from_html(
            "file:///fixture.html#initial",
            "<main><p>history and selection smoke</p></main>",
        )
        .unwrap();

        for expr in [
            "document.createRange().collapsed",
            "document.createRange().startOffset",
            "document.createRange().endOffset",
            "document.createRange().toString()",
            "window.getSelection().rangeCount",
            "document.getSelection().isCollapsed",
        ] {
            assert_eq!(page.evaluate_dom_expression(expr), None, "{expr}");
        }
    }

    #[test]
    fn page_does_not_project_traversal_mutation_or_clone_runtime_values() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<main><div id='walk-root'><article id='art-1'><h2 id='heading'>Heading</h2><p id='para-1'>first</p><p id='para-2'>second</p></article><aside id='aside-1'><span id='aside-span'>aside</span></aside></div></main>",
        )
        .unwrap();

        for expr in [
            "structuredClone('hello')",
            "structuredClone([1,2,3]).length",
            "structuredClone({greeting:'hello'}).greeting",
            "structuredClone(new Date(42)).getTime()",
            "structuredClone(new Map([['answer', 42]])).get('answer')",
            "structuredClone(new Map([['answer', 42]])).entries().next().value[0]",
            "structuredClone(new Set(['alpha','beta'])).has('beta')",
            "structuredClone(new TypeError('boom')).name",
            "structuredClone(new TypeError('boom')).message",
            "new MutationObserver(() => {}).takeRecords().length",
            "new MutationObserver(() => {}).disconnect()",
            "document.createTreeWalker(document.getElementById('walk-root'), NodeFilter.SHOW_ELEMENT).root.id",
            "document.createTreeWalker(document.getElementById('walk-root'), NodeFilter.SHOW_ELEMENT).firstChild().id",
            "document.createTreeWalker(document.getElementById('walk-root'), NodeFilter.SHOW_ELEMENT).lastChild().id",
            "document.createNodeIterator(document.getElementById('walk-root'), NodeFilter.SHOW_ELEMENT).nextNode().id",
        ] {
            assert_eq!(page.evaluate_dom_expression(expr), None, "{expr}");
        }
    }

    #[test]
    fn page_does_not_project_fetch_body_runtime_values() {
        let page = Page::from_html("file:///fixture.html", "<p>x</p>").unwrap();

        for expr in [
            "new Headers([['X-Test','a']]).get('x-test')",
            "new Blob(['Hi'], { type: 'TEXT/PLAIN' }).size",
            "new File(['hello'], 'note.txt', { type: 'text/plain' }).name",
            "new Response('Created', { status: 201 }).status",
            "Response.json({ok:true}, { status: 201 }).status",
            "Response.error().status",
            "Response.redirect('https://example.com/target', 302).ok",
            "new Request('https://example.com/api').method",
        ] {
            assert_eq!(page.evaluate_dom_expression(expr), None, "{expr}");
        }
    }

    #[test]
    fn page_does_not_project_abort_and_url_runtime_values() {
        let page = Page::from_html("file:///fixture.html", "<p>x</p>").unwrap();

        for expr in [
            "new AbortController().signal.aborted",
            "AbortSignal.timeout(0).aborted",
            "AbortSignal.any([AbortSignal.timeout(0)]).aborted",
            "new URLPattern({ pathname: '/posts/:id' }).test({ pathname: '/posts/42' })",
            "new URLPattern({ pathname: '/posts/:id' }).exec({ pathname: '/posts/42' }).pathname.groups.id",
            "new URLPattern({ pathname: '/assets/*' }).exec({ pathname: '/assets/img/logo.png' }).pathname.groups['*']",
            "URL.canParse('/other', 'https://example.com/app/page')",
            "URL.canParse('://bad')",
            "URL.canParse('data:text/plain,Hello')",
            "new URL('data:text/plain,Hello').protocol",
            "new URL('data:text/plain,Hello').origin",
            "new URL('data:text/plain,Hello').pathname",
            "new URL('https://example.com:8443/path?q=1#frag').origin",
            "new URL('https://example.com/path?q=1&tag=web&tag=engine').toString()",
            "new URL('https://example.com/path?q=1&tag=web&tag=engine').searchParams.size",
            "new URL('https://example.com/path?q=1&tag=web&tag=engine').searchParams.has('tag')",
            "new URL('https://example.com/path?q=1&tag=web&tag=engine').searchParams.getAll('tag')[1]",
            "new URL('/other', 'https://example.com/app/page').href",
            "new URL('https://example.com/path?q=1#frag').searchParams.get('q')",
            "new URLSearchParams('?q=rust+lang&tag=web&tag=engine').get('q')",
            "new URLSearchParams('tag=web&tag=engine').getAll('tag').length",
            "new URLSearchParams('a=1&b=2').has('b')",
            "new URLSearchParams('space=a b').toString()",
            "new URLSearchParams([['q','rust lang'], ['tag','web'], ['tag','engine']]).toString()",
            "new URLSearchParams([['q','rust lang'], ['tag','web']]).entries().next().value[0]",
            "new URLSearchParams([['q','rust lang'], ['tag','web']]).entries().next().value[1]",
            "new URLSearchParams([['q','rust lang'], ['tag','web']]).keys().next().value",
            "new URLSearchParams([['q','rust lang'], ['tag','web']]).values().next().value",
            "new URLSearchParams('tag=web').has('tag', 'web')",
        ] {
            assert_eq!(page.evaluate_dom_expression(expr), None, "{expr}");
        }
    }

    #[test]
    fn page_does_not_project_perf_media_navigator_runtime_values() {
        let page = Page::from_html("file:///fixture.html", "<p>x</p>").unwrap();

        for expr in [
            "typeof performance.now()",
            "performance.now() >= 0",
            "performance.timeOrigin + performance.now() >= performance.timeOrigin",
            "matchMedia('(min-width: 800px)').matches",
            "window.matchMedia('print').matches",
            "navigator.onLine",
            "window.navigator.cookieEnabled",
            "navigator.languages[0]",
            "navigator.userAgent.includes('Vixen')",
        ] {
            assert_eq!(page.evaluate_dom_expression(expr), None, "{expr}");
        }
    }

    #[test]
    fn page_does_not_project_document_viewport_or_event_values() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<html><head><meta id='charset' charset='utf-8'><meta id='referrer' name='referrer' content='strict-origin'><title>T</title></head>\
             <body><iframe id='frame' sandbox='allow-scripts allow-forms'></iframe><button id='btn'>go</button></body></html>",
        )
        .unwrap();

        for expr in [
            "document.readyState",
            "document.compatMode",
            "window.innerWidth",
            "window.innerHeight",
            "devicePixelRatio",
            "screen.width",
            "visualViewport.scale",
            "document.defaultView === window",
            "document.scrollingElement.tagName",
            "document.characterSet",
            "document.querySelector('#referrer').content",
            "document.querySelector('#charset').charset",
            "document.querySelector('#frame').sandbox.contains('allow-forms')",
            "document.querySelector('#btn').dispatchEvent(new Event('click'))",
        ] {
            assert_eq!(page.evaluate_dom_expression(expr), None, "{expr}");
        }
    }

    #[test]
    fn page_does_not_project_event_constructor_values() {
        let page = Page::from_html("file:///fixture.html", "<p>x</p>").unwrap();

        for expr in [
            "new Event('ready', {bubbles:true}).bubbles",
            "new Event('message').target === null",
            "new Event('message').composedPath().length",
            "new CustomEvent('ready', {detail:'ok'}).detail",
        ] {
            assert_eq!(page.evaluate_dom_expression(expr), None, "{expr}");
        }
    }

    #[test]
    fn page_does_not_project_text_codec_base64_or_domparser_values() {
        let page = Page::from_html("file:///fixture.html", "<p>x</p>").unwrap();

        for expr in [
            "new TextEncoder().encoding",
            "new TextEncoder().encode('é').length",
            "new TextEncoder().encodeInto('aé', new Uint8Array(3)).written",
            "new TextDecoder().decode([65,13,10,66])",
            "new TextDecoder('UTF-8', { fatal: true }).fatal",
            "btoa('Vixen')",
            "atob('Vml4ZW4=')",
            "new DOMParser().parseFromString(\"<main><p id='parsed'>Parsed</p></main>\", 'text/html').querySelector('#parsed').textContent",
        ] {
            assert_eq!(page.evaluate_dom_expression(expr), None, "{expr}");
        }
    }

    #[test]
    fn page_does_not_project_html_serialization_values() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<main id='root'><h1 id='title'>DOM <span>Basic</span></h1><p id='outro'>Closing text.</p></main>",
        )
        .unwrap();

        for expr in [
            "document.querySelector('#title').innerHTML",
            "document.querySelector('#outro').outerHTML",
        ] {
            assert_eq!(page.evaluate_dom_expression(expr), None, "{expr}");
        }
    }

    #[test]
    fn page_does_not_project_responsive_image_current_src_values() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<img id='widths' src='small.jpg' srcset='small.jpg 480w, medium.jpg 800w, large.jpg 1200w' sizes='100vw'>\
             <img id='density' srcset='one.png 1x, two.png 2x'>\
             <img id='fallback' src='fallback.jpg'>",
        )
        .unwrap();

        for expr in [
            "document.querySelector('#widths').currentSrc",
            "document.querySelector('#density').currentSrc",
            "document.querySelector('#fallback').currentSrc",
        ] {
            assert_eq!(page.evaluate_dom_expression(expr), None, "{expr}");
        }
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
