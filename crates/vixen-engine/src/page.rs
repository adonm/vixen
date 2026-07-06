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
use crate::layout_tree::{LayoutTree, build_layout_tree, dump_layout_tree, line_boxes_from_tree};
use crate::line_layout::{LineBox, dump_line_boxes};
use crate::style_cascade::AuthorStylesheet;
use crate::style_dom::Selector;

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
        self.document.text_content()
    }

    /// First Vixen-owned layout tree slice: styled DOM projected into an
    /// arena-backed tree with stable layout-node ids.
    pub fn layout_tree(&self, viewport: (u32, u32)) -> LayoutTree {
        build_layout_tree(&self.document, viewport, |node_id| {
            self.computed_style(node_id)
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

    /// First executable Phase 5 paint slice: convert the Page-backed line boxes
    /// into the single invariant-enforced display-list command stream.
    pub fn display_list(&self, viewport: (u32, u32)) -> Vec<PaintCommand> {
        let viewport_rect = Rect::new(0.0, 0.0, viewport.0 as f32, viewport.1 as f32);
        let mut builder = DisplayListBuilder::new();
        builder.push(viewport_background_item(viewport_rect));
        for (idx, line) in self.layout_lines(viewport).iter().enumerate() {
            builder.push(line_text_item(idx as u32 + 1, line, viewport_rect));
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
            text_content: self.document.text_content(),
            element_count: self.document.element_count(),
        }
    }

    /// Query selector facade over the current DOM-backed selector surface.
    pub fn query_selector_all(&self, selector: &str) -> Result<Vec<ElementInfo>, String> {
        let parsed = Selector::parse(selector).map_err(|e| e.to_string())?;
        Ok(self
            .document
            .query_all(&parsed)
            .into_iter()
            .map(|m| ElementInfo {
                node_id: m.node_id,
                tag: m.tag,
                id: m.id,
                classes: m.classes,
                attributes: m.attributes,
                text: m.text,
                bbox: None,
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

fn line_text_item(order: u32, line: &LineBox, viewport: Rect) -> DrawItem {
    let rect = Rect::new(line.x, line.y, line.w, line.h);
    DrawItem {
        order,
        z_index: 0,
        visibility: crate::display_list::Visibility::Visible,
        opacity: 1.0,
        clip: Some(viewport),
        is_viewport_background: false,
        border_box: rect,
        padding_box: rect,
        content_box: rect,
        background_clip: BackgroundBox::BorderBox,
        background_origin: BackgroundBox::ContentBox,
        background_attachment: BackgroundAttachment::Scroll,
        background: None,
        text: Some(TextRun {
            color: Color::BLACK,
            text: line.text.clone(),
        }),
    }
}

impl EngineInspector for Page {
    fn inspect_element_at(&self, _x: f64, _y: f64) -> Option<ElementInfo> {
        // Hit-testing needs layout boxes (Phase 4). The inspector surface still
        // exists so consumers can share this facade today.
        None
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
        assert_eq!(snap.element_count, 5);
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
        assert!(!dump.contains("Hidden title"));
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
