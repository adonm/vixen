//! Vertical page pipeline facade — the small integration seam every large
//! browser milestone should extend.
//!
//! This owns the loaded URL + parsed [`crate::doc::Document`] and exposes the
//! source/inspection operations used by BrowserCore and the renderer bridge.
//!
//! The intended growth path is deliberately boring:
//! `Page::from_html` → cascade/source projection → Flutter renderer commit.

#![forbid(unsafe_code)]

use vixen_api::{
    ACCESSIBILITY_MAX_NODES, ACCESSIBILITY_MAX_STRING_BYTES, AccessibilityNode, AccessibilityRange,
    AccessibilityRect, AccessibilitySnapshot, AccessibilityTextInputAction,
    AccessibilityTextInputType, AccessibilityTextSelection, BrowsingContextId, DocumentId,
    ElementInfo, EngineDiagnostic, FindTextResult, FullRenderSnapshot, PageSnapshot, RenderNode,
    RenderNodeId, RenderNodeKind, RenderPoint, RenderResource, RenderResourceId,
    RenderResourceKind, RenderRevision, RenderScrollIntent, RenderScrollIntentKind,
    RenderScrollNodeId, RenderSemanticActionKind, RenderSemanticNode, RenderStyleProperty,
    RenderViewport, SemanticNodeId,
};
use vixen_net::csp::ContentSecurityPolicy;

use crate::display_list::{
    BackgroundAttachment, BackgroundBox, Color, DisplayListBuilder, DrawItem, PaintCommand,
    PaintStats, RasterImage, Rect, TextRun, dump_paint_commands, dump_paint_stats,
};
use crate::doc::{Document, DocumentParser, DocumentRenderNodeKind, DocumentStyleItem, ParseError};
use crate::history::{HistoryElementScroll, HistoryEntry, HistoryScrollState, SessionHistory};
use crate::layout_tree::{
    LayoutFragment, LayoutFragmentKind, LayoutNodeId, LayoutNodeKind, LayoutPosition, LayoutTree,
    apply_element_scrolls, apply_root_scroll, build_layout_tree, dump_layout_tree,
    layout_fragments_from_tree, line_boxes_from_tree,
};
use crate::line_layout::{LineBox, dump_line_boxes};
use crate::style_cascade::AuthorStylesheet;
use crate::style_dom::{AccessibilityElement, Selector};
use crate::whatwg_url::{parse as parse_url, parse_with_base as parse_url_with_base};

mod interaction;
pub use interaction::{FormSubmissionSnapshot, PageSelection};

const MAX_HISTORY_SCROLLPORTS: usize = 1024;

/// A loaded page at the current vertical integration boundary.
pub struct Page {
    url: String,
    document: Document,
    history: SessionHistory,
    csp: ContentSecurityPolicy,
    author_stylesheet: AuthorStylesheet,
    external_stylesheets: std::collections::BTreeMap<usize, String>,
    raster_images: std::collections::BTreeMap<usize, RasterImage>,
    diagnostics: Vec<EngineDiagnostic>,
    focused_element_node_id: Option<usize>,
    accessibility_mutation_epoch: u64,
    selection: Option<PageSelection>,
    text_selections: std::collections::HashMap<usize, AccessibilityTextSelection>,
    layout_viewport: (u32, u32),
    root_scroll: (f32, f32),
    element_scrolls: std::collections::HashMap<usize, (f32, f32)>,
    find_state: Option<FindState>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ElementScrollMetrics {
    pub position: (f32, f32),
    pub max: (f32, f32),
    pub user_scrollable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ElementScrollRequest {
    pub node_id: usize,
    pub element_id: Option<String>,
    pub tag: String,
    pub position: (f64, f64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FindState {
    query: String,
    case_sensitive: bool,
    active_match: usize,
}

#[derive(Debug, Clone)]
struct FindMatch {
    rect: Rect,
    fixed: bool,
    highlights: Vec<FindHighlight>,
}

#[derive(Debug, Clone, Copy)]
struct FindHighlight {
    rect: Rect,
    order: u32,
    clip: Option<Rect>,
}

const MAX_FIND_MATCHES: usize = 10_000;
const MAX_FIND_HIGHLIGHTS_PER_MATCH: usize = 64;

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
            external_stylesheets: std::collections::BTreeMap::new(),
            raster_images: std::collections::BTreeMap::new(),
            diagnostics: Vec::new(),
            focused_element_node_id: None,
            accessibility_mutation_epoch: 1,
            selection: None,
            text_selections: std::collections::HashMap::new(),
            layout_viewport: (800, 600),
            root_scroll: (0.0, 0.0),
            element_scrolls: std::collections::HashMap::new(),
            find_state: None,
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

    /// Clone session history after capturing this page's current bounded scroll
    /// state into the active entry.
    pub(crate) fn history_with_current_scroll(&self) -> SessionHistory {
        let mut history = self.history.clone();
        let mut element_offsets = self
            .element_scrolls
            .iter()
            .filter_map(|(node_id, offset)| {
                let element = self.document.element_by_node_id(*node_id)?;
                Some(HistoryElementScroll {
                    node_id: *node_id,
                    element_id: element.id,
                    tag: element.tag,
                    offset: *offset,
                })
            })
            .collect::<Vec<_>>();
        element_offsets.sort_by_key(|scroll| scroll.node_id);
        element_offsets.truncate(MAX_HISTORY_SCROLLPORTS);
        if self.root_scroll != (0.0, 0.0)
            || !element_offsets.is_empty()
            || history
                .current()
                .is_some_and(|entry| entry.scroll_state.is_some())
        {
            history.set_current_scroll_state(HistoryScrollState {
                root_offset: self.root_scroll,
                element_offsets,
            });
        }
        history
    }

    /// Replace the session-history model after a navigation/history host hook.
    /// The loaded URL is kept in sync with the current history entry.
    pub fn set_session_history(&mut self, history: SessionHistory) {
        let restoration = history.restoration_scroll_state().cloned();
        if let Some(url) = history.url() {
            self.url = url.to_owned();
        }
        self.history = history;
        if let Some(restoration) = restoration {
            self.scroll_root_to((
                f64::from(restoration.root_offset.0),
                f64::from(restoration.root_offset.1),
            ));
            if !self.element_scrolls.is_empty() {
                self.element_scrolls.clear();
                self.bump_accessibility_mutation_epoch();
            }
            self.scroll_elements_to(restoration.element_offsets.into_iter().map(|scroll| {
                ElementScrollRequest {
                    node_id: scroll.node_id,
                    element_id: scroll.element_id,
                    tag: scroll.tag,
                    position: (f64::from(scroll.offset.0), f64::from(scroll.offset.1)),
                }
            }));
        }
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
        self.text_selections.clear();
        self.element_scrolls.clear();
        self.bump_accessibility_mutation_epoch();
        self.refresh_author_stylesheet();
        self.clamp_scroll_offsets();
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
        self.clamp_scroll_offsets();
        Ok(())
    }

    /// Remove an element attribute in the authoritative Page DOM.
    pub fn remove_element_attribute(&mut self, node_id: usize, name: &str) -> Result<(), String> {
        self.document.remove_element_attribute(node_id, name)?;
        self.bump_accessibility_mutation_epoch();
        self.refresh_author_stylesheet();
        self.clamp_scroll_offsets();
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
        self.text_selections.clear();
        self.element_scrolls.clear();
        self.bump_accessibility_mutation_epoch();
        self.refresh_author_stylesheet();
        self.clamp_scroll_offsets();
        Ok(())
    }

    /// Persist UTF-16 selection offsets from the live native text-control
    /// runtime so the accessibility projection does not infer caret state.
    pub fn set_form_control_selection(
        &mut self,
        node_id: usize,
        element_id: Option<&str>,
        name: Option<&str>,
        tag: &str,
        base_offset: u32,
        extent_offset: u32,
    ) -> Result<(), String> {
        let node_id = self
            .resolve_form_control_node_id(node_id, element_id, name, tag)
            .ok_or_else(|| format!("native text control node {node_id} is missing"))?;
        if !matches!(tag, "input" | "textarea") {
            return Err("text selection target is not a native text control".to_owned());
        }
        let selection = AccessibilityTextSelection {
            base_offset,
            extent_offset,
        };
        if self.text_selections.get(&node_id) == Some(&selection) {
            return Ok(());
        }
        self.text_selections.insert(node_id, selection);
        self.bump_accessibility_mutation_epoch();
        Ok(())
    }

    /// Commit a focused contenteditable host's full text and UTF-16 selection
    /// from the live runtime without clearing focus during subtree replacement.
    pub fn set_contenteditable_text_state(
        &mut self,
        node_id: usize,
        value: &str,
        base_offset: u32,
        extent_offset: u32,
    ) -> Result<(), String> {
        let element = self
            .document
            .element_by_node_id(node_id)
            .ok_or_else(|| format!("contenteditable host node {node_id} is missing"))?;
        let editable = element.attributes.iter().any(|(name, value)| {
            name == "contenteditable"
                && !matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "false" | "inherit"
                )
        });
        if !editable || self.focused_element_node_id != Some(node_id) {
            return Err("text input target is not the focused contenteditable host".to_owned());
        }
        let utf16_len = value.encode_utf16().count();
        if base_offset as usize > utf16_len || extent_offset as usize > utf16_len {
            return Err("contenteditable selection exceeds the UTF-16 text length".to_owned());
        }

        let selection = AccessibilityTextSelection {
            base_offset,
            extent_offset,
        };
        let text_changed = self.document.element_text_content(node_id).as_deref() != Some(value);
        let selection_changed = self.text_selections.get(&node_id) != Some(&selection);
        if text_changed {
            self.document.set_element_text_content(node_id, value)?;
            self.refresh_author_stylesheet();
            self.clamp_scroll_offsets();
        }
        if selection_changed {
            self.text_selections.insert(node_id, selection);
        }
        if text_changed || selection_changed {
            self.bump_accessibility_mutation_epoch();
        }
        Ok(())
    }

    fn resolve_form_control_node_id(
        &self,
        node_id: usize,
        element_id: Option<&str>,
        name: Option<&str>,
        tag: &str,
    ) -> Option<usize> {
        let matches_identity = |element: &crate::style_dom::MatchedElement| {
            element.tag == tag
                && element_id.is_none_or(|id| element.id.as_deref() == Some(id))
                && name.is_none_or(|name| {
                    element
                        .attributes
                        .iter()
                        .any(|(attribute, value)| attribute == "name" && value == name)
                })
        };
        if let Some(element) = self.document.element_by_node_id(node_id)
            && matches_identity(&element)
        {
            return Some(node_id);
        }
        let selector = Selector::parse(tag).ok()?;
        self.document
            .query_all(&selector)
            .into_iter()
            .find(matches_identity)
            .map(|element| element.node_id)
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
        self.clamp_scroll_offsets();
        Ok(())
    }

    fn refresh_author_stylesheet(&mut self) {
        let blocks = self
            .document
            .style_execution_items()
            .into_iter()
            .filter_map(|item| match item {
                DocumentStyleItem::InlineStyle(source) => Some(source),
                DocumentStyleItem::ExternalStylesheet { index, .. } => {
                    self.external_stylesheets.get(&index).cloned()
                }
                DocumentStyleItem::CspMeta(_) => None,
            })
            .collect::<Vec<_>>();
        self.author_stylesheet = AuthorStylesheet::from_blocks(&blocks);
    }

    /// Apply one exact parser-discovered external stylesheet to the live
    /// cascade after BrowserCore has completed resource policy checks.
    pub(crate) fn apply_external_stylesheet(
        &mut self,
        index: usize,
        source: String,
    ) -> Result<(), String> {
        let known = self.document.style_execution_items().iter().any(|item| {
            matches!(
                item,
                DocumentStyleItem::ExternalStylesheet {
                    index: candidate,
                    ..
                } if *candidate == index
            )
        });
        if !known {
            return Err(format!(
                "external stylesheet {index} is not in the document"
            ));
        }
        self.external_stylesheets.insert(index, source);
        self.refresh_author_stylesheet();
        self.clamp_scroll_offsets();
        self.bump_accessibility_mutation_epoch();
        Ok(())
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
        self.find_text_matches(query, case_sensitive, (800, 600))
            .len() as u32
    }

    /// Select a visible-text match in document order and scroll the top-level
    /// viewport just enough to reveal it. Repeating the same query advances or
    /// reverses with wrapping; a changed query starts at the first/last match.
    pub fn find_text(
        &mut self,
        query: &str,
        case_sensitive: bool,
        forward: bool,
        viewport: (u32, u32),
    ) -> FindTextResult {
        if query.is_empty() {
            self.find_state = None;
            return FindTextResult {
                matches: 0,
                active_match: None,
            };
        }

        let matches = self.find_text_matches(query, case_sensitive, viewport);
        if matches.is_empty() {
            self.find_state = Some(FindState {
                query: query.to_owned(),
                case_sensitive,
                active_match: 0,
            });
            return FindTextResult {
                matches: 0,
                active_match: None,
            };
        }

        let same_query = self
            .find_state
            .as_ref()
            .is_some_and(|state| state.query == query && state.case_sensitive == case_sensitive);
        let active_match = if same_query {
            let current = self
                .find_state
                .as_ref()
                .map_or(0, |state| state.active_match.min(matches.len() - 1));
            if forward {
                (current + 1) % matches.len()
            } else {
                current.checked_sub(1).unwrap_or(matches.len() - 1)
            }
        } else if forward {
            0
        } else {
            matches.len() - 1
        };
        self.find_state = Some(FindState {
            query: query.to_owned(),
            case_sensitive,
            active_match,
        });
        let selected = &matches[active_match];
        if !selected.fixed {
            self.scroll_root_to_rect(viewport, selected.rect);
        }
        FindTextResult {
            matches: matches.len() as u32,
            active_match: Some(active_match as u32 + 1),
        }
    }

    /// Current top-level document scroll offset in layout pixels.
    pub fn root_scroll(&self) -> (f32, f32) {
        self.root_scroll
    }

    /// Browser-owned CSS layout viewport used by live script scrolling.
    pub(crate) fn layout_viewport(&self) -> (u32, u32) {
        self.layout_viewport
    }

    /// Update the live CSS viewport and clamp the retained root offset.
    pub(crate) fn set_layout_viewport(&mut self, viewport: (u32, u32)) {
        self.layout_viewport = viewport;
        self.clamp_scroll_offsets();
    }

    fn clamp_scroll_offsets(&mut self) {
        let viewport = self.layout_viewport;
        self.scroll_root_by(viewport, (0.0, 0.0));
        let tree = build_layout_tree(&self.document, viewport, |node_id| {
            self.layout_style_for_viewport(node_id, viewport)
        });
        let mut changed = false;
        self.element_scrolls.retain(|node_id, position| {
            let Some(limits) = element_scroll_limits_in_tree(&tree, *node_id) else {
                changed = true;
                return false;
            };
            let next = (
                position.0.clamp(0.0, limits.0),
                position.1.clamp(0.0, limits.1),
            );
            changed |= next != *position;
            *position = next;
            next != (0.0, 0.0)
        });
        if changed {
            self.bump_accessibility_mutation_epoch();
        }
    }

    /// Current maximum top-level scroll offset for the live CSS viewport.
    pub(crate) fn root_scroll_max(&self) -> (f32, f32) {
        self.root_scroll_limits(self.layout_viewport)
    }

    /// Apply an absolute script-driven top-level scroll request.
    pub(crate) fn scroll_root_to(&mut self, position: (f64, f64)) -> bool {
        if !position.0.is_finite() || !position.1.is_finite() {
            return false;
        }
        let (current_x, current_y) = self.root_scroll;
        self.scroll_root_by(
            self.layout_viewport,
            (
                position.0 - f64::from(current_x),
                position.1 - f64::from(current_y),
            ),
        )
    }

    /// Current position and maximum offset for a page-backed element
    /// scrollport. Non-scroll containers report a zero maximum.
    #[cfg(test)]
    pub(crate) fn element_scroll_metrics(&self, node_id: usize) -> Option<ElementScrollMetrics> {
        let tree = build_layout_tree(&self.document, self.layout_viewport, |node_id| {
            self.layout_style_for_viewport(node_id, self.layout_viewport)
        });
        let limits = element_scroll_limits_in_tree(&tree, node_id)?;
        let current = self
            .element_scrolls
            .get(&node_id)
            .copied()
            .unwrap_or((0.0, 0.0));
        Some(ElementScrollMetrics {
            position: (
                current.0.clamp(0.0, limits.0),
                current.1.clamp(0.0, limits.1),
            ),
            max: limits,
            user_scrollable: tree.nodes.iter().any(|node| {
                node.dom_node_id == Some(node_id) && node.style.overflow.user_scrollable()
            }),
        })
    }

    pub(crate) fn element_scroll_metrics_snapshot(
        &self,
    ) -> std::collections::HashMap<usize, ElementScrollMetrics> {
        let tree = build_layout_tree(&self.document, self.layout_viewport, |node_id| {
            self.layout_style_for_viewport(node_id, self.layout_viewport)
        });
        tree.nodes
            .iter()
            .filter(|node| node.style.overflow.clips_contents())
            .filter_map(|node| {
                let node_id = node.dom_node_id?;
                let limits = element_scroll_limits_in_tree(&tree, node_id).unwrap_or((0.0, 0.0));
                let current = self
                    .element_scrolls
                    .get(&node_id)
                    .copied()
                    .unwrap_or((0.0, 0.0));
                Some((
                    node_id,
                    ElementScrollMetrics {
                        position: (
                            current.0.clamp(0.0, limits.0),
                            current.1.clamp(0.0, limits.1),
                        ),
                        max: limits,
                        user_scrollable: node.style.overflow.user_scrollable(),
                    },
                ))
            })
            .collect()
    }

    /// Apply a bounded element scroll request. Returns whether paint, hit
    /// testing, or accessibility geometry changed.
    #[cfg(test)]
    pub(crate) fn scroll_element_to(&mut self, node_id: usize, position: (f64, f64)) -> bool {
        self.scroll_elements_to([ElementScrollRequest {
            node_id,
            element_id: None,
            tag: String::new(),
            position,
        }])
    }

    pub(crate) fn scroll_elements_to<I>(&mut self, requests: I) -> bool
    where
        I: IntoIterator<Item = ElementScrollRequest>,
    {
        let tree = build_layout_tree(&self.document, self.layout_viewport, |node_id| {
            self.layout_style_for_viewport(node_id, self.layout_viewport)
        });
        let mut changed = false;
        for request in requests {
            let Some(node_id) = self.resolve_element_scroll_node_id(&request) else {
                continue;
            };
            let position = request.position;
            if !position.0.is_finite() || !position.1.is_finite() {
                continue;
            }
            let Some(limits) = element_scroll_limits_in_tree(&tree, node_id) else {
                continue;
            };
            let current = self
                .element_scrolls
                .get(&node_id)
                .copied()
                .unwrap_or((0.0, 0.0));
            let next = (
                position.0.clamp(0.0, f64::from(limits.0)) as f32,
                position.1.clamp(0.0, f64::from(limits.1)) as f32,
            );
            if next == current {
                continue;
            }
            changed = true;
            if next == (0.0, 0.0) {
                self.element_scrolls.remove(&node_id);
            } else {
                self.element_scrolls.insert(node_id, next);
            }
        }
        if changed {
            self.bump_accessibility_mutation_epoch();
        }
        changed
    }

    fn resolve_element_scroll_node_id(&self, request: &ElementScrollRequest) -> Option<usize> {
        let matches_identity = |element: &crate::style_dom::MatchedElement| {
            (request.tag.is_empty() || element.tag == request.tag)
                && request
                    .element_id
                    .as_deref()
                    .is_none_or(|id| element.id.as_deref() == Some(id))
        };
        if let Some(element) = self.document.element_by_node_id(request.node_id)
            && matches_identity(&element)
        {
            return Some(request.node_id);
        }
        let id = request.element_id.as_deref()?;
        let selector = Selector::parse(&request.tag).ok()?;
        self.document
            .query_all(&selector)
            .into_iter()
            .find(|element| matches_identity(element) && element.id.as_deref() == Some(id))
            .map(|element| element.node_id)
    }

    /// Whether the focused live element owns navigation-key defaults that must
    /// not fall through to top-level document scrolling.
    pub fn focused_element_consumes_scroll_keys(&self) -> bool {
        let Some(node_id) = self.focused_element_node_id else {
            return false;
        };
        let Some(element) = self.document.element_by_node_id(node_id) else {
            return false;
        };
        matches!(
            element.tag.as_str(),
            "button" | "input" | "select" | "textarea"
        ) || element
            .attributes
            .iter()
            .any(|(name, value)| name == "contenteditable" && !value.eq_ignore_ascii_case("false"))
    }

    /// Apply a bounded top-level default scroll action. Returns whether the
    /// visible projection changed.
    pub fn scroll_root_by(&mut self, viewport: (u32, u32), delta: (f64, f64)) -> bool {
        if !delta.0.is_finite() || !delta.1.is_finite() {
            return false;
        }
        let limits = self.root_scroll_limits(viewport);
        let next = (
            (f64::from(self.root_scroll.0) + delta.0).clamp(0.0, f64::from(limits.0)) as f32,
            (f64::from(self.root_scroll.1) + delta.1).clamp(0.0, f64::from(limits.1)) as f32,
        );
        if next == self.root_scroll {
            return false;
        }
        self.root_scroll = next;
        self.bump_accessibility_mutation_epoch();
        true
    }

    fn scroll_root_to_rect(&mut self, viewport: (u32, u32), rect: Rect) -> bool {
        let (current_x, current_y) = self.root_scroll;
        let viewport_width = viewport.0 as f32;
        let viewport_height = viewport.1 as f32;
        let target_x = if rect.x < current_x {
            rect.x
        } else if rect.x + rect.w > current_x + viewport_width {
            rect.x + rect.w - viewport_width
        } else {
            current_x
        };
        let target_y = if rect.y < current_y {
            rect.y
        } else if rect.y + rect.h > current_y + viewport_height {
            rect.y + rect.h - viewport_height
        } else {
            current_y
        };
        self.scroll_root_by(
            viewport,
            (
                f64::from(target_x - current_x),
                f64::from(target_y - current_y),
            ),
        )
    }

    fn find_text_matches(
        &self,
        query: &str,
        case_sensitive: bool,
        viewport: (u32, u32),
    ) -> Vec<FindMatch> {
        if query.is_empty() {
            return Vec::new();
        }
        let tree = build_layout_tree(&self.document, viewport, |node_id| {
            self.layout_style_for_viewport(node_id, viewport)
        });
        let mut fixed_subtree = vec![false; tree.nodes.len()];
        for node in &tree.nodes {
            let parent_fixed = node
                .parent
                .is_some_and(|parent| fixed_subtree[parent.index()]);
            let fixed = parent_fixed || node.style.position == LayoutPosition::Fixed;
            fixed_subtree[node.id.index()] = fixed;
        }
        find_matches_in_tree(&tree, query, case_sensitive, |node_id| {
            fixed_subtree[node_id.index()]
        })
    }

    /// Fallback event target for wheel input over unpainted viewport space.
    pub fn default_pointer_event_target_node_id(&self) -> Option<usize> {
        self.query_selector_all("body")
            .ok()
            .and_then(|matches| matches.into_iter().next())
            .or_else(|| {
                self.query_selector_all("html")
                    .ok()
                    .and_then(|matches| matches.into_iter().next())
            })
            .map(|element| element.node_id)
    }

    fn root_scroll_limits(&self, viewport: (u32, u32)) -> (f32, f32) {
        let tree = build_layout_tree(&self.document, viewport, |node_id| {
            self.layout_style_for_viewport(node_id, viewport)
        });
        let mut fixed_subtree = vec![false; tree.nodes.len()];
        let mut right = viewport.0 as f32;
        let mut bottom = viewport.1 as f32;
        for node in &tree.nodes {
            if node.id == tree.root {
                continue;
            }
            let parent_fixed = node
                .parent
                .is_some_and(|parent| fixed_subtree[parent.index()]);
            let fixed = parent_fixed || node.style.position == LayoutPosition::Fixed;
            fixed_subtree[node.id.index()] = fixed;
            if fixed {
                continue;
            }
            if has_clipping_ancestor(&tree, node.id, None) {
                continue;
            }
            right = right.max(node.boxes.margin.x + node.boxes.margin.w);
            bottom = bottom.max(node.boxes.margin.y + node.boxes.margin.h);
        }
        (
            (right - viewport.0 as f32).max(0.0),
            (bottom - viewport.1 as f32).max(0.0),
        )
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
        let mut tree = build_layout_tree(&self.document, viewport, |node_id| {
            self.layout_style_for_viewport(node_id, viewport)
        });
        let offsets = self
            .element_scrolls
            .iter()
            .filter_map(|(node_id, position)| {
                let limits = element_scroll_limits_in_tree(&tree, *node_id)?;
                let position = (
                    position.0.clamp(0.0, limits.0),
                    position.1.clamp(0.0, limits.1),
                );
                (position != (0.0, 0.0)).then_some((*node_id, position))
            })
            .collect::<std::collections::HashMap<_, _>>();
        apply_element_scrolls(&mut tree, |node_id| offsets.get(&node_id).copied());
        apply_root_scroll(&mut tree, self.root_scroll);
        tree
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
        let tree = self.layout_tree(viewport);
        let fragments = layout_fragments_from_tree(&tree);
        if let Some(find) = self.find_state.as_ref()
            && !find.query.is_empty()
        {
            let matches = find_matches_in_tree(&tree, &find.query, find.case_sensitive, |_| false);
            let active_match = find.active_match.min(matches.len().saturating_sub(1));
            for (index, found) in matches.into_iter().enumerate() {
                for highlight in found.highlights {
                    builder.push(find_highlight_draw_item(highlight, index == active_match));
                }
            }
        }
        for fragment in fragments {
            if matches!(fragment.kind, LayoutFragmentKind::Image) {
                if let Some(image) = fragment
                    .dom_node_id
                    .and_then(|node_id| self.raster_images.get(&node_id))
                {
                    builder.push(image_draw_item(&fragment, image.clone()));
                }
            } else {
                builder.push(fragment_draw_item(&fragment));
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
            root_scroll: self.root_scroll,
            root_scroll_max: self.root_scroll_max(),
        }
    }

    pub fn render_snapshot(
        &self,
        context_id: BrowsingContextId,
        document_id: DocumentId,
        viewport: (u32, u32),
        viewport_generation: u64,
        page_zoom: f64,
    ) -> Result<FullRenderSnapshot, String> {
        let accessibility = self.accessibility_snapshot(context_id, document_id, viewport);
        let semantics = accessibility
            .nodes
            .into_iter()
            .filter(|node| !node.hidden && node.id != 0)
            .map(|node| (node.id, node))
            .collect::<std::collections::HashMap<_, _>>();
        let action_generation = accessibility.generation;
        let mut next_text_id = i64::MAX as u64;
        let mut nodes = Vec::new();
        for projected in self.document.render_nodes() {
            let (id, kind, styles, resource_ids, semantic) = match projected.kind {
                DocumentRenderNodeKind::Element {
                    node_id,
                    local_name,
                } => {
                    let id = RenderNodeId::new(
                        u64::try_from(node_id)
                            .map_err(|_| "DOM node id exceeds the renderer id range".to_owned())?,
                    )
                    .ok_or_else(|| "DOM node id must be nonzero".to_owned())?;
                    let styles = self
                        .layout_style_for_viewport(node_id, viewport)
                        .into_iter()
                        .map(|(name, value)| RenderStyleProperty { name, value })
                        .collect::<Vec<_>>();
                    let resource_ids = self
                        .raster_images
                        .contains_key(&node_id)
                        .then_some(
                            RenderResourceId::new(id.get())
                                .expect("nonzero render node id is a resource id"),
                        )
                        .into_iter()
                        .collect::<Vec<_>>();
                    let semantic = semantics.get(&node_id).and_then(|source| {
                        let semantic_id = SemanticNodeId::new(u64::try_from(source.id).ok()?)?;
                        Some(RenderSemanticNode {
                            id: semantic_id,
                            role: source.role.clone(),
                            name: source.label.clone(),
                            value: source.value.clone(),
                            action_generation,
                            actions: source
                                .actions
                                .iter()
                                .filter_map(|action| match action.as_str() {
                                    "tap" => Some(RenderSemanticActionKind::Activate),
                                    "focus" => Some(RenderSemanticActionKind::Focus),
                                    "set_value" => Some(RenderSemanticActionKind::SetValue),
                                    "increase" => Some(RenderSemanticActionKind::Increase),
                                    "decrease" => Some(RenderSemanticActionKind::Decrease),
                                    _ => None,
                                })
                                .collect(),
                        })
                    });
                    (
                        id,
                        RenderNodeKind::Element { local_name },
                        styles,
                        resource_ids,
                        semantic,
                    )
                }
                DocumentRenderNodeKind::Text { text } => {
                    while RenderNodeId::new(next_text_id).is_none() {
                        next_text_id = next_text_id
                            .checked_sub(1)
                            .ok_or_else(|| "renderer text node ids exhausted".to_owned())?;
                    }
                    let id = RenderNodeId::new(next_text_id).expect("text id checked");
                    next_text_id = next_text_id
                        .checked_sub(1)
                        .ok_or_else(|| "renderer text node ids exhausted".to_owned())?;
                    let styles = projected
                        .parent_element_node_id
                        .map(|parent_id| self.layout_style_for_viewport(parent_id, viewport))
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(name, value)| RenderStyleProperty { name, value })
                        .collect();
                    (id, RenderNodeKind::Text { text }, styles, Vec::new(), None)
                }
            };
            let parent_id = projected
                .parent_element_node_id
                .map(|parent_id| {
                    u64::try_from(parent_id)
                        .ok()
                        .and_then(RenderNodeId::new)
                        .ok_or_else(|| "renderer parent node id is invalid".to_owned())
                })
                .transpose()?;
            nodes.push(RenderNode {
                id,
                parent_id,
                sibling_index: projected.sibling_index,
                depth: projected.depth,
                kind,
                styles,
                resource_ids,
                semantic,
            });
        }
        let mut resources = Vec::new();
        for (node_id, image) in &self.raster_images {
            let resource_id =
                RenderResourceId::new(u64::try_from(*node_id).map_err(|_| {
                    "image node id exceeds the renderer resource id range".to_owned()
                })?)
                .ok_or_else(|| "image resource id must be nonzero".to_owned())?;
            resources.push(RenderResource {
                id: resource_id,
                kind: RenderResourceKind::Image,
                mime: "image/png".to_owned(),
                bytes: encode_render_png(image)?,
            });
        }
        let root_id = nodes
            .iter()
            .find(|node| node.parent_id.is_none())
            .map(|node| node.id)
            .ok_or_else(|| "renderer document has no root element".to_owned())?;
        let generation = self.accessibility_mutation_epoch.max(1);
        let snapshot = FullRenderSnapshot {
            version: vixen_api::RENDER_PROTOCOL_VERSION,
            revision: RenderRevision {
                context_id,
                document_id,
                source_generation: generation,
                style_generation: generation,
                viewport_generation,
                resource_generation: generation,
            },
            viewport: RenderViewport {
                width: viewport.0,
                height: viewport.1,
                device_scale: 1.0,
                page_zoom,
            },
            nodes,
            resources,
            scroll_intents: vec![RenderScrollIntent {
                scroll_node_id: RenderScrollNodeId::new(1).expect("constant scroll id"),
                node_id: root_id,
                kind: RenderScrollIntentKind::To(RenderPoint {
                    x: f64::from(self.root_scroll.0) * page_zoom,
                    y: f64::from(self.root_scroll.1) * page_zoom,
                }),
            }],
        };
        snapshot.validate().map_err(|error| error.to_string())?;
        Ok(snapshot)
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
                if aria_mixed(element.aria_checked.as_deref()) {
                    Some(false)
                } else {
                    aria_bool(element.aria_checked.as_deref())
                }
            } else {
                matches!(role.as_str(), "checkbox" | "radio").then_some(element.checked)
            };
            let mixed = aria_mixed(element.aria_checked.as_deref()).then_some(true);
            let selected = aria_bool(element.aria_selected.as_deref()).unwrap_or(element.selected);
            let expanded = aria_bool(element.aria_expanded.as_deref());
            let value = accessibility_value(&element, &role);
            let range = accessibility_range(&element, &role);
            let text_selection = (self.focused_element_node_id == Some(element.node_id)
                && matches!(role.as_str(), "textbox" | "searchbox"))
            .then(|| self.text_selections.get(&element.node_id).copied())
            .flatten();
            let multiline = matches!(role.as_str(), "textbox" | "searchbox")
                && (element.tag == "textarea" || element.contenteditable);
            let writable_text = !disabled && accessibility_set_value_supported(&element, &role);
            let text_input_type =
                writable_text.then(|| accessibility_text_input_type(&element, multiline));
            let text_input_action =
                writable_text.then(|| accessibility_text_input_action(&element, &role, multiline));
            let live_region = accessibility_live_region(&element, &role);
            let heading_level = accessibility_heading_level(&element, &role);
            let mut actions = Vec::new();
            if !disabled && matches!(role.as_str(), "button" | "link" | "checkbox" | "radio") {
                actions.push("tap".to_owned());
            }
            if focusable {
                actions.push("focus".to_owned());
            }
            if writable_text {
                actions.push("set_value".to_owned());
            }
            if !disabled && range.is_some() {
                actions.push("increase".to_owned());
                actions.push("decrease".to_owned());
            }
            nodes.push(AccessibilityNode {
                id: element.node_id,
                parent_id: element.parent_id,
                controls_ids: element.controls_ids,
                described_by_ids: element.described_by_ids,
                details_ids: element.details_ids,
                owns_ids: element.owns_ids,
                role,
                label: element.label,
                description: element.description,
                value,
                text_selection,
                multiline,
                text_input_type,
                text_input_action,
                range,
                bbox: bounds.get(&element.node_id).copied(),
                focused: self.focused_element_node_id == Some(element.node_id),
                disabled,
                checked,
                mixed,
                selected,
                expanded,
                heading_level,
                hidden: false,
                live_region,
                focusable,
                actions,
            });
        }
        let emitted_ids = nodes
            .iter()
            .map(|node| node.id)
            .collect::<std::collections::HashSet<_>>();
        for node in &mut nodes {
            node.controls_ids
                .retain(|target| emitted_ids.contains(target));
            node.described_by_ids
                .retain(|target| emitted_ids.contains(target));
            node.details_ids
                .retain(|target| emitted_ids.contains(target));
            node.owns_ids.retain(|target| emitted_ids.contains(target));
        }
        let node_indexes = nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.id, index))
            .collect::<std::collections::HashMap<_, _>>();
        let mut claimed = std::collections::HashSet::new();
        let ownership = nodes
            .iter()
            .flat_map(|owner| {
                owner
                    .owns_ids
                    .iter()
                    .copied()
                    .map(move |target| (owner.id, target))
            })
            .collect::<Vec<_>>();
        for (owner_id, target_id) in ownership {
            let (Some(&owner_index), Some(&target_index)) =
                (node_indexes.get(&owner_id), node_indexes.get(&target_id))
            else {
                continue;
            };
            if owner_index < target_index && claimed.insert(target_id) {
                nodes[target_index].parent_id = Some(owner_id);
            }
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

    fn layout_style_for_viewport(
        &self,
        node_id: usize,
        viewport: (u32, u32),
    ) -> Vec<(String, String)> {
        let mut styles = self.computed_style_for_viewport(node_id, viewport);
        if let Some(image) = self.raster_images.get(&node_id) {
            if !styles.iter().any(|(property, _)| property == "width") {
                styles.push(("width".to_owned(), format!("{}px", image.width)));
            }
            if !styles.iter().any(|(property, _)| property == "height") {
                styles.push(("height".to_owned(), format!("{}px", image.height)));
            }
        }
        styles
    }

    pub(crate) fn apply_raster_image(&mut self, node_id: usize, image: RasterImage) {
        self.raster_images.insert(node_id, image);
        self.bump_accessibility_mutation_epoch();
    }

    /// Diagnostics accumulated by pipeline stages.
    pub fn diagnostics(&self) -> Vec<EngineDiagnostic> {
        self.diagnostics.clone()
    }
}

fn encode_render_png(image: &RasterImage) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|error| format!("renderer PNG header failed: {error}"))?;
    writer
        .write_image_data(image.rgba.as_slice())
        .map_err(|error| format!("renderer PNG encoding failed: {error}"))?;
    writer
        .finish()
        .map_err(|error| format!("renderer PNG finish failed: {error}"))?;
    Ok(bytes)
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

fn element_scroll_limits_in_tree(tree: &LayoutTree, node_id: usize) -> Option<(f32, f32)> {
    let scrollport = tree
        .nodes
        .iter()
        .find(|node| node.dom_node_id == Some(node_id))?;
    if !scrollport.style.overflow.programmatically_scrollable() {
        return Some((0.0, 0.0));
    }

    let padding = scrollport.boxes.padding;
    let mut right = padding.x + padding.w;
    let mut bottom = padding.y + padding.h;
    for node in &tree.nodes {
        if node.id == scrollport.id || !is_descendant_of(tree, node.id, scrollport.id) {
            continue;
        }
        if has_clipping_ancestor(tree, node.id, Some(scrollport.id)) {
            continue;
        }
        right = right.max(node.boxes.margin.x + node.boxes.margin.w);
        bottom = bottom.max(node.boxes.margin.y + node.boxes.margin.h);
    }
    Some((
        (right - (padding.x + padding.w)).max(0.0),
        (bottom - (padding.y + padding.h)).max(0.0),
    ))
}

fn is_descendant_of(tree: &LayoutTree, node_id: LayoutNodeId, ancestor_id: LayoutNodeId) -> bool {
    let mut parent = tree.node(node_id).parent;
    while let Some(parent_id) = parent {
        if parent_id == ancestor_id {
            return true;
        }
        parent = tree.node(parent_id).parent;
    }
    false
}

fn has_clipping_ancestor(
    tree: &LayoutTree,
    node_id: LayoutNodeId,
    stop_at: Option<LayoutNodeId>,
) -> bool {
    let mut parent = tree.node(node_id).parent;
    while let Some(parent_id) = parent {
        if Some(parent_id) == stop_at {
            return false;
        }
        let parent_node = tree.node(parent_id);
        if parent_node.style.overflow.clips_contents() {
            return true;
        }
        parent = parent_node.parent;
    }
    false
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
        image: None,
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
            image: None,
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
            image: None,
            text: Some(TextRun {
                color: *color,
                text: text.clone(),
            }),
        },
        LayoutFragmentKind::Image => unreachable!("image fragments use image_draw_item"),
    }
}

fn image_draw_item(fragment: &LayoutFragment, image: RasterImage) -> DrawItem {
    DrawItem {
        order: fragment.order,
        z_index: 0,
        visibility: crate::display_list::Visibility::Visible,
        opacity: 1.0,
        clip: fragment.clip,
        is_viewport_background: false,
        border_box: fragment.rect,
        padding_box: fragment.rect,
        content_box: fragment.rect,
        background_clip: BackgroundBox::ContentBox,
        background_origin: BackgroundBox::ContentBox,
        background_attachment: BackgroundAttachment::Scroll,
        background: None,
        image: Some(image),
        text: None,
    }
}

fn find_highlight_draw_item(found: FindHighlight, active: bool) -> DrawItem {
    DrawItem {
        order: found.order,
        z_index: 0,
        visibility: crate::display_list::Visibility::Visible,
        opacity: 1.0,
        clip: found.clip,
        is_viewport_background: false,
        border_box: found.rect,
        padding_box: found.rect,
        content_box: found.rect,
        background_clip: BackgroundBox::BorderBox,
        background_origin: BackgroundBox::ContentBox,
        background_attachment: BackgroundAttachment::Scroll,
        background: Some(if active {
            Color::rgb(255, 152, 0)
        } else {
            Color::rgb(255, 235, 59)
        }),
        image: None,
        text: None,
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
        _ if element.contenteditable => "textbox",
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
    if element.contenteditable {
        return Some(element.text.clone());
    }
    element
        .aria_value_text
        .clone()
        .or_else(|| element.aria_value_now.clone())
        .or_else(|| element.value.clone())
        .or_else(|| {
            matches!(element.tag.as_str(), "textarea" | "select")
                .then(|| element.text.clone())
                .filter(|value| !value.is_empty())
        })
}

fn accessibility_range(element: &AccessibilityElement, role: &str) -> Option<AccessibilityRange> {
    if element.read_only {
        return None;
    }
    let native_range = element.tag == "input"
        && element
            .input_type
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case("range"))
        && role == "slider";
    let authored_current = element
        .aria_value_now
        .as_deref()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite());
    let authored_range = matches!(role, "slider" | "spinbutton")
        && element.role.as_deref() == Some(role)
        && authored_current.is_some()
        && accessibility_focusable(element);
    if !native_range && !authored_range {
        return None;
    }
    let minimum = (if authored_range {
        element.aria_value_min.as_deref()
    } else {
        element.minimum.as_deref()
    })
    .and_then(|value| value.parse::<f64>().ok())
    .filter(|value| value.is_finite())
    .unwrap_or(0.0);
    let maximum = (if authored_range {
        element.aria_value_max.as_deref()
    } else {
        element.maximum.as_deref()
    })
    .and_then(|value| value.parse::<f64>().ok())
    .filter(|value| value.is_finite() && *value >= minimum)
    .unwrap_or(100.0_f64.max(minimum));
    let step = if authored_range {
        1.0
    } else {
        element
            .step
            .as_deref()
            .filter(|value| !value.eq_ignore_ascii_case("any"))
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(1.0)
    };
    let current = (if authored_range {
        authored_current
    } else {
        element
            .value
            .as_deref()
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite())
    })
    .unwrap_or(minimum + (maximum - minimum) / 2.0)
    .clamp(minimum, maximum);
    Some(AccessibilityRange {
        current,
        minimum,
        maximum,
        step,
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

fn accessibility_set_value_supported(element: &AccessibilityElement, role: &str) -> bool {
    if element.read_only || !matches!(role, "textbox" | "searchbox") {
        return false;
    }
    if element.contenteditable || element.tag == "textarea" {
        return true;
    }
    if element.tag != "input" {
        return false;
    }
    matches!(
        element
            .input_type
            .as_deref()
            .unwrap_or("text")
            .to_ascii_lowercase()
            .as_str(),
        "text" | "search" | "url" | "tel" | "email"
    )
}

fn accessibility_text_input_type(
    element: &AccessibilityElement,
    multiline: bool,
) -> AccessibilityTextInputType {
    match element
        .input_mode
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "none" => AccessibilityTextInputType::None,
        "text" => AccessibilityTextInputType::Text,
        "decimal" => AccessibilityTextInputType::Decimal,
        "numeric" => AccessibilityTextInputType::Number,
        "tel" => AccessibilityTextInputType::Telephone,
        "search" => AccessibilityTextInputType::Search,
        "email" => AccessibilityTextInputType::Email,
        "url" => AccessibilityTextInputType::Url,
        _ if multiline => AccessibilityTextInputType::Multiline,
        _ => match element
            .input_type
            .as_deref()
            .map(str::trim)
            .unwrap_or("text")
            .to_ascii_lowercase()
            .as_str()
        {
            "search" => AccessibilityTextInputType::Search,
            "email" => AccessibilityTextInputType::Email,
            "url" => AccessibilityTextInputType::Url,
            "tel" => AccessibilityTextInputType::Telephone,
            _ => AccessibilityTextInputType::Text,
        },
    }
}

fn accessibility_text_input_action(
    element: &AccessibilityElement,
    role: &str,
    multiline: bool,
) -> AccessibilityTextInputAction {
    match element
        .enter_key_hint
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "enter" => AccessibilityTextInputAction::Newline,
        "done" => AccessibilityTextInputAction::Done,
        "go" => AccessibilityTextInputAction::Go,
        "next" => AccessibilityTextInputAction::Next,
        "previous" => AccessibilityTextInputAction::Previous,
        "search" => AccessibilityTextInputAction::Search,
        "send" => AccessibilityTextInputAction::Send,
        _ if multiline => AccessibilityTextInputAction::Newline,
        _ if role == "searchbox" => AccessibilityTextInputAction::Search,
        _ => AccessibilityTextInputAction::Done,
    }
}

fn accessibility_live_region(element: &AccessibilityElement, role: &str) -> bool {
    if let Some(value) = element.aria_live.as_deref() {
        return matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "polite" | "assertive"
        );
    }
    matches!(role, "alert" | "log" | "marquee" | "status" | "timer")
}

fn accessibility_heading_level(element: &AccessibilityElement, role: &str) -> Option<u8> {
    if role != "heading" {
        return None;
    }
    element
        .aria_level
        .as_deref()
        .and_then(|value| value.parse::<u8>().ok())
        .filter(|level| (1..=6).contains(level))
        .or_else(|| {
            element
                .tag
                .strip_prefix('h')
                .and_then(|value| value.parse::<u8>().ok())
                .filter(|level| (1..=6).contains(level))
        })
}

fn aria_bool(value: Option<&str>) -> Option<bool> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn aria_mixed(value: Option<&str>) -> bool {
    value.is_some_and(|value| value.trim().eq_ignore_ascii_case("mixed"))
}

fn text_match_ranges(haystack: &str, needle: &str, case_sensitive: bool) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    let (search_haystack, character_map) = if case_sensitive {
        (
            haystack.to_owned(),
            (0..haystack.chars().count()).collect::<Vec<_>>(),
        )
    } else {
        fold_case_with_character_map(haystack)
    };
    let search_needle = if case_sensitive {
        needle.to_owned()
    } else {
        needle.to_lowercase()
    };
    if search_needle.is_empty() {
        return Vec::new();
    }
    let byte_starts = character_byte_starts(&search_haystack);
    search_haystack
        .match_indices(&search_needle)
        .take(MAX_FIND_MATCHES)
        .filter_map(|(start, matched)| {
            let start_index = byte_starts.binary_search(&start).ok()?;
            let end_index = byte_starts.binary_search(&(start + matched.len())).ok()?;
            let original_start = *character_map.get(start_index)?;
            let original_end = character_map.get(end_index.checked_sub(1)?)? + 1;
            Some((original_start, original_end))
        })
        .collect()
}

fn fold_case_with_character_map(value: &str) -> (String, Vec<usize>) {
    let mut folded = String::new();
    let mut map = Vec::new();
    for (index, character) in value.chars().enumerate() {
        for folded_character in character.to_lowercase() {
            folded.push(folded_character);
            map.push(index);
        }
    }
    (folded, map)
}

fn character_byte_starts(value: &str) -> Vec<usize> {
    value
        .char_indices()
        .map(|(offset, _)| offset)
        .chain(std::iter::once(value.len()))
        .collect()
}

fn find_matches_in_tree(
    tree: &LayoutTree,
    query: &str,
    case_sensitive: bool,
    is_fixed: impl Fn(crate::layout_tree::LayoutNodeId) -> bool,
) -> Vec<FindMatch> {
    let fragments = layout_fragments_from_tree(tree);
    let mut fragments_by_node = std::collections::HashMap::<_, Vec<_>>::new();
    for fragment in &fragments {
        if matches!(fragment.kind, LayoutFragmentKind::Text { .. }) {
            fragments_by_node
                .entry(fragment.node_id)
                .or_default()
                .push(fragment);
        }
    }
    let mut matches = Vec::new();
    for node in &tree.nodes {
        if node.kind != LayoutNodeKind::Text {
            continue;
        }
        let Some(text) = node.text.as_deref() else {
            continue;
        };
        let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let Some(node_fragments) = fragments_by_node.get(&node.id) else {
            continue;
        };
        let line_ranges = find_line_ranges(&normalized, node_fragments);
        for (match_start, match_end) in text_match_ranges(&normalized, query, case_sensitive) {
            let mut highlights = Vec::new();
            for (fragment, line_start, line_end) in &line_ranges {
                let start = match_start.max(*line_start);
                let end = match_end.min(*line_end);
                if start >= end {
                    continue;
                }
                let LayoutFragmentKind::Text { text, .. } = &fragment.kind else {
                    continue;
                };
                let character_width = fragment.rect.w / text.chars().count().max(1) as f32;
                highlights.push(FindHighlight {
                    rect: Rect::new(
                        fragment.rect.x + (start - *line_start) as f32 * character_width,
                        fragment.rect.y,
                        ((end - start) as f32 * character_width).max(1.0),
                        fragment.rect.h,
                    ),
                    order: fragment.order,
                    clip: fragment.clip,
                });
                if highlights.len() == MAX_FIND_HIGHLIGHTS_PER_MATCH {
                    break;
                }
            }
            let Some(first) = highlights.first() else {
                continue;
            };
            matches.push(FindMatch {
                rect: first.rect,
                fixed: is_fixed(node.id),
                highlights,
            });
            if matches.len() == MAX_FIND_MATCHES {
                return matches;
            }
        }
    }
    matches
}

fn find_line_ranges<'a>(
    normalized: &str,
    fragments: &[&'a LayoutFragment],
) -> Vec<(&'a LayoutFragment, usize, usize)> {
    let byte_starts = character_byte_starts(normalized);
    let mut cursor = 0;
    let mut ranges = Vec::new();
    for fragment in fragments {
        let LayoutFragmentKind::Text { text, .. } = &fragment.kind else {
            continue;
        };
        let Some(relative_start) = normalized[cursor..].find(text) else {
            continue;
        };
        let start_byte = cursor + relative_start;
        let end_byte = start_byte + text.len();
        let Ok(start) = byte_starts.binary_search(&start_byte) else {
            continue;
        };
        let Ok(end) = byte_starts.binary_search(&end_byte) else {
            continue;
        };
        ranges.push((*fragment, start, end));
        cursor = end_byte;
    }
    ranges
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
    fn render_snapshot_projects_full_styled_dom_with_stable_element_ids() {
        let page = Page::from_html(
            "file:///render.html",
            r#"<!doctype html><style>
                body { margin: 0; }
                #hit { display:block; width:120px; height:40px; background:#22bb66; }
            </style><button id="hit">Hit me</button><p>After</p>"#,
        )
        .unwrap();
        let button_id = page.query_selector_all("#hit").unwrap()[0].node_id;
        let snapshot = page
            .render_snapshot(
                BrowsingContextId::new(1).unwrap(),
                DocumentId::new(2).unwrap(),
                (320, 240),
                3,
                1.0,
            )
            .unwrap();
        snapshot.validate().unwrap();

        let button = snapshot
            .nodes
            .iter()
            .find(|node| node.id.get() == button_id as u64)
            .unwrap();
        assert!(matches!(
            &button.kind,
            RenderNodeKind::Element { local_name } if local_name == "button"
        ));
        assert!(
            button
                .styles
                .iter()
                .any(|style| style.name == "width" && style.value == "120px")
        );
        assert!(button.semantic.is_some());
        assert!(snapshot.nodes.iter().any(|node| {
            node.parent_id == Some(button.id)
                && matches!(&node.kind, RenderNodeKind::Text { text } if text == "Hit me")
        }));
        assert_eq!(snapshot.revision.viewport_generation, 3);
        assert_eq!(snapshot.viewport.width, 320);
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
        assert_eq!(checkbox.actions, ["tap", "focus"]);

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
    fn accessibility_snapshot_normalizes_text_input_keyboard_and_action_hints() {
        let page = Page::from_html(
            "file:///text-input-hints.html",
            r#"<!doctype html>
                <input aria-label="none" inputmode="none" enterkeyhint="enter">
                <input aria-label="number" inputmode="numeric" enterkeyhint="done">
                <input aria-label="decimal" inputmode="decimal" enterkeyhint="go">
                <input aria-label="telephone" inputmode="tel" enterkeyhint="next">
                <input aria-label="email" inputmode="email" enterkeyhint="previous">
                <input aria-label="url" inputmode="url" enterkeyhint="search">
                <input aria-label="search" inputmode="search" enterkeyhint="send">
                <input aria-label="typed search" type="search">
                <textarea aria-label="multiline"></textarea>"#,
        )
        .unwrap();
        let snapshot = page.accessibility_snapshot(
            BrowsingContextId::new(7).unwrap(),
            DocumentId::new(9).unwrap(),
            (320, 480),
        );
        let hint = |label: &str| {
            let node = snapshot
                .nodes
                .iter()
                .find(|node| node.label == label)
                .unwrap();
            (
                node.text_input_type.unwrap(),
                node.text_input_action.unwrap(),
            )
        };
        assert_eq!(
            hint("none"),
            (
                AccessibilityTextInputType::None,
                AccessibilityTextInputAction::Newline
            )
        );
        assert_eq!(
            hint("number"),
            (
                AccessibilityTextInputType::Number,
                AccessibilityTextInputAction::Done
            )
        );
        assert_eq!(
            hint("decimal"),
            (
                AccessibilityTextInputType::Decimal,
                AccessibilityTextInputAction::Go
            )
        );
        assert_eq!(
            hint("telephone"),
            (
                AccessibilityTextInputType::Telephone,
                AccessibilityTextInputAction::Next
            )
        );
        assert_eq!(
            hint("email"),
            (
                AccessibilityTextInputType::Email,
                AccessibilityTextInputAction::Previous
            )
        );
        assert_eq!(
            hint("url"),
            (
                AccessibilityTextInputType::Url,
                AccessibilityTextInputAction::Search
            )
        );
        assert_eq!(
            hint("search"),
            (
                AccessibilityTextInputType::Search,
                AccessibilityTextInputAction::Send
            )
        );
        assert_eq!(
            hint("typed search"),
            (
                AccessibilityTextInputType::Search,
                AccessibilityTextInputAction::Search
            )
        );
        assert_eq!(
            hint("multiline"),
            (
                AccessibilityTextInputType::Multiline,
                AccessibilityTextInputAction::Newline
            )
        );
    }

    #[test]
    fn accessibility_snapshot_projects_nearest_semantic_parent_hierarchy() {
        let page = Page::from_html(
            "file:///accessibility-hierarchy.html",
            r#"<!doctype html>
                <main><div><ul><li><button>Run</button></li></ul></div></main>
                <p>Independent</p>"#,
        )
        .unwrap();
        let snapshot = page.accessibility_snapshot(
            BrowsingContextId::new(1).unwrap(),
            DocumentId::new(1).unwrap(),
            (800, 600),
        );
        let main = snapshot
            .nodes
            .iter()
            .find(|node| node.role == "main")
            .unwrap();
        let list = snapshot
            .nodes
            .iter()
            .find(|node| node.role == "list")
            .unwrap();
        let item = snapshot
            .nodes
            .iter()
            .find(|node| node.role == "listitem")
            .unwrap();
        let button = snapshot
            .nodes
            .iter()
            .find(|node| node.role == "button")
            .unwrap();
        let independent = snapshot
            .nodes
            .iter()
            .find(|node| node.label == "Independent")
            .unwrap();

        assert_eq!(main.parent_id, None);
        assert_eq!(list.parent_id, Some(main.id));
        assert_eq!(item.parent_id, Some(list.id));
        assert_eq!(button.parent_id, Some(item.id));
        assert_eq!(independent.parent_id, None);
        assert!(
            snapshot
                .nodes
                .iter()
                .all(|node| { node.parent_id.is_none_or(|parent_id| parent_id < node.id) })
        );
    }

    #[test]
    fn accessibility_snapshot_maps_owned_nodes_headings_and_mixed_state() {
        let page = Page::from_html(
            "file:///accessibility-platform-mappings.html",
            r#"<!doctype html>
                <section role="group" aria-label="Palette" aria-owns="blue missing red blue"></section>
                <button id="red">Red</button>
                <button id="blue">Blue</button>
                <h2>Native heading</h2>
                <div role="heading" aria-level="4">Authored heading</div>
                <div role="checkbox" aria-label="Some selected" aria-checked="mixed" tabindex="0"></div>"#,
        )
        .unwrap();
        let snapshot = page.accessibility_snapshot(
            BrowsingContextId::new(1).unwrap(),
            DocumentId::new(1).unwrap(),
            (800, 600),
        );
        let palette = snapshot
            .nodes
            .iter()
            .find(|node| node.label == "Palette")
            .unwrap();
        let red = snapshot
            .nodes
            .iter()
            .find(|node| node.label == "Red")
            .unwrap();
        let blue = snapshot
            .nodes
            .iter()
            .find(|node| node.label == "Blue")
            .unwrap();
        assert_eq!(palette.owns_ids, [blue.id, red.id]);
        assert_eq!(red.parent_id, Some(palette.id));
        assert_eq!(blue.parent_id, Some(palette.id));
        assert_eq!(
            snapshot
                .nodes
                .iter()
                .find(|node| node.label == "Native heading")
                .unwrap()
                .heading_level,
            Some(2)
        );
        assert_eq!(
            snapshot
                .nodes
                .iter()
                .find(|node| node.label == "Authored heading")
                .unwrap()
                .heading_level,
            Some(4)
        );
        let mixed = snapshot
            .nodes
            .iter()
            .find(|node| node.label == "Some selected")
            .unwrap();
        assert_eq!(mixed.checked, Some(false));
        assert_eq!(mixed.mixed, Some(true));
    }

    #[test]
    fn accessibility_snapshot_projects_controls_relationships_and_native_range() {
        let page = Page::from_html(
            "file:///accessibility-controls.html",
            r#"<!doctype html>
                <button aria-controls="panel missing panel">Toggle</button>
                <section id="panel" aria-label="Panel">Content</section>
                <input type="range" aria-label="Volume" min="10" max="20" step="2" value="14">
                <div id="hidden" hidden>Hidden target</div>"#,
        )
        .unwrap();
        let snapshot = page.accessibility_snapshot(
            BrowsingContextId::new(1).unwrap(),
            DocumentId::new(1).unwrap(),
            (800, 600),
        );
        let panel = snapshot
            .nodes
            .iter()
            .find(|node| node.label == "Panel")
            .unwrap();
        let toggle = snapshot
            .nodes
            .iter()
            .find(|node| node.label == "Toggle")
            .unwrap();
        assert_eq!(toggle.controls_ids, [panel.id]);

        let volume = snapshot
            .nodes
            .iter()
            .find(|node| node.label == "Volume")
            .unwrap();
        assert_eq!(volume.role, "slider");
        assert_eq!(
            volume.range,
            Some(AccessibilityRange {
                current: 14.0,
                minimum: 10.0,
                maximum: 20.0,
                step: 2.0,
            })
        );
        assert_eq!(volume.actions, ["focus", "increase", "decrease"]);
    }

    #[test]
    fn accessibility_snapshot_projects_authored_range_state_conservatively() {
        let page = Page::from_html(
            "file:///accessibility-authored-range.html",
            r#"<!doctype html>
                <div role="slider" tabindex="0" aria-label="Brightness" aria-valuemin="10" aria-valuemax="20" aria-valuenow="14" aria-valuetext="Medium"></div>
                <div role="spinbutton" tabindex="0" aria-label="Guests" aria-valuenow="3"></div>
                <div role="slider" tabindex="0" aria-label="Missing value"></div>
                <div role="slider" tabindex="0" aria-label="Invalid value" aria-valuenow="many"></div>"#,
        )
        .unwrap();
        let snapshot = page.accessibility_snapshot(
            BrowsingContextId::new(1).unwrap(),
            DocumentId::new(1).unwrap(),
            (800, 600),
        );
        let brightness = snapshot
            .nodes
            .iter()
            .find(|node| node.label == "Brightness")
            .unwrap();
        assert_eq!(brightness.value.as_deref(), Some("Medium"));
        assert_eq!(
            brightness.range,
            Some(AccessibilityRange {
                current: 14.0,
                minimum: 10.0,
                maximum: 20.0,
                step: 1.0,
            })
        );
        assert_eq!(brightness.actions, ["focus", "increase", "decrease"]);
        let guests = snapshot
            .nodes
            .iter()
            .find(|node| node.label == "Guests")
            .unwrap();
        assert_eq!(guests.value.as_deref(), Some("3"));
        assert_eq!(guests.range.unwrap().current, 3.0);
        for label in ["Missing value", "Invalid value"] {
            let node = snapshot
                .nodes
                .iter()
                .find(|node| node.label == label)
                .unwrap();
            assert!(node.range.is_none());
            assert_eq!(node.actions, ["focus"]);
        }
    }

    #[test]
    fn accessibility_snapshot_projects_descriptions_and_detail_relationships() {
        let page = Page::from_html(
            "file:///accessibility-descriptions.html",
            r#"<!doctype html>
                <button aria-label="Save" aria-describedby="help hidden help" aria-details="details missing" title="Fallback title">Go</button>
                <p id="help">Writes the document</p>
                <p id="hidden" hidden>Hidden description</p>
                <section id="details" aria-label="Save details">Extended help</section>
                <button aria-label="Share" aria-description="Sends a private link"></button>
                <button aria-label="Print" title="Opens the print dialog"></button>"#,
        )
        .unwrap();
        let snapshot = page.accessibility_snapshot(
            BrowsingContextId::new(1).unwrap(),
            DocumentId::new(1).unwrap(),
            (800, 600),
        );
        let save = snapshot
            .nodes
            .iter()
            .find(|node| node.label == "Save")
            .unwrap();
        let help = snapshot
            .nodes
            .iter()
            .find(|node| node.label == "Writes the document")
            .unwrap();
        let details = snapshot
            .nodes
            .iter()
            .find(|node| node.label == "Save details")
            .unwrap();
        assert_eq!(save.description, "Writes the document Hidden description");
        assert_eq!(save.described_by_ids, [help.id]);
        assert_eq!(save.details_ids, [details.id]);
        assert_eq!(
            snapshot
                .nodes
                .iter()
                .find(|node| node.label == "Share")
                .unwrap()
                .description,
            "Sends a private link"
        );
        assert_eq!(
            snapshot
                .nodes
                .iter()
                .find(|node| node.label == "Print")
                .unwrap()
                .description,
            "Opens the print dialog"
        );
    }

    #[test]
    fn accessibility_snapshot_projects_explicit_and_implicit_live_regions() {
        let page = Page::from_html(
            "file:///accessibility-live.html",
            r#"<!doctype html>
                <p aria-live="polite">Saved</p>
                <div role="alert">Connection lost</div>
                <div role="status" aria-live="off">Idle</div>
                <p aria-live="invalid">Ignored</p>"#,
        )
        .unwrap();
        let snapshot = page.accessibility_snapshot(
            BrowsingContextId::new(1).unwrap(),
            DocumentId::new(1).unwrap(),
            (800, 600),
        );
        let live = |label: &str| {
            snapshot
                .nodes
                .iter()
                .find(|node| node.label == label)
                .unwrap()
                .live_region
        };
        assert!(live("Saved"));
        assert!(live("Connection lost"));
        assert!(!live("Idle"));
        assert!(!live("Ignored"));
    }

    #[test]
    fn accessibility_set_value_is_limited_to_writable_text_hosts() {
        let page = Page::from_html(
            "file:///accessibility-values.html",
            r#"<!doctype html>
                <input aria-label="Name">
                <textarea aria-label="Notes"></textarea>
                <input aria-label="Read only" readonly>
                <input aria-label="Secret" type="password">
                <div contenteditable aria-label="Editor">draft</div>
                <div role="textbox" aria-label="Authored"></div>"#,
        )
        .unwrap();
        let snapshot = page.accessibility_snapshot(
            BrowsingContextId::new(1).unwrap(),
            DocumentId::new(1).unwrap(),
            (800, 600),
        );
        let supports_set_value = |label: &str| {
            snapshot
                .nodes
                .iter()
                .find(|node| node.label == label)
                .unwrap()
                .actions
                .iter()
                .any(|action| action == "set_value")
        };
        assert!(supports_set_value("Name"));
        assert!(supports_set_value("Notes"));
        assert!(!supports_set_value("Read only"));
        assert!(!supports_set_value("Secret"));
        assert!(supports_set_value("Editor"));
        assert!(!supports_set_value("Authored"));
    }

    #[test]
    fn accessibility_snapshot_projects_only_focused_native_control_selection() {
        let mut page = Page::from_html(
            "file:///accessibility-selection.html",
            "<input id='name' aria-label='Name' value='Vixen'><div role='textbox' aria-label='Authored'></div>",
        )
        .unwrap();
        let input_id = page.query_selector_all("#name").unwrap()[0].node_id;
        page.set_form_control_selection(input_id, Some("name"), None, "input", 1, 4)
            .unwrap();
        let context_id = BrowsingContextId::new(1).unwrap();
        let document_id = DocumentId::new(1).unwrap();
        let unfocused = page.accessibility_snapshot(context_id, document_id, (800, 600));
        assert!(
            unfocused
                .nodes
                .iter()
                .all(|node| node.text_selection.is_none())
        );

        page.set_focused_element_node_id(Some(input_id));
        let focused = page.accessibility_snapshot(context_id, document_id, (800, 600));
        assert_eq!(
            focused
                .nodes
                .iter()
                .find(|node| node.label == "Name")
                .unwrap()
                .text_selection,
            Some(AccessibilityTextSelection {
                base_offset: 1,
                extent_offset: 4,
            })
        );
        assert!(
            focused
                .nodes
                .iter()
                .find(|node| node.label == "Authored")
                .unwrap()
                .text_selection
                .is_none()
        );
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
        let mut page = Page::from_html(
            "file:///fixture.html",
            "<html><head><title>Rust hidden</title><style>.gone{display:none}#space{height:600px}</style></head><body><p>Rust rust rustic</p><p hidden>Rust hidden</p><p class='gone'>Rust gone</p><div id='space'></div><p>Trust Rust</p></body></html>",
        )
        .unwrap();

        assert_eq!(page.find_text_count("Rust", true), 2);
        assert_eq!(page.find_text_count("rust", false), 5);
        assert_eq!(page.find_text_count("hidden", false), 0);
        assert_eq!(page.find_text_count("", false), 0);

        let viewport = (200, 100);
        let first = page.find_text("Rust", true, true, viewport);
        assert_eq!(first.matches, 2);
        assert_eq!(first.active_match, Some(1));
        let first_scroll = page.root_scroll();

        let second = page.find_text("Rust", true, true, viewport);
        assert_eq!(second.active_match, Some(2));
        let second_scroll = page.root_scroll().1;
        assert!(second_scroll > first_scroll.1);

        let wrapped = page.find_text("Rust", true, true, viewport);
        assert_eq!(wrapped.active_match, Some(1));
        let wrapped_scroll = page.root_scroll();
        assert!(wrapped_scroll.1 < second_scroll);

        let reversed = page.find_text("Rust", true, false, viewport);
        assert_eq!(reversed.active_match, Some(2));
        assert!(page.root_scroll().1 > wrapped_scroll.1);

        assert_eq!(
            page.find_text("", false, true, viewport),
            FindTextResult {
                matches: 0,
                active_match: None,
            }
        );
    }

    #[test]
    fn find_highlights_all_visible_matches_and_distinguishes_active_match() {
        let mut page = Page::from_html(
            "file:///find.html",
            "<style>body{margin:0}</style><p>Rust rust</p>",
        )
        .unwrap();
        let viewport = (300, 100);
        let active_color = Color::rgb(255, 152, 0);
        let other_color = Color::rgb(255, 235, 59);
        let highlight_rects = |page: &Page| {
            page.display_list(viewport)
                .into_iter()
                .filter_map(|command| match command {
                    PaintCommand::Background { fill, color, .. }
                        if color == active_color || color == other_color =>
                    {
                        Some((fill, color))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };

        assert_eq!(
            page.find_text("rust", false, true, viewport).active_match,
            Some(1)
        );
        let first = highlight_rects(&page);
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].1, active_color);
        assert_eq!(first[1].1, other_color);
        assert_eq!(first[0].0.w, 32.0);
        assert!(first[0].0.x < first[1].0.x);

        assert_eq!(
            page.find_text("rust", false, true, viewport).active_match,
            Some(2)
        );
        let second = highlight_rects(&page);
        assert_eq!(second[0].1, other_color);
        assert_eq!(second[1].1, active_color);

        page.find_text("", false, true, viewport);
        assert!(highlight_rects(&page).is_empty());

        let mut wrapped = Page::from_html(
            "file:///wrapped-find.html",
            "<style>body{margin:0}</style><p>alpha beta</p>",
        )
        .unwrap();
        let narrow_viewport = (60, 100);
        assert_eq!(
            wrapped.find_text("alpha beta", false, true, narrow_viewport),
            FindTextResult {
                matches: 1,
                active_match: Some(1),
            }
        );
        assert_eq!(
            wrapped
                .display_list(narrow_viewport)
                .into_iter()
                .filter(|command| matches!(
                    command,
                    PaintCommand::Background { color, .. } if *color == active_color
                ))
                .count(),
            2
        );
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
    fn root_scroll_is_bounded_and_moves_document_but_not_fixed_content() {
        let mut page = Page::from_html(
            "file:///scroll.html",
            "<style>body{margin:0}#spacer{height:600px}#fixed{position:fixed;top:4px;height:20px}</style><div id='spacer'>Top</div><button id='bottom'>Bottom</button><div id='fixed'>Fixed</div>",
        )
        .unwrap();
        let viewport = (200, 100);
        let before_bottom = page
            .query_selector_all_in_viewport("#bottom", viewport)
            .unwrap()[0]
            .bbox
            .unwrap();
        let before_fixed = page
            .query_selector_all_in_viewport("#fixed", viewport)
            .unwrap()[0]
            .bbox
            .unwrap();

        assert!(page.scroll_root_by(viewport, (0.0, 250.0)));
        assert_eq!(page.root_scroll(), (0.0, 250.0));
        let after_bottom = page
            .query_selector_all_in_viewport("#bottom", viewport)
            .unwrap()[0]
            .bbox
            .unwrap();
        let after_fixed = page
            .query_selector_all_in_viewport("#fixed", viewport)
            .unwrap()[0]
            .bbox
            .unwrap();
        assert_eq!(after_bottom.1, before_bottom.1 - 250.0);
        assert_eq!(after_fixed, before_fixed);

        assert!(page.scroll_root_by(viewport, (0.0, f64::MAX)));
        let bottom_limit = page.root_scroll().1;
        assert!(bottom_limit > 250.0);
        assert!(!page.scroll_root_by(viewport, (0.0, f64::MAX)));
        assert!(!page.scroll_root_by(viewport, (0.0, f64::NAN)));
    }

    #[test]
    fn nested_scroll_is_bounded_moves_content_and_does_not_expand_root() {
        let mut page = Page::from_html(
            "file:///nested-scroll.html",
            r#"
                <style>
                  html, body { margin: 0; }
                  #scroll { width: 100px; height: 60px; overflow: auto; }
                  #content { height: 220px; }
                </style>
                <div id="scroll"><div id="content">Scrollable</div></div>
            "#,
        )
        .unwrap();
        let viewport = (300, 200);
        page.set_layout_viewport(viewport);
        let scroll_id = page.query_selector_all("#scroll").unwrap()[0].node_id;
        let before = page.query_selector_all("#content").unwrap()[0]
            .bbox
            .unwrap();
        let metrics = page.element_scroll_metrics(scroll_id).unwrap();
        assert_eq!(metrics.position, (0.0, 0.0));
        assert_eq!(metrics.max.0, 0.0);
        assert!(metrics.max.1 >= 160.0, "{metrics:?}");
        assert!(metrics.user_scrollable);
        assert_eq!(page.root_scroll_max(), (0.0, 0.0));

        assert!(page.scroll_element_to(scroll_id, (0.0, 45.0)));
        assert_eq!(
            page.element_scroll_metrics(scroll_id).unwrap().position,
            (0.0, 45.0)
        );
        let tree = page.layout_tree(viewport);
        let content = tree
            .nodes
            .iter()
            .find(|node| {
                node.dom_node_id == Some(page.query_selector_all("#content").unwrap()[0].node_id)
            })
            .unwrap();
        assert_eq!(content.rect.y as f64, before.1 - 45.0);

        assert!(page.scroll_element_to(scroll_id, (0.0, f64::MAX)));
        assert_eq!(
            page.element_scroll_metrics(scroll_id).unwrap().position.1,
            metrics.max.1
        );
        assert!(!page.scroll_element_to(scroll_id, (0.0, f64::MAX)));
        assert!(!page.scroll_element_to(scroll_id, (0.0, f64::NAN)));
    }

    #[test]
    fn focused_editing_controls_consume_scroll_keys() {
        let mut page = Page::from_html(
            "file:///controls.html",
            "<input id='field'><div id='editor' contenteditable>edit</div><a id='link'>link</a>",
        )
        .unwrap();
        let input = page.query_selector_all("#field").unwrap()[0].node_id;
        let editor = page.query_selector_all("#editor").unwrap()[0].node_id;
        let link = page.query_selector_all("#link").unwrap()[0].node_id;

        assert!(!page.focused_element_consumes_scroll_keys());
        page.set_focused_element_node_id(Some(input));
        assert!(page.focused_element_consumes_scroll_keys());
        page.set_focused_element_node_id(Some(editor));
        assert!(page.focused_element_consumes_scroll_keys());
        page.set_focused_element_node_id(Some(link));
        assert!(!page.focused_element_consumes_scroll_keys());
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
                LayoutFragmentKind::Background { .. } | LayoutFragmentKind::Image => None,
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
