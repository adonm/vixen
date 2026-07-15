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
    AccessibilitySnapshot, AccessibilityTextInputAction, AccessibilityTextInputType,
    AccessibilityTextSelection, BrowsingContextId, DocumentId, ElementInfo, EngineDiagnostic,
    FindTextResult, FullRenderSnapshot, PageSnapshot, RenderCommit, RenderNode, RenderNodeId,
    RenderNodeKind, RenderPoint, RenderResource, RenderResourceId, RenderResourceKind,
    RenderRevision, RenderScrollIntent, RenderScrollIntentKind, RenderScrollNodeId,
    RenderSemanticActionKind, RenderSemanticNode, RenderStyleProperty, RenderViewport,
    SemanticNodeId,
};
use vixen_net::csp::ContentSecurityPolicy;

use crate::doc::{Document, DocumentParser, DocumentRenderNodeKind, DocumentStyleItem, ParseError};
use crate::history::{HistoryElementScroll, HistoryEntry, HistoryScrollState, SessionHistory};
use crate::raster_image::RasterImage;
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
    external_stylesheets: std::collections::BTreeMap<usize, String>,
    raster_images: std::collections::BTreeMap<usize, RasterImage>,
    diagnostics: Vec<EngineDiagnostic>,
    focused_element_node_id: Option<usize>,
    accessibility_mutation_epoch: u64,
    selection: Option<PageSelection>,
    text_selections: std::collections::HashMap<usize, AccessibilityTextSelection>,
    layout_viewport: (u32, u32),
    root_scroll: (f32, f32),
    root_scroll_intent: (f32, f32),
    root_scroll_max: (f32, f32),
    renderer_scroll_known: bool,
    element_scrolls: std::collections::HashMap<usize, ElementScrollState>,
    find_state: Option<FindState>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ElementScrollState {
    scroll_node_id: RenderScrollNodeId,
    position: (f32, f32),
    intent: Option<(f64, f64)>,
    max: (f32, f32),
    user_scrollable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ElementScrollProjection {
    pub node_id: usize,
    pub element_id: Option<String>,
    pub tag: String,
    pub position: (f32, f32),
    pub max: (f32, f32),
    pub user_scrollable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FindState {
    query: String,
    case_sensitive: bool,
    active_match: usize,
}

const MAX_FIND_MATCHES: usize = 10_000;

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
            root_scroll_intent: (0.0, 0.0),
            root_scroll_max: (0.0, 0.0),
            renderer_scroll_known: false,
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
            .element_scroll_state_snapshot()
            .into_iter()
            .filter(|scroll| scroll.position != (0.0, 0.0))
            .map(|scroll| HistoryElementScroll {
                node_id: scroll.node_id,
                element_id: scroll.element_id,
                tag: scroll.tag,
                offset: scroll.position,
            })
            .collect::<Vec<_>>();
        element_offsets.sort_by_key(|scroll| scroll.node_id);
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
            for scroll in restoration.element_offsets {
                self.set_element_scroll(
                    scroll.node_id,
                    scroll.element_id.as_deref(),
                    &scroll.tag,
                    (f64::from(scroll.offset.0), f64::from(scroll.offset.1)),
                );
            }
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
        self.bump_accessibility_mutation_epoch();
        self.refresh_author_stylesheet();
        self.renderer_scroll_known = false;
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
        self.renderer_scroll_known = false;
        Ok(())
    }

    /// Remove an element attribute in the authoritative Page DOM.
    pub fn remove_element_attribute(&mut self, node_id: usize, name: &str) -> Result<(), String> {
        self.document.remove_element_attribute(node_id, name)?;
        self.bump_accessibility_mutation_epoch();
        self.refresh_author_stylesheet();
        self.renderer_scroll_known = false;
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
        self.bump_accessibility_mutation_epoch();
        self.refresh_author_stylesheet();
        self.renderer_scroll_known = false;
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
            self.renderer_scroll_known = false;
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
        self.renderer_scroll_known = false;
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
        self.renderer_scroll_known = false;
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
        self.find_text_match_count(query, case_sensitive) as u32
    }

    /// Select a visible-text match in document order. Repeating the same query
    /// advances or reverses with wrapping; Flutter reveals the selected match
    /// from exact commit-bound Paragraph geometry.
    pub fn find_text(
        &mut self,
        query: &str,
        case_sensitive: bool,
        forward: bool,
    ) -> FindTextResult {
        if query.is_empty() {
            self.find_state = None;
            return FindTextResult {
                matches: 0,
                active_match: None,
            };
        }

        let matches = self.find_text_match_count(query, case_sensitive);
        if matches == 0 {
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
                .map_or(0, |state| state.active_match.min(matches - 1));
            if forward {
                (current + 1) % matches
            } else {
                current.checked_sub(1).unwrap_or(matches - 1)
            }
        } else if forward {
            0
        } else {
            matches - 1
        };
        self.find_state = Some(FindState {
            query: query.to_owned(),
            case_sensitive,
            active_match,
        });
        FindTextResult {
            matches: matches as u32,
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

    pub(crate) fn renderer_source_generation(&self) -> u64 {
        self.accessibility_mutation_epoch.max(1)
    }

    /// Update the live CSS viewport. Flutter recomputes and commits exact scroll
    /// limits for this source viewport.
    pub(crate) fn set_layout_viewport(&mut self, viewport: (u32, u32)) {
        if self.layout_viewport != viewport {
            self.layout_viewport = viewport;
            self.renderer_scroll_known = false;
        }
    }

    /// Maximum top-level offset from the most recent exact Flutter commit.
    pub(crate) fn root_scroll_max(&self) -> (f32, f32) {
        self.root_scroll_max
    }

    /// Apply an absolute script-driven top-level scroll intent. The formatter
    /// owns exact clamping and reports the accepted offset in its next commit.
    pub(crate) fn scroll_root_to(&mut self, position: (f64, f64)) -> bool {
        if !position.0.is_finite() || !position.1.is_finite() {
            return false;
        }
        let limit = vixen_api::RENDER_MAX_COORDINATE;
        let next = (
            position.0.clamp(0.0, limit) as f32,
            position.1.clamp(0.0, limit) as f32,
        );
        if next == self.root_scroll {
            return false;
        }
        self.root_scroll = next;
        self.root_scroll_intent = next;
        self.bump_accessibility_mutation_epoch();
        true
    }

    pub(crate) fn set_element_scroll(
        &mut self,
        node_id: usize,
        element_id: Option<&str>,
        tag: &str,
        position: (f64, f64),
    ) -> bool {
        if !position.0.is_finite() || !position.1.is_finite() {
            return false;
        }
        let Some(element) = self.document.element_by_node_id(node_id) else {
            return false;
        };
        if element.tag != tag || element.id.as_deref() != element_id {
            return false;
        }
        let Some(state) = self.element_scrolls.get_mut(&node_id) else {
            return false;
        };
        let limit = vixen_api::RENDER_MAX_COORDINATE;
        let next = (
            position.0.clamp(0.0, limit) as f32,
            position.1.clamp(0.0, limit) as f32,
        );
        if state.position == next {
            return false;
        }
        state.position = next;
        state.intent = Some((position.0.clamp(0.0, limit), position.1.clamp(0.0, limit)));
        self.bump_accessibility_mutation_epoch();
        true
    }

    pub(crate) fn element_scroll_state_snapshot(&self) -> Vec<ElementScrollProjection> {
        let mut states = self
            .element_scrolls
            .iter()
            .filter_map(|(node_id, state)| {
                let element = self.document.element_by_node_id(*node_id)?;
                Some(ElementScrollProjection {
                    node_id: *node_id,
                    element_id: element.id,
                    tag: element.tag,
                    position: state.position,
                    max: state.max,
                    user_scrollable: state.user_scrollable,
                })
            })
            .collect::<Vec<_>>();
        states.sort_by_key(|state| state.node_id);
        states
    }

    /// Accept mechanical scroll state only from the formatter commit that owns
    /// the current geometry. No Rust layout estimate participates.
    pub(crate) fn apply_renderer_scroll(&mut self, commit: &RenderCommit) {
        let output_scale = commit.viewport.device_scale * commit.viewport.page_zoom;
        let mut source_changed = false;
        let root = commit
            .scroll_snapshot
            .iter()
            .find(|scroll| scroll.scroll_node_id.get() == 1);
        let Some(root) = root else {
            self.root_scroll = (0.0, 0.0);
            self.root_scroll_max = (0.0, 0.0);
            self.renderer_scroll_known = true;
            return;
        };
        self.root_scroll = (
            (root.offset.x / output_scale) as f32,
            (root.offset.y / output_scale) as f32,
        );
        if self.root_scroll_intent != self.root_scroll {
            self.root_scroll_intent = self.root_scroll;
            source_changed = true;
        }
        self.root_scroll_max = (
            (root.max_offset.x / output_scale) as f32,
            (root.max_offset.y / output_scale) as f32,
        );
        let mut element_scrolls = std::collections::HashMap::new();
        for scroll in &commit.scroll_snapshot {
            if scroll.scroll_node_id.get() == 1 {
                continue;
            }
            let Ok(node_id) = usize::try_from(scroll.node_id.get()) else {
                continue;
            };
            if self.document.element_by_node_id(node_id).is_none() {
                continue;
            }
            let styles = self.computed_style_for_viewport(node_id, self.layout_viewport);
            let user_scrollable = ["overflow", "overflow-x", "overflow-y"]
                .iter()
                .filter_map(|name| {
                    styles
                        .iter()
                        .find(|(candidate, _)| candidate == name)
                        .map(|(_, value)| value.to_ascii_lowercase())
                })
                .any(|value| {
                    value
                        .split_ascii_whitespace()
                        .any(|keyword| matches!(keyword, "auto" | "scroll"))
                });
            let previous = self
                .element_scrolls
                .get(&node_id)
                .filter(|state| state.scroll_node_id == scroll.scroll_node_id);
            let exact_position = (
                scroll.offset.x / output_scale,
                scroll.offset.y / output_scale,
            );
            let position = (exact_position.0 as f32, exact_position.1 as f32);
            let mut intent = previous.and_then(|state| state.intent);
            if intent.is_some_and(|requested| requested != exact_position)
                || (intent.is_none() && position != (0.0, 0.0))
            {
                intent = Some(exact_position);
                source_changed = true;
            }
            element_scrolls.insert(
                node_id,
                ElementScrollState {
                    scroll_node_id: scroll.scroll_node_id,
                    position,
                    intent,
                    max: (
                        (scroll.max_offset.x / output_scale) as f32,
                        (scroll.max_offset.y / output_scale) as f32,
                    ),
                    user_scrollable,
                },
            );
        }
        if self.element_scrolls.iter().any(|(node_id, state)| {
            state.intent.is_some() && !element_scrolls.contains_key(node_id)
        }) {
            source_changed = true;
        }
        self.element_scrolls = element_scrolls;
        self.renderer_scroll_known = true;
        if source_changed {
            self.bump_accessibility_mutation_epoch();
        }
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

    /// Apply a top-level scroll intent. If an exact formatter commit is known,
    /// clamp to it; otherwise stay within the renderer protocol bound until the
    /// formatter returns authoritative mechanical state.
    pub fn scroll_root_by(&mut self, _viewport: (u32, u32), delta: (f64, f64)) -> bool {
        if !delta.0.is_finite() || !delta.1.is_finite() {
            return false;
        }
        let limits = if self.renderer_scroll_known {
            (
                f64::from(self.root_scroll_max.0),
                f64::from(self.root_scroll_max.1),
            )
        } else {
            (
                vixen_api::RENDER_MAX_COORDINATE,
                vixen_api::RENDER_MAX_COORDINATE,
            )
        };
        let next = (
            (f64::from(self.root_scroll.0) + delta.0).clamp(0.0, limits.0) as f32,
            (f64::from(self.root_scroll.1) + delta.1).clamp(0.0, limits.1) as f32,
        );
        if next == self.root_scroll {
            return false;
        }
        self.root_scroll = next;
        self.root_scroll_intent = next;
        self.bump_accessibility_mutation_epoch();
        true
    }

    fn find_text_match_count(&self, query: &str, case_sensitive: bool) -> usize {
        if query.is_empty() {
            return 0;
        }
        let folded_query = (!case_sensitive).then(|| query.to_lowercase());
        let mut hidden = std::collections::HashSet::new();
        let mut matches = 0usize;
        for node in self.document.render_nodes() {
            match node.kind {
                DocumentRenderNodeKind::Element {
                    node_id,
                    local_name,
                } => {
                    let parent_hidden = node
                        .parent_element_node_id
                        .is_some_and(|parent| hidden.contains(&parent));
                    let intrinsically_hidden = matches!(
                        local_name.as_str(),
                        "head" | "title" | "style" | "script" | "template"
                    ) || self
                        .document
                        .element_by_node_id(node_id)
                        .is_some_and(|element| {
                            element.attributes.iter().any(|(name, _)| name == "hidden")
                        });
                    let style_hidden = self
                        .computed_style_for_viewport(node_id, self.layout_viewport)
                        .iter()
                        .any(|(name, value)| {
                            (name == "display" && value.eq_ignore_ascii_case("none"))
                                || (name == "visibility"
                                    && matches!(
                                        value.to_ascii_lowercase().as_str(),
                                        "hidden" | "collapse"
                                    ))
                        });
                    if parent_hidden || intrinsically_hidden || style_hidden {
                        hidden.insert(node_id);
                    }
                }
                DocumentRenderNodeKind::Text { text }
                    if !node
                        .parent_element_node_id
                        .is_some_and(|parent| hidden.contains(&parent)) =>
                {
                    let count = if let Some(query) = folded_query.as_deref() {
                        text.to_lowercase().match_indices(query).count()
                    } else {
                        text.match_indices(query).count()
                    };
                    matches = matches.saturating_add(count).min(MAX_FIND_MATCHES);
                    if matches == MAX_FIND_MATCHES {
                        break;
                    }
                }
                DocumentRenderNodeKind::Text { .. } => {}
            }
        }
        matches
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
        device_scale: f64,
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
                        .renderer_style_for_viewport(node_id, self.layout_viewport)
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
                        .map(|parent_id| {
                            self.renderer_style_for_viewport(parent_id, self.layout_viewport)
                        })
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
        let output_scale = device_scale * page_zoom;
        let mut scroll_intents = vec![RenderScrollIntent {
            scroll_node_id: RenderScrollNodeId::new(1).expect("constant scroll id"),
            node_id: root_id,
            kind: RenderScrollIntentKind::To(RenderPoint {
                x: f64::from(self.root_scroll_intent.0) * output_scale,
                y: f64::from(self.root_scroll_intent.1) * output_scale,
            }),
        }];
        let render_node_ids = nodes
            .iter()
            .map(|node| node.id)
            .collect::<std::collections::HashSet<_>>();
        let mut element_scrolls = self.element_scrolls.iter().collect::<Vec<_>>();
        element_scrolls.sort_by_key(|(node_id, _)| **node_id);
        for (node_id, scroll) in element_scrolls {
            let Some(intent) = scroll.intent else {
                continue;
            };
            let Some(render_node_id) = u64::try_from(*node_id).ok().and_then(RenderNodeId::new)
            else {
                continue;
            };
            if !render_node_ids.contains(&render_node_id) {
                continue;
            }
            scroll_intents.push(RenderScrollIntent {
                scroll_node_id: scroll.scroll_node_id,
                node_id: render_node_id,
                kind: RenderScrollIntentKind::To(RenderPoint {
                    x: intent.0 * output_scale,
                    y: intent.1 * output_scale,
                }),
            });
        }
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
                device_scale,
                page_zoom,
            },
            nodes,
            resources,
            scroll_intents,
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
        let (elements, truncated) = self.document.accessibility_elements(
            ACCESSIBILITY_MAX_NODES,
            ACCESSIBILITY_MAX_STRING_BYTES,
            |node_id| {
                !self
                    .computed_style_for_viewport(node_id, self.layout_viewport)
                    .iter()
                    .any(|(name, value)| {
                        (name == "display" && value.eq_ignore_ascii_case("none"))
                            || (name == "visibility"
                                && matches!(
                                    value.to_ascii_lowercase().as_str(),
                                    "hidden" | "collapse"
                                ))
                    })
            },
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
                bbox: None,
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

    /// Query selector facade. Geometry belongs to an exact Flutter commit and
    /// is attached by rendered CDP/automation callers, not fabricated here.
    pub fn query_selector_all_in_viewport(
        &self,
        selector: &str,
        _viewport: (u32, u32),
    ) -> Result<Vec<ElementInfo>, String> {
        let parsed = Selector::parse(selector).map_err(|e| e.to_string())?;
        Ok(self
            .document
            .query_all(&parsed)
            .into_iter()
            .map(|m| ElementInfo {
                bbox: None,
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

    fn renderer_style_for_viewport(
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
    fn formatter_scroll_state_becomes_bounded_nested_source_intent() {
        let mut page = Page::from_html(
            "file:///scroll.html",
            r#"<!doctype html><style>
                #scroller { width:120px; height:80px; overflow:auto; }
                #content { width:280px; height:320px; }
            </style><div id="scroller"><div id="content"></div></div>"#,
        )
        .unwrap();
        let scroller = page.query_selector_all("#scroller").unwrap()[0].clone();
        let snapshot = page
            .render_snapshot(
                BrowsingContextId::new(1).unwrap(),
                DocumentId::new(2).unwrap(),
                (240, 160),
                1,
                1.0,
                1.0,
            )
            .unwrap();
        let root_id = snapshot
            .nodes
            .iter()
            .find(|node| node.parent_id.is_none())
            .unwrap()
            .id;
        let raw_node_id = u64::try_from(scroller.node_id).unwrap();
        let scroll_node_id = RenderScrollNodeId::new(raw_node_id + 1).unwrap();
        let commit = RenderCommit {
            version: vixen_api::RENDER_PROTOCOL_VERSION,
            commit_id: vixen_api::RenderCommitId::new(1).unwrap(),
            revision: snapshot.revision,
            viewport: snapshot.viewport,
            geometry_index: Vec::new(),
            hit_test_handle: vixen_api::RenderHitTestHandle::new(1).unwrap(),
            text_query_handle: vixen_api::RenderTextQueryHandle::new(1).unwrap(),
            scroll_snapshot: vec![
                vixen_api::RenderScrollState {
                    scroll_node_id: RenderScrollNodeId::new(1).unwrap(),
                    node_id: root_id,
                    offset: RenderPoint { x: 0.0, y: 0.0 },
                    max_offset: RenderPoint { x: 0.0, y: 0.0 },
                    viewport: vixen_api::RenderRect {
                        x: 0.0,
                        y: 0.0,
                        width: 240.0,
                        height: 160.0,
                    },
                    content_size: vixen_api::RenderSize {
                        width: 240.0,
                        height: 160.0,
                    },
                },
                vixen_api::RenderScrollState {
                    scroll_node_id,
                    node_id: RenderNodeId::new(raw_node_id).unwrap(),
                    offset: RenderPoint { x: 0.0, y: 0.0 },
                    max_offset: RenderPoint { x: 160.0, y: 240.0 },
                    viewport: vixen_api::RenderRect {
                        x: 0.0,
                        y: 0.0,
                        width: 120.0,
                        height: 80.0,
                    },
                    content_size: vixen_api::RenderSize {
                        width: 280.0,
                        height: 320.0,
                    },
                },
            ],
            semantic_bounds: Vec::new(),
            truncations: Vec::new(),
        };
        page.apply_renderer_scroll(&commit);
        assert_eq!(
            page.renderer_source_generation(),
            snapshot.revision.source_generation
        );
        let state = page.element_scroll_state_snapshot().pop().unwrap();
        assert_eq!(state.node_id, scroller.node_id);
        assert_eq!(state.max, (160.0, 240.0));
        assert!(state.user_scrollable);

        assert!(page.set_element_scroll(
            scroller.node_id,
            scroller.id.as_deref(),
            &scroller.tag,
            (25.0, 45.0),
        ));
        let next = page
            .render_snapshot(
                BrowsingContextId::new(1).unwrap(),
                DocumentId::new(2).unwrap(),
                (240, 160),
                1,
                1.0,
                1.0,
            )
            .unwrap();
        let intent = next
            .scroll_intents
            .iter()
            .find(|intent| intent.scroll_node_id == scroll_node_id)
            .unwrap();
        assert_eq!(intent.node_id.get(), raw_node_id);
        assert_eq!(
            intent.kind,
            RenderScrollIntentKind::To(RenderPoint { x: 25.0, y: 45.0 })
        );

        assert!(page.set_element_scroll(
            scroller.node_id,
            scroller.id.as_deref(),
            &scroller.tag,
            (0.0, 179.2),
        ));
        let fractional = page
            .render_snapshot(
                BrowsingContextId::new(1).unwrap(),
                DocumentId::new(2).unwrap(),
                (240, 160),
                1,
                1.0,
                1.0,
            )
            .unwrap();
        assert_eq!(
            fractional
                .scroll_intents
                .iter()
                .find(|intent| intent.scroll_node_id == scroll_node_id)
                .unwrap()
                .kind,
            RenderScrollIntentKind::To(RenderPoint { x: 0.0, y: 179.2 })
        );
    }

    #[test]
    fn accessibility_snapshot_projects_dom_semantics_and_focus() {
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
        assert!(button.bbox.is_none());

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

        let first = page.find_text("Rust", true, true);
        assert_eq!(first.matches, 2);
        assert_eq!(first.active_match, Some(1));

        let second = page.find_text("Rust", true, true);
        assert_eq!(second.active_match, Some(2));

        let wrapped = page.find_text("Rust", true, true);
        assert_eq!(wrapped.active_match, Some(1));

        let reversed = page.find_text("Rust", true, false);
        assert_eq!(reversed.active_match, Some(2));

        assert_eq!(
            page.find_text("", false, true),
            FindTextResult {
                matches: 0,
                active_match: None,
            }
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

    fn style_value<'a>(styles: &'a [(String, String)], property: &str) -> Option<&'a str> {
        styles
            .iter()
            .find(|(name, _)| name == property)
            .map(|(_, value)| value.as_str())
    }
}
