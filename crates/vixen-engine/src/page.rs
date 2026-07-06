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

use crate::doc::{Document, ParseError};
use crate::line_layout::{LineBox, LineLayoutConfig, dump_line_boxes, layout_text_lines};
use crate::style_dom::Selector;

/// A loaded page at the current vertical integration boundary.
pub struct Page {
    url: String,
    document: Document,
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
        Ok(Self {
            url: url.into(),
            document,
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

    /// First executable Phase 4 layout slice: body text wrapped into stable
    /// line boxes for a viewport.
    pub fn layout_lines(&self, viewport: (u32, u32)) -> Vec<LineBox> {
        layout_text_lines(
            &self.document.body_text_content(),
            LineLayoutConfig::for_viewport(viewport),
        )
    }

    /// Text dump for `vixen-headless --dump-lines`.
    pub fn dump_lines(&self, viewport: (u32, u32)) -> String {
        dump_line_boxes(&self.layout_lines(viewport), viewport)
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

    /// Computed style facade for the current Phase 3 vertical slice.
    ///
    /// Full Stylo cascade lands next. Until then this returns the element's
    /// inline declaration block with CSS cascade basics applied locally:
    /// declaration parsing, ASCII case-insensitive property names, inline
    /// `!important` precedence, and last-declaration-wins within the same
    /// priority tier. Missing elements or elements without inline style return
    /// an empty vector rather than fabricating UA/default values.
    pub fn computed_style(&self, node_id: usize) -> Vec<(String, String)> {
        self.document
            .element_by_node_id(node_id)
            .and_then(|element| {
                element
                    .attributes
                    .into_iter()
                    .find(|(name, _)| name == "style")
                    .map(|(_, value)| parse_inline_style(&value))
            })
            .unwrap_or_default()
    }

    /// Diagnostics accumulated by pipeline stages.
    pub fn diagnostics(&self) -> Vec<EngineDiagnostic> {
        self.diagnostics.clone()
    }
}

fn parse_inline_style(style: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String, bool)> = Vec::new();
    for declaration in split_top_level(style, ';') {
        let Some((name, value)) = split_once_top_level(declaration, ':') else {
            continue;
        };
        let Some(property) = normalise_property_name(name) else {
            continue;
        };
        let Some((value, important)) = normalise_declaration_value(value) else {
            continue;
        };
        if let Some((_, existing_value, existing_important)) =
            out.iter_mut().find(|(p, _, _)| *p == property)
        {
            if important || !*existing_important {
                *existing_value = value;
                *existing_important = important;
            }
        } else {
            out.push((property, value, important));
        }
    }
    out.into_iter()
        .map(|(property, value, _)| (property, value))
        .collect()
}

fn normalise_property_name(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    if name.starts_with("--") {
        return Some(name.to_owned());
    }
    let lower = name.to_ascii_lowercase();
    if lower
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        Some(lower)
    } else {
        None
    }
}

fn normalise_declaration_value(value: &str) -> Option<(String, bool)> {
    let (value, important) = strip_important(value.trim());
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some((value.to_owned(), important))
    }
}

fn strip_important(value: &str) -> (&str, bool) {
    let trimmed = value.trim_end();
    if trimmed.to_ascii_lowercase().ends_with("!important") {
        let keep_len = trimmed.len() - "!important".len();
        (&trimmed[..keep_len], true)
    } else {
        (value, false)
    }
}

fn split_top_level(input: &str, delimiter: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    for (idx, ch) in top_level_chars(input) {
        if ch == delimiter {
            parts.push(&input[start..idx]);
            start = idx + ch.len_utf8();
        }
    }
    parts.push(&input[start..]);
    parts
}

fn split_once_top_level(input: &str, delimiter: char) -> Option<(&str, &str)> {
    for (idx, ch) in top_level_chars(input) {
        if ch == delimiter {
            let rhs = idx + ch.len_utf8();
            return Some((&input[..idx], &input[rhs..]));
        }
    }
    None
}

fn top_level_chars(input: &str) -> impl Iterator<Item = (usize, char)> + '_ {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    input.char_indices().filter_map(move |(idx, ch)| {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == q {
                quote = None;
            }
            return None;
        }

        match ch {
            '\'' | '"' => {
                quote = Some(ch);
                None
            }
            '(' => {
                depth = depth.saturating_add(1);
                None
            }
            ')' => {
                depth = depth.saturating_sub(1);
                None
            }
            _ if depth == 0 => Some((idx, ch)),
            _ => None,
        }
    })
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
}
