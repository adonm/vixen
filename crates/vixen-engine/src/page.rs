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

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use vixen_api::{ElementInfo, EngineDiagnostic, EngineInspector, PageSnapshot};
use vixen_net::csp::ContentSecurityPolicy;

use crate::abort::{AbortController, AbortSignal, TimeoutSignal, abort_any};
use crate::class_list::DomTokenList;
use crate::dataset::collect_dataset;
use crate::display_list::{
    BackgroundAttachment, BackgroundBox, Color, DisplayListBuilder, DrawItem, PaintCommand,
    PaintStats, Rect, TextRun, dump_paint_commands, dump_paint_stats,
};
use crate::doc::{Document, InlineScript, ParseError};
use crate::form_submission::{FormEntry, FormEntryValue};
use crate::forms::{
    Validity, email_is_valid, length_validity, range_validity, step_mismatch_f64, url_is_valid,
};
use crate::geometry::{DOMMatrix, DOMPoint, DOMQuad, DOMRect};
use crate::headers::{Headers, is_forbidden_request_header, is_forbidden_response_header_name};
use crate::high_res_time::{MonotonicClock, TimeOrigin};
use crate::history::{HistoryEntry, SessionHistory};
use crate::layout_tree::{
    LayoutFragment, LayoutFragmentKind, LayoutTree, build_layout_tree, dump_layout_tree,
    layout_fragments_from_tree, line_boxes_from_tree,
};
use crate::line_layout::{LineBox, dump_line_boxes};
use crate::media_query::{MediaQuery, Viewport};
use crate::mime::MimeType;
use crate::mutation_observer::MutationObserver;
use crate::range::{Boundary, DocumentOrder, NodeRef, Range as DomRange, Selection};
use crate::responsive_select::select_from as select_responsive_image_source;
use crate::storage_key::validate_storage_key;
use crate::structured_clone::{ErrorKind, StructuredCloneValue, clone as structured_clone_value};
use crate::style_cascade::{AuthorStyleRule, AuthorStylesheet, css_supports};
use crate::style_dom::{ElementRelation, Selector};
use crate::text_codec::{TEXT_DECODER_ENCODING, TEXT_ENCODER_ENCODING, TextDecoder, TextEncoder};
use crate::traversal::{AcceptAll, NodeIterator, NodeType, Tree, TreeWalker, WhatToShow};
use crate::url_pattern::URLPattern;
use crate::url_search_params::UrlSearchParams;
use crate::whatwg_url::{
    Url as WhatwgUrl, parse as parse_url, parse_with_base as parse_url_with_base,
};

mod interaction;
pub use interaction::FormSubmissionSnapshot;

/// A loaded page at the current vertical integration boundary.
pub struct Page {
    url: String,
    document: Document,
    history: SessionHistory,
    csp: ContentSecurityPolicy,
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

    /// Minimal DOM-backed JS expression projection for host-binding smoke
    /// checks while full JS runtime DOM objects are still landing. This is a
    /// deliberately tiny, fail-closed subset: callers get `None` for unsupported
    /// expressions and can fall back to the real JS runtime.
    pub fn evaluate_dom_expression(&self, expr: &str) -> Option<Result<String, String>> {
        let expr = expr.trim();
        match expr {
            "document.title" => return Some(Ok(self.document.title().unwrap_or_default())),
            "document.readyState" => return Some(Ok("complete".into())),
            "document.compatMode" => return Some(Ok("CSS1Compat".into())),
            "document.characterSet" | "document.charset" => return Some(Ok("UTF-8".into())),
            "document.contentType" => return Some(Ok("text/html".into())),
            "document.visibilityState" => return Some(Ok("visible".into())),
            "document.hidden" => return Some(Ok("false".into())),
            "document.referrer" => return Some(Ok(String::new())),
            "document.URL" | "location.href" | "window.location.href" => {
                return Some(Ok(self.url.clone()));
            }
            "document.location.href" => return Some(Ok(self.url.clone())),
            "document.documentURI" => return Some(Ok(self.url.clone())),
            "document.baseURI" => return Some(Ok(self.document_base_uri())),
            "document.hasFocus()" => return Some(Ok("true".into())),
            "history.length" | "window.history.length" => {
                return Some(Ok(self.history.length().to_string()));
            }
            "history.state" | "window.history.state" => {
                return Some(Ok(history_state_value(&self.history)));
            }
            "history.scrollRestoration" | "window.history.scrollRestoration" => {
                return Some(Ok(self
                    .history
                    .scroll_restoration()
                    .to_keyword()
                    .to_owned()));
            }
            "document.body.textContent" | "document.body.innerText" => {
                return Some(Ok(self.document.body_text_content()));
            }
            "document.forms.length" => {
                return Some(self.query_selector_all("form").map(|m| m.len().to_string()));
            }
            "document.images.length" => {
                return Some(self.query_selector_all("img").map(|m| m.len().to_string()));
            }
            "document.links.length" => {
                return Some(
                    self.query_selector_all("a[href], area[href]")
                        .map(|m| m.len().to_string()),
                );
            }
            "document.scripts.length" => {
                return Some(
                    self.query_selector_all("script")
                        .map(|m| m.len().to_string()),
                );
            }
            "document.createRange().collapsed" => {
                return Some(Ok(document_range().is_collapsed().to_string()));
            }
            "document.createRange().startOffset" => {
                return Some(Ok(document_range().start.offset.to_string()));
            }
            "document.createRange().endOffset" => {
                return Some(Ok(document_range().end.offset.to_string()));
            }
            "document.createRange().toString()" => return Some(Ok(String::new())),
            "window.getSelection().rangeCount" | "document.getSelection().rangeCount" => {
                return Some(Ok(Selection::empty().range_count().to_string()));
            }
            "window.getSelection().isCollapsed" | "document.getSelection().isCollapsed" => {
                return Some(Ok(Selection::empty().is_collapsed().to_string()));
            }
            _ => {}
        }

        if let Some(result) = self.document_member_expr(expr) {
            return Some(result);
        }

        if let Some(result) = self.computed_style_expr(expr) {
            return Some(result);
        }

        if let Some(result) = self.cssom_expr(expr) {
            return Some(result);
        }

        if let Some(result) = performance_expr(expr) {
            return Some(result);
        }

        if let Some(result) = match_media_expr(expr) {
            return Some(result);
        }

        if let Some(result) = viewport_expr(expr) {
            return Some(result);
        }

        if let Some(result) = geometry_expr(expr) {
            return Some(result);
        }

        if let Some(result) = navigator_expr(expr) {
            return Some(result);
        }

        if let Some(result) = storage_expr(expr) {
            return Some(result);
        }

        if let Some(result) = event_expr(expr) {
            return Some(result);
        }

        if let Some(result) = self.traversal_expr(expr) {
            return Some(result);
        }

        if let Some(result) = structured_clone_expr(expr) {
            return Some(result);
        }

        if let Some(result) = mutation_observer_expr(expr) {
            return Some(result);
        }

        if let Some(result) = headers_expr(expr) {
            return Some(result);
        }

        if let Some(result) = blob_expr(expr) {
            return Some(result);
        }

        if let Some(result) = file_expr(expr) {
            return Some(result);
        }

        if let Some(result) = request_expr(expr) {
            return Some(result);
        }

        if let Some(result) = response_static_expr(expr) {
            return Some(result);
        }

        if let Some(result) = response_expr(expr) {
            return Some(result);
        }

        if let Some(result) = abort_expr(expr) {
            return Some(result);
        }

        if let Some(result) = url_pattern_expr(expr) {
            return Some(result);
        }

        if let Some(result) = url_can_parse_expr(expr) {
            return Some(result);
        }

        if let Some(result) = url_expr(expr) {
            return Some(result);
        }

        if let Some(result) = url_search_params_expr(expr) {
            return Some(result);
        }

        if let Some(result) = self.form_data_expr(expr) {
            return Some(result);
        }

        if let Some(result) = text_encoder_expr(expr) {
            return Some(result);
        }

        if let Some(result) = text_decoder_expr(expr) {
            return Some(result);
        }

        if let Some(result) = base64_expr(expr) {
            return Some(result);
        }

        if let Some(result) = dom_parser_expr(expr) {
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

    fn traversal_expr(&self, expr: &str) -> Option<Result<String, String>> {
        if let Some(rest) = expr.strip_prefix("document.createTreeWalker(") {
            return Some(self.parse_element_traversal_call(rest).and_then(
                |(root, what_to_show, member)| {
                    self.tree_walker_member_value(root.node_id, what_to_show, member)
                },
            ));
        }
        if let Some(rest) = expr.strip_prefix("document.createNodeIterator(") {
            return Some(self.parse_element_traversal_call(rest).and_then(
                |(root, what_to_show, member)| {
                    self.node_iterator_member_value(root.node_id, what_to_show, member)
                },
            ));
        }
        None
    }

    fn parse_element_traversal_call<'a>(
        &self,
        input: &'a str,
    ) -> Result<(ElementInfo, WhatToShow, &'a str), String> {
        let rest = input
            .strip_prefix("document.getElementById(")
            .ok_or_else(|| "traversal smoke needs document.getElementById root".to_owned())?;
        let (id, rest) = parse_single_string_arg_call(rest)
            .ok_or_else(|| "traversal smoke needs a simple id root".to_owned())?;
        if !is_simple_id_selector(&id) {
            return Err("traversal smoke only accepts simple ids".into());
        }
        let rest = rest
            .trim_start()
            .strip_prefix(',')
            .ok_or_else(|| "traversal smoke needs a whatToShow argument".to_owned())?
            .trim_start();
        let (what_to_show, rest) = parse_what_to_show(rest)
            .ok_or_else(|| "unsupported traversal whatToShow".to_owned())?;
        let member = rest
            .trim_start()
            .strip_prefix(')')
            .ok_or_else(|| "unsupported traversal constructor arguments".to_owned())?;
        let selector = format!("#{id}");
        let Some(root) = self.query_selector_all(&selector)?.into_iter().next() else {
            return Err("traversal root matched nothing".into());
        };
        Ok((root, what_to_show, member))
    }

    fn tree_walker_member_value(
        &self,
        root: usize,
        what_to_show: WhatToShow,
        member: &str,
    ) -> Result<String, String> {
        let tree = ElementTraversalTree {
            document: &self.document,
        };
        let filter = AcceptAll;
        let mut walker = TreeWalker::new(root, what_to_show);
        match member {
            ".root.id" | ".currentNode.id" => self.traversal_node_member_value(root, ".id"),
            ".whatToShow" => Ok(what_to_show.0.to_string()),
            _ => {
                if let Some(tail) = member.strip_prefix(".firstChild()") {
                    return self.traversal_result_member(walker.first_child(&tree, &filter), tail);
                }
                if let Some(tail) = member.strip_prefix(".lastChild()") {
                    return self.traversal_result_member(walker.last_child(&tree, &filter), tail);
                }
                if let Some(tail) = member.strip_prefix(".nextNode()") {
                    return self.traversal_result_member(walker.next_node(&tree, &filter), tail);
                }
                if let Some(tail) = member.strip_prefix(".parentNode()") {
                    return self.traversal_result_member(walker.parent_node(&tree, &filter), tail);
                }
                Err("unsupported TreeWalker eval member expression".into())
            }
        }
    }

    fn node_iterator_member_value(
        &self,
        root: usize,
        what_to_show: WhatToShow,
        member: &str,
    ) -> Result<String, String> {
        let tree = ElementTraversalTree {
            document: &self.document,
        };
        let filter = AcceptAll;
        let mut iterator = NodeIterator::new(root, what_to_show);
        match member {
            ".root.id" | ".referenceNode.id" => self.traversal_node_member_value(root, ".id"),
            ".whatToShow" => Ok(what_to_show.0.to_string()),
            _ => {
                if let Some(tail) = member.strip_prefix(".nextNode()") {
                    return self.traversal_result_member(iterator.next_node(&tree, &filter), tail);
                }
                if let Some(tail) = member.strip_prefix(".previousNode()") {
                    return self
                        .traversal_result_member(iterator.previous_node(&tree, &filter), tail);
                }
                Err("unsupported NodeIterator eval member expression".into())
            }
        }
    }

    fn traversal_result_member(&self, node: Option<usize>, member: &str) -> Result<String, String> {
        match (node, member) {
            (None, " === null") => Ok("true".into()),
            (Some(_), " === null") => Ok("false".into()),
            (None, _) => Err("traversal eval matched nothing".into()),
            (Some(node), member) => self.traversal_node_member_value(node, member),
        }
    }

    fn traversal_node_member_value(&self, node: usize, member: &str) -> Result<String, String> {
        let Some(info) = self
            .document
            .element_by_node_id(node)
            .map(|element| element.into_element_info())
        else {
            return Err("traversal eval node matched nothing".into());
        };
        self.element_member_value(info, member)
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
        if let Some(member) = expr.strip_prefix("document.activeElement") {
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
        if member == ".checkValidity()" || member == ".reportValidity()" {
            return self
                .element_or_form_validity(info)
                .map(|validity| validity.is_valid().to_string());
        }
        if member == ".willValidate" {
            return Ok(will_validate(info).to_string());
        }
        if let Some(flag) = member.strip_prefix(".validity.") {
            return self.validity_flag_value(info, flag);
        }
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
        if let Some(rest) = member.strip_prefix(".closest(") {
            let (selector, tail) = parse_single_string_arg_call(rest)
                .ok_or_else(|| "Element.closest smoke needs a selector string".to_owned())?;
            return self.closest_member_value(info.node_id, &selector, tail);
        }
        if let Some(tail) = member.strip_prefix(".getBoundingClientRect()") {
            return self.element_bounding_client_rect_value(info, tail);
        }
        if member == ".getClientRects().length" {
            return Ok(if self.element_bounding_client_rect(info).is_some() {
                "1".into()
            } else {
                "0".into()
            });
        }
        if let Some(inner) = member
            .strip_prefix(".dispatchEvent(")
            .and_then(|inner| inner.strip_suffix(')'))
        {
            return dispatch_event_value(inner);
        }
        if let Some(result) = self.related_element_member_value(info, member) {
            return result;
        }
        Err("unsupported DOM eval member expression".into())
    }

    fn closest_member_value(
        &self,
        node_id: usize,
        selector: &str,
        member: &str,
    ) -> Result<String, String> {
        let selector = Selector::parse(selector).map_err(|err| err.to_string())?;
        let mut current = Some(node_id);
        while let Some(id) = current {
            if self.document.matches_selector(id, &selector) {
                return match member {
                    " === null" => Ok("false".into()),
                    " !== null" => Ok("true".into()),
                    _ => self.traversal_node_member_value(id, member),
                };
            }
            current = self
                .document
                .related_element_by_node_id(id, ElementRelation::Parent)
                .map(|element| element.into_element_info().node_id);
        }
        match member {
            " === null" => Ok("true".into()),
            " !== null" => Ok("false".into()),
            _ => Err("Element.closest matched nothing".into()),
        }
    }

    fn element_bounding_client_rect(&self, info: &ElementInfo) -> Option<DOMRect> {
        let tree = self.layout_tree((800, 600));
        let (x, y, width, height) = layout_bbox_for_node(&tree, info.node_id)?;
        Some(DOMRect {
            x,
            y,
            width,
            height,
        })
    }

    fn element_bounding_client_rect_value(
        &self,
        info: &ElementInfo,
        member: &str,
    ) -> Result<String, String> {
        let rect = self.element_bounding_client_rect(info).unwrap_or_default();
        dom_rect_member_value(&rect, member)
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
        if let Some(rest) = member.strip_prefix(".classList") {
            return token_list_member_value(&info, "class", rest);
        }
        if let Some(rest) = member.strip_prefix(".relList") {
            return token_list_member_value(&info, "rel", rest);
        }
        if let Some(rest) = member.strip_prefix(".sandbox") {
            return token_list_member_value(&info, "sandbox", rest);
        }
        if let Some(rest) = member.strip_prefix(".dataset") {
            return dataset_member_value(&info, rest);
        }

        match member {
            ".id" => Ok(info.id.unwrap_or_default()),
            ".className" => Ok(info.classes.join(" ")),
            ".tagName" => Ok(info.tag.to_ascii_uppercase()),
            ".nodeName" => Ok(info.tag.to_ascii_uppercase()),
            ".localName" => Ok(info.tag),
            ".nodeType" => Ok("1".into()),
            ".isConnected" => Ok("true".into()),
            ".ownerDocument === document" => Ok("true".into()),
            ".textContent" | ".innerText" => Ok(self
                .document
                .element_text_content(info.node_id)
                .unwrap_or(info.text)),
            ".innerHTML" => Ok(self
                .document
                .element_inner_html(info.node_id)
                .unwrap_or_default()),
            ".outerHTML" => Ok(self
                .document
                .element_outer_html(info.node_id)
                .unwrap_or_default()),
            ".childElementCount" | ".children.length" => Ok(self
                .document
                .element_child_count(info.node_id)
                .unwrap_or_default()
                .to_string()),
            ".value" => Ok(self.form_control_value(&info)),
            ".currentSrc" => Ok(image_current_src(&info)),
            ".name" => Ok(element_attr(&info, "name").unwrap_or_default()),
            ".type" => Ok(element_attr(&info, "type").unwrap_or_else(|| default_type(&info))),
            ".content" => Ok(element_attr(&info, "content").unwrap_or_default()),
            ".charset" => Ok(element_attr(&info, "charset").unwrap_or_default()),
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

    fn computed_style_expr(&self, expr: &str) -> Option<Result<String, String>> {
        let rest = expr
            .strip_prefix("getComputedStyle(")
            .or_else(|| expr.strip_prefix("window.getComputedStyle("))?;
        let rest = rest.strip_prefix("document.querySelector(")?;
        let (selector, tail) = parse_single_string_arg_call(rest)?;
        let member = tail.trim_start().strip_prefix(')')?;
        let property = computed_style_member_property(member)?;
        Some(self.computed_style_property_value(&selector, &property))
    }

    fn cssom_expr(&self, expr: &str) -> Option<Result<String, String>> {
        if let Some(result) = css_supports_expr(expr) {
            return Some(result);
        }
        if let Some(result) = self.cssom_rule_expr(expr) {
            return Some(result);
        }
        let value = match expr {
            "document.styleSheets.length" => self.document.style_blocks().len().to_string(),
            "document.styleSheets[0].cssRules.length" => {
                self.author_stylesheet.rule_count().to_string()
            }
            "document.styleSheets[0].disabled" => "false".into(),
            "document.styleSheets[0].href === null" => "true".into(),
            "document.styleSheets[0].ownerNode.tagName" => "STYLE".into(),
            _ => return None,
        };
        Some(Ok(value))
    }

    fn cssom_rule_expr(&self, expr: &str) -> Option<Result<String, String>> {
        let rest = expr.strip_prefix("document.styleSheets[0].cssRules[")?;
        let (raw_index, member) = rest.split_once(']')?;
        let Ok(index) = raw_index.trim().parse::<usize>() else {
            return Some(Err("CSSRule smoke needs a numeric rule index".into()));
        };
        let Some(rule) = self.author_stylesheet.rule(index) else {
            return Some(Err("CSSRule smoke index out of range".into()));
        };
        Some(cssom_rule_member_value(rule, member))
    }

    fn form_data_expr(&self, expr: &str) -> Option<Result<String, String>> {
        let parsed = parse_form_data_constructor_expr(expr)?;
        Some(parsed.and_then(|(form_id, member)| {
            self.form_submission(&form_id)
                .and_then(|submission| form_data_member_value(&submission.entries, member))
        }))
    }

    fn computed_style_property_value(
        &self,
        selector: &str,
        property: &str,
    ) -> Result<String, String> {
        let Some(info) = self.query_selector_all(selector)?.into_iter().next() else {
            return Err("getComputedStyle selector matched nothing".into());
        };
        let styles = self.computed_style(info.node_id);
        Ok(styles
            .into_iter()
            .find(|(name, _)| computed_style_property_matches(name, property))
            .map(|(_, value)| value)
            .unwrap_or_default())
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

    fn validity_flag_value(&self, info: &ElementInfo, flag: &str) -> Result<String, String> {
        let validity = self.element_validity(info)?;
        let value = match flag {
            "valid" => validity.is_valid(),
            "valueMissing" => validity.value_missing,
            "typeMismatch" => validity.type_mismatch,
            "patternMismatch" => validity.pattern_mismatch,
            "tooLong" => validity.too_long,
            "tooShort" => validity.too_short,
            "rangeUnderflow" => validity.range_underflow,
            "rangeOverflow" => validity.range_overflow,
            "stepMismatch" => validity.step_mismatch,
            "badInput" => validity.bad_input,
            "customError" => validity.custom_error,
            _ => return Err("unsupported validity flag".into()),
        };
        Ok(value.to_string())
    }

    fn element_or_form_validity(&self, info: &ElementInfo) -> Result<Validity, String> {
        if info.tag == "form" {
            return self.form_validity(info);
        }
        self.element_validity(info)
    }

    fn form_validity(&self, info: &ElementInfo) -> Result<Validity, String> {
        let Some(id) = info.id.as_deref().filter(|id| is_simple_id_selector(id)) else {
            return Ok(Validity::default());
        };
        let selector = format!("#{id} input, #{id} select, #{id} textarea");
        let mut aggregate = Validity::default();
        for control in self.query_selector_all(&selector)? {
            merge_validity(&mut aggregate, &self.element_validity(&control)?);
        }
        Ok(aggregate)
    }

    fn element_validity(&self, info: &ElementInfo) -> Result<Validity, String> {
        let mut validity = Validity::default();
        if !will_validate(info) {
            return Ok(validity);
        }

        let value = self.form_control_value(info);
        let input_type = element_attr(info, "type")
            .unwrap_or_else(|| default_type(info))
            .to_ascii_lowercase();
        if element_has_attr(info, "required") {
            let missing = match input_type.as_str() {
                "checkbox" | "radio" => !element_has_attr(info, "checked"),
                _ => value.is_empty(),
            };
            if missing {
                validity.value_missing = true;
            }
        }

        if !value.is_empty() {
            match input_type.as_str() {
                "email" if !email_is_valid(&value) => validity.type_mismatch = true,
                "url" if !url_is_valid(&value) => validity.type_mismatch = true,
                "number" | "range" => self.apply_numeric_validity(info, &value, &mut validity),
                _ => {}
            }

            let len_validity = length_validity(
                value.chars().count(),
                element_attr(info, "minlength").and_then(|value| value.parse().ok()),
                element_attr(info, "maxlength").and_then(|value| value.parse().ok()),
            );
            merge_validity(&mut validity, &len_validity);
        }

        Ok(validity)
    }

    fn apply_numeric_validity(&self, info: &ElementInfo, value: &str, validity: &mut Validity) {
        let Ok(value) = value.parse::<f64>() else {
            validity.bad_input = true;
            return;
        };
        let min = element_attr(info, "min").and_then(|value| value.parse::<f64>().ok());
        let max = element_attr(info, "max").and_then(|value| value.parse::<f64>().ok());
        merge_validity(validity, &range_validity(value, min, max));

        if let Some(step_attr) = element_attr(info, "step") {
            if step_attr.eq_ignore_ascii_case("any") {
                return;
            }
            if let Ok(step) = step_attr.parse::<f64>() {
                let base = min.unwrap_or(0.0);
                validity.step_mismatch = step_mismatch_f64(base, value, step);
            }
        }
    }

    fn form_control_value(&self, info: &ElementInfo) -> String {
        match info.tag.as_str() {
            "textarea" => self
                .document
                .element_text_content(info.node_id)
                .unwrap_or_default(),
            "select" => self.selected_option_value(info),
            _ => element_attr(info, "value").unwrap_or_default(),
        }
    }

    fn selected_option_value(&self, info: &ElementInfo) -> String {
        let Some(id) = info.id.as_deref().filter(|id| is_simple_id_selector(id)) else {
            return String::new();
        };
        for selector in [format!("#{id} option[selected]"), format!("#{id} option")] {
            if let Ok(Some(option)) = self
                .query_selector_all(&selector)
                .map(|items| items.into_iter().next())
            {
                return element_attr(&option, "value").unwrap_or(option.text);
            }
        }
        String::new()
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

fn query_selector_all_length_expr(expr: &str) -> Option<String> {
    let inner = expr
        .strip_prefix("document.querySelectorAll(")?
        .strip_suffix(").length")?;
    js_string_literal(inner)
}

fn computed_style_member_property(member: &str) -> Option<String> {
    if let Some(property) = method_string_arg(member, ".getPropertyValue(") {
        return Some(property);
    }
    let ident = member.strip_prefix('.')?;
    if ident.is_empty()
        || !ident
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return None;
    }
    Some(js_style_property_to_css(ident))
}

fn js_style_property_to_css(ident: &str) -> String {
    if ident == "cssFloat" {
        return "float".into();
    }
    let mut out = String::new();
    for ch in ident.chars() {
        if ch.is_ascii_uppercase() {
            out.push('-');
            out.push(ch.to_ascii_lowercase());
        } else if ch == '_' {
            out.push('-');
        } else {
            out.push(ch);
        }
    }
    out
}

fn computed_style_property_matches(name: &str, property: &str) -> bool {
    if property.starts_with("--") {
        name == property
    } else {
        name.eq_ignore_ascii_case(property)
    }
}

struct ElementTraversalTree<'a> {
    document: &'a Document,
}

impl Tree for ElementTraversalTree<'_> {
    fn parent(&self, node: usize) -> Option<usize> {
        self.document
            .related_element_by_node_id(node, ElementRelation::Parent)
            .map(|element| element.into_element_info().node_id)
    }

    fn first_child(&self, node: usize) -> Option<usize> {
        self.document
            .related_element_by_node_id(node, ElementRelation::FirstChild)
            .map(|element| element.into_element_info().node_id)
    }

    fn last_child(&self, node: usize) -> Option<usize> {
        self.document
            .related_element_by_node_id(node, ElementRelation::LastChild)
            .map(|element| element.into_element_info().node_id)
    }

    fn prev_sibling(&self, node: usize) -> Option<usize> {
        self.document
            .related_element_by_node_id(node, ElementRelation::PreviousSibling)
            .map(|element| element.into_element_info().node_id)
    }

    fn next_sibling(&self, node: usize) -> Option<usize> {
        self.document
            .related_element_by_node_id(node, ElementRelation::NextSibling)
            .map(|element| element.into_element_info().node_id)
    }

    fn node_type(&self, _node: usize) -> NodeType {
        NodeType::Element
    }
}

fn parse_what_to_show(input: &str) -> Option<(WhatToShow, &str)> {
    for (prefix, value) in [
        ("NodeFilter.SHOW_ALL", WhatToShow::ALL),
        ("NodeFilter.SHOW_ELEMENT", WhatToShow::ELEMENT),
        ("NodeFilter.SHOW_TEXT", WhatToShow::TEXT),
        ("NodeFilter.SHOW_COMMENT", WhatToShow::COMMENT),
        ("1", WhatToShow::ELEMENT),
    ] {
        if let Some(rest) = input.strip_prefix(prefix) {
            return Some((value, rest));
        }
    }
    None
}

fn structured_clone_expr(expr: &str) -> Option<Result<String, String>> {
    let rest = expr.strip_prefix("structuredClone(")?;
    let (value, member) = parse_structured_clone_arg_call(rest)?;
    let known_platform_types = std::collections::HashSet::new();
    Some(
        structured_clone_value(&value, &[], false, &known_platform_types)
            .map_err(|err| err.to_string())
            .and_then(|cloned| structured_clone_member_value(&cloned, member)),
    )
}

fn parse_structured_clone_arg_call(input: &str) -> Option<(StructuredCloneValue, &str)> {
    let input = input.trim_start();
    let (value, rest) = parse_structured_clone_value_prefix(input)?;
    let rest = rest.trim_start().strip_prefix(')')?;
    Some((value, rest))
}

fn parse_structured_clone_value_prefix(input: &str) -> Option<(StructuredCloneValue, &str)> {
    let input = input.trim_start();
    if let Some(rest) = input.strip_prefix("undefined") {
        return Some((StructuredCloneValue::Undefined, rest));
    }
    if let Some((value, rest)) = parse_js_string_prefix(input) {
        return Some((StructuredCloneValue::String(value), rest));
    }
    if let Some(rest) = input.strip_prefix("true") {
        return Some((StructuredCloneValue::Boolean(true), rest));
    }
    if let Some(rest) = input.strip_prefix("false") {
        return Some((StructuredCloneValue::Boolean(false), rest));
    }
    if let Some(rest) = input.strip_prefix("null") {
        return Some((StructuredCloneValue::Null, rest));
    }
    if let Some(parsed) = parse_structured_clone_date_prefix(input) {
        return Some(parsed);
    }
    if let Some(parsed) = parse_structured_clone_error_prefix(input) {
        return Some(parsed);
    }
    if let Some(parsed) = parse_structured_clone_map_prefix(input) {
        return Some(parsed);
    }
    if let Some(parsed) = parse_structured_clone_set_prefix(input) {
        return Some(parsed);
    }
    if input.starts_with('[') {
        return parse_structured_clone_array_prefix(input);
    }
    if input.starts_with('{') {
        return parse_structured_clone_object_prefix(input);
    }
    parse_number_prefix(input).map(|(value, rest)| (StructuredCloneValue::Number(value), rest))
}

fn parse_structured_clone_date_prefix(input: &str) -> Option<(StructuredCloneValue, &str)> {
    let rest = input.strip_prefix("new Date(")?;
    let (args, rest) = parse_number_args_call(rest).ok()?;
    (args.len() == 1).then_some((StructuredCloneValue::Date(args[0]), rest))
}

fn parse_structured_clone_error_prefix(input: &str) -> Option<(StructuredCloneValue, &str)> {
    for (prefix, kind) in [
        ("new Error(", ErrorKind::Error),
        ("new EvalError(", ErrorKind::EvalError),
        ("new RangeError(", ErrorKind::RangeError),
        ("new ReferenceError(", ErrorKind::ReferenceError),
        ("new SyntaxError(", ErrorKind::SyntaxError),
        ("new TypeError(", ErrorKind::TypeError),
        ("new URIError(", ErrorKind::UriError),
    ] {
        if let Some(rest) = input.strip_prefix(prefix) {
            let (message, rest) = parse_js_string_prefix(rest.trim_start())?;
            let rest = rest.trim_start().strip_prefix(')')?;
            return Some((
                StructuredCloneValue::Error {
                    kind,
                    message,
                    stack: String::new(),
                },
                rest,
            ));
        }
    }
    None
}

fn parse_structured_clone_map_prefix(input: &str) -> Option<(StructuredCloneValue, &str)> {
    let mut rest = input.strip_prefix("new Map(")?.trim_start();
    if let Some(tail) = rest.strip_prefix(')') {
        return Some((StructuredCloneValue::Map(Vec::new()), tail));
    }
    rest = rest.strip_prefix('[')?.trim_start();
    let mut entries = Vec::new();
    if let Some(tail) = rest.strip_prefix(']') {
        let tail = tail.trim_start().strip_prefix(')')?;
        return Some((StructuredCloneValue::Map(entries), tail));
    }
    loop {
        rest = rest.strip_prefix('[')?.trim_start();
        let (key, tail) = parse_structured_clone_value_prefix(rest)?;
        rest = tail.trim_start().strip_prefix(',')?.trim_start();
        let (value, tail) = parse_structured_clone_value_prefix(rest)?;
        rest = tail.trim_start().strip_prefix(']')?.trim_start();
        entries.push((key, value));
        if let Some(tail) = rest.strip_prefix(',') {
            rest = tail.trim_start();
            continue;
        }
        let tail = rest.strip_prefix(']')?.trim_start().strip_prefix(')')?;
        return Some((StructuredCloneValue::Map(entries), tail));
    }
}

fn parse_structured_clone_set_prefix(input: &str) -> Option<(StructuredCloneValue, &str)> {
    let mut rest = input.strip_prefix("new Set(")?.trim_start();
    if let Some(tail) = rest.strip_prefix(')') {
        return Some((StructuredCloneValue::Set(Vec::new()), tail));
    }
    rest = rest.strip_prefix('[')?.trim_start();
    let mut values = Vec::new();
    if let Some(tail) = rest.strip_prefix(']') {
        let tail = tail.trim_start().strip_prefix(')')?;
        return Some((StructuredCloneValue::Set(values), tail));
    }
    loop {
        let (value, tail) = parse_structured_clone_value_prefix(rest)?;
        values.push(value);
        rest = tail.trim_start();
        if let Some(tail) = rest.strip_prefix(']') {
            let tail = tail.trim_start().strip_prefix(')')?;
            return Some((StructuredCloneValue::Set(values), tail));
        }
        rest = rest.strip_prefix(',')?.trim_start();
    }
}

fn parse_structured_clone_array_prefix(input: &str) -> Option<(StructuredCloneValue, &str)> {
    let mut rest = input.strip_prefix('[')?.trim_start();
    let mut values = Vec::new();
    if let Some(tail) = rest.strip_prefix(']') {
        return Some((StructuredCloneValue::Array(values), tail));
    }
    loop {
        let (value, tail) = parse_structured_clone_value_prefix(rest)?;
        values.push(value);
        rest = tail.trim_start();
        if let Some(tail) = rest.strip_prefix(']') {
            return Some((StructuredCloneValue::Array(values), tail));
        }
        rest = rest.strip_prefix(',')?.trim_start();
    }
}

fn parse_structured_clone_object_prefix(input: &str) -> Option<(StructuredCloneValue, &str)> {
    let mut rest = input.strip_prefix('{')?.trim_start();
    let mut entries = Vec::new();
    if let Some(tail) = rest.strip_prefix('}') {
        return Some((StructuredCloneValue::Object(entries), tail));
    }
    loop {
        let (name, tail) = parse_object_key_prefix(rest)?;
        rest = tail.trim_start().strip_prefix(':')?.trim_start();
        let (value, tail) = parse_structured_clone_value_prefix(rest)?;
        entries.push((name, value));
        rest = tail.trim_start();
        if let Some(tail) = rest.strip_prefix('}') {
            return Some((StructuredCloneValue::Object(entries), tail));
        }
        rest = rest.strip_prefix(',')?.trim_start();
    }
}

fn structured_clone_member_value(
    value: &StructuredCloneValue,
    member: &str,
) -> Result<String, String> {
    match (value, member) {
        (_, "") => Ok(render_structured_clone_value(value)),
        (StructuredCloneValue::Array(items), ".length") => Ok(items.len().to_string()),
        (StructuredCloneValue::Array(items), member) => bracket_usize(member)
            .and_then(|index| items.get(index))
            .map(render_structured_clone_value)
            .ok_or_else(|| "unsupported structuredClone array member".to_owned()),
        (StructuredCloneValue::Date(value), ".getTime()") => Ok(render_number(*value)),
        (StructuredCloneValue::Map(entries), ".size") => Ok(entries.len().to_string()),
        (StructuredCloneValue::Map(entries), member) => {
            structured_clone_map_member_value(entries, member)
        }
        (StructuredCloneValue::Set(values), ".size") => Ok(values.len().to_string()),
        (StructuredCloneValue::Set(values), member) => {
            structured_clone_set_member_value(values, member)
        }
        (StructuredCloneValue::Error { kind, .. }, ".name") => Ok(kind.name().into()),
        (StructuredCloneValue::Error { message, .. }, ".message") => Ok(message.clone()),
        (StructuredCloneValue::Object(entries), member) => {
            let name = member
                .strip_prefix('.')
                .ok_or_else(|| "unsupported structuredClone object member".to_owned())?;
            entries
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| render_structured_clone_value(value))
                .ok_or_else(|| "structuredClone object member missing".to_owned())
        }
        _ => Err("unsupported structuredClone eval member".into()),
    }
}

fn structured_clone_map_member_value(
    entries: &[(StructuredCloneValue, StructuredCloneValue)],
    member: &str,
) -> Result<String, String> {
    if let Some(key) = method_structured_clone_key_arg(member, ".get(") {
        return Ok(entries
            .iter()
            .find(|(candidate, _)| structured_clone_key_eq(candidate, &key))
            .map(|(_, value)| render_structured_clone_value(value))
            .unwrap_or_else(|| "undefined".into()));
    }
    if let Some(key) = method_structured_clone_key_arg(member, ".has(") {
        return Ok(entries
            .iter()
            .any(|(candidate, _)| structured_clone_key_eq(candidate, &key))
            .to_string());
    }
    if let Some((key, value)) = entries.first() {
        return match member {
            ".entries().next().done" => Ok("false".into()),
            ".entries().next().value[0]" => Ok(render_structured_clone_value(key)),
            ".entries().next().value[1]" => Ok(render_structured_clone_value(value)),
            _ => Err("unsupported structuredClone Map member".into()),
        };
    }
    match member {
        ".entries().next().done" => Ok("true".into()),
        ".entries().next().value[0]" | ".entries().next().value[1]" => Ok("undefined".into()),
        _ => Err("unsupported structuredClone Map member".into()),
    }
}

fn structured_clone_set_member_value(
    values: &[StructuredCloneValue],
    member: &str,
) -> Result<String, String> {
    if let Some(key) = method_structured_clone_key_arg(member, ".has(") {
        return Ok(values
            .iter()
            .any(|candidate| structured_clone_key_eq(candidate, &key))
            .to_string());
    }
    if let Some(value) = values.first() {
        return match member {
            ".values().next().done" => Ok("false".into()),
            ".values().next().value" => Ok(render_structured_clone_value(value)),
            _ => Err("unsupported structuredClone Set member".into()),
        };
    }
    match member {
        ".values().next().done" => Ok("true".into()),
        ".values().next().value" => Ok("undefined".into()),
        _ => Err("unsupported structuredClone Set member".into()),
    }
}

fn method_structured_clone_key_arg(member: &str, prefix: &str) -> Option<StructuredCloneValue> {
    let inner = member.strip_prefix(prefix)?.strip_suffix(')')?;
    let (value, rest) = parse_structured_clone_value_prefix(inner.trim())?;
    rest.trim().is_empty().then_some(value)
}

fn structured_clone_key_eq(left: &StructuredCloneValue, right: &StructuredCloneValue) -> bool {
    match (left, right) {
        (StructuredCloneValue::Undefined, StructuredCloneValue::Undefined)
        | (StructuredCloneValue::Null, StructuredCloneValue::Null) => true,
        (StructuredCloneValue::Boolean(a), StructuredCloneValue::Boolean(b)) => a == b,
        (StructuredCloneValue::Number(a), StructuredCloneValue::Number(b)) => a == b,
        (StructuredCloneValue::BigInt(a), StructuredCloneValue::BigInt(b))
        | (StructuredCloneValue::String(a), StructuredCloneValue::String(b)) => a == b,
        _ => false,
    }
}

fn render_structured_clone_value(value: &StructuredCloneValue) -> String {
    match value {
        StructuredCloneValue::Undefined => "undefined".into(),
        StructuredCloneValue::Null => "null".into(),
        StructuredCloneValue::Boolean(value) => value.to_string(),
        StructuredCloneValue::Number(value) => render_number(*value),
        StructuredCloneValue::BigInt(value) | StructuredCloneValue::String(value) => value.clone(),
        StructuredCloneValue::Array(_) => "[object Array]".into(),
        StructuredCloneValue::Object(_) => "[object Object]".into(),
        StructuredCloneValue::Map(_) => "[object Map]".into(),
        StructuredCloneValue::Set(_) => "[object Set]".into(),
        StructuredCloneValue::Date(_) => "[object Date]".into(),
        StructuredCloneValue::ArrayBuffer(_) => "[object ArrayBuffer]".into(),
        StructuredCloneValue::MessagePort(_) => "[object MessagePort]".into(),
        StructuredCloneValue::Error { kind, .. } => kind.name().into(),
        StructuredCloneValue::PlatformObject(name) => format!("[object {name}]"),
    }
}

fn mutation_observer_expr(expr: &str) -> Option<Result<String, String>> {
    let member = expr.strip_prefix("new MutationObserver(() => {})")?;
    let mut observer = MutationObserver::new();
    match member {
        ".takeRecords().length" => Some(Ok(observer.take_records().len().to_string())),
        ".disconnect()" => {
            observer.disconnect();
            Some(Ok("undefined".into()))
        }
        _ => Some(Err("unsupported MutationObserver eval member".into())),
    }
}

fn headers_expr(expr: &str) -> Option<Result<String, String>> {
    let rest = expr.strip_prefix("new Headers(")?;
    let (headers, member) = match parse_headers_constructor_call(rest) {
        Some(Ok(parsed)) => parsed,
        Some(Err(err)) => return Some(Err(err)),
        None => return None,
    };
    Some(headers_member_value(&headers, member))
}

fn headers_member_value(headers: &Headers, member: &str) -> Result<String, String> {
    if let Some(name) = method_string_arg(member, ".get(") {
        return Ok(headers.get(&name).unwrap_or_else(|| "null".into()));
    }
    if let Some(name) = method_string_arg(member, ".has(") {
        return Ok(headers.has(&name).to_string());
    }
    if let Some(rest) = member.strip_prefix(".getAll(") {
        let (name, tail) = parse_single_string_arg_call(rest)
            .ok_or_else(|| "Headers.getAll smoke needs a string name".to_owned())?;
        let values = headers.get_all(&name);
        return match tail {
            ".length" => Ok(values.len().to_string()),
            _ => bracket_usize(tail)
                .map(|index| values.get(index).copied().unwrap_or("undefined").to_owned())
                .ok_or_else(|| "unsupported Headers.getAll eval member".into()),
        };
    }
    if let Some(result) = headers_iterator_member_value(headers, member) {
        return result;
    }
    match member {
        ".size" => Ok(headers.len().to_string()),
        _ => Err("unsupported Headers eval member expression".into()),
    }
}

fn headers_iterator_member_value(
    headers: &Headers,
    member: &str,
) -> Option<Result<String, String>> {
    let first = headers.iter().next();
    match member {
        ".keys().next().done" | ".values().next().done" | ".entries().next().done" => {
            Some(Ok(first.is_none().to_string()))
        }
        ".keys().next().value" => Some(Ok(first
            .as_ref()
            .map(|(name, _)| (*name).to_owned())
            .unwrap_or_else(|| "undefined".into()))),
        ".values().next().value" => Some(Ok(first
            .as_ref()
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| "undefined".into()))),
        ".entries().next().value[0]" => Some(Ok(first
            .as_ref()
            .map(|(name, _)| (*name).to_owned())
            .unwrap_or_else(|| "undefined".into()))),
        ".entries().next().value[1]" => Some(Ok(first
            .as_ref()
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| "undefined".into()))),
        _ => None,
    }
}

fn parse_headers_constructor_call(input: &str) -> Option<Result<(Headers, &str), String>> {
    let rest = input.trim_start();
    if let Some(member) = rest.strip_prefix(')') {
        return Some(Ok((Headers::new(), member)));
    }
    let (records, rest) = match parse_header_records_prefix(rest)? {
        Ok(parsed) => parsed,
        Err(err) => return Some(Err(err)),
    };
    let member = rest.trim_start().strip_prefix(')')?;
    Some(
        Headers::from_records(records)
            .map(|headers| (headers, member))
            .map_err(|err| err.to_string()),
    )
}

type HeaderRecordsParse<'a> = Option<Result<(Vec<(String, String)>, &'a str), String>>;

fn parse_header_records_prefix(input: &str) -> HeaderRecordsParse<'_> {
    let mut rest = input.trim_start();
    rest = rest.strip_prefix('[')?.trim_start();
    let mut records = Vec::new();
    if let Some(tail) = rest.strip_prefix(']') {
        return Some(Ok((records, tail)));
    }
    loop {
        let Some(tail) = rest.strip_prefix('[') else {
            return Some(Err("Headers records need [name, value] tuples".into()));
        };
        rest = tail.trim_start();
        let Some((name, tail)) = parse_js_string_prefix(rest) else {
            return Some(Err("Headers record name needs a string".into()));
        };
        rest = tail.trim_start().strip_prefix(',')?.trim_start();
        let Some((value, tail)) = parse_js_string_prefix(rest) else {
            return Some(Err("Headers record value needs a string".into()));
        };
        records.push((name, value));
        rest = tail.trim_start().strip_prefix(']')?.trim_start();
        if let Some(tail) = rest.strip_prefix(',') {
            rest = tail.trim_start();
            continue;
        }
        let tail = rest.strip_prefix(']')?;
        return Some(Ok((records, tail)));
    }
}

fn headers_from_records_filtered<I, F>(records: I, is_forbidden: F) -> Result<Headers, String>
where
    I: IntoIterator<Item = (String, String)>,
    F: Fn(&str) -> bool,
{
    let mut headers = Headers::new();
    for (name, value) in records {
        if is_forbidden(&name) {
            continue;
        }
        headers
            .append(&name, &value)
            .map_err(|err| err.to_string())?;
    }
    Ok(headers)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BlobProjection {
    size: usize,
    type_: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileProjection {
    blob: BlobProjection,
    name: String,
    last_modified: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResponseProjection {
    type_: String,
    status: u16,
    status_text: String,
    headers: Headers,
    body_is_null: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RequestProjection {
    url: String,
    method: String,
    headers: Headers,
    body_is_null: bool,
    cache: String,
    credentials: String,
    mode: String,
    redirect: String,
    referrer: String,
    referrer_policy: String,
    integrity: String,
    keepalive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RequestOptions {
    method: String,
    headers: Headers,
    body: BodyInitProjection,
    cache: String,
    credentials: String,
    mode: String,
    redirect: String,
    referrer: String,
    referrer_policy: String,
    integrity: String,
    keepalive: bool,
}

impl Default for RequestOptions {
    fn default() -> Self {
        Self {
            method: "GET".into(),
            headers: Headers::new(),
            body: BodyInitProjection::null(),
            cache: "default".into(),
            credentials: "same-origin".into(),
            mode: "cors".into(),
            redirect: "follow".into(),
            referrer: "about:client".into(),
            referrer_policy: String::new(),
            integrity: String::new(),
            keepalive: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FileOptions {
    type_: String,
    last_modified: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResponseOptions {
    status: u16,
    status_text: String,
    headers: Headers,
}

impl Default for ResponseOptions {
    fn default() -> Self {
        Self {
            status: 200,
            status_text: String::new(),
            headers: Headers::new(),
        }
    }
}

fn blob_expr(expr: &str) -> Option<Result<String, String>> {
    let rest = expr.strip_prefix("new Blob(")?;
    let (blob, member) = match parse_blob_constructor_prefix(rest) {
        Some(Ok(parsed)) => parsed,
        Some(Err(err)) => return Some(Err(err)),
        None => return None,
    };
    Some(blob_member_value(&blob, member))
}

fn file_expr(expr: &str) -> Option<Result<String, String>> {
    let rest = expr.strip_prefix("new File(")?;
    let (file, member) = match parse_file_constructor_prefix(rest) {
        Some(Ok(parsed)) => parsed,
        Some(Err(err)) => return Some(Err(err)),
        None => return None,
    };
    Some(file_member_value(&file, member))
}

fn request_expr(expr: &str) -> Option<Result<String, String>> {
    let rest = expr.strip_prefix("new Request(")?;
    let (request, member) = match parse_request_constructor_prefix(rest) {
        Some(Ok(parsed)) => parsed,
        Some(Err(err)) => return Some(Err(err)),
        None => return None,
    };
    Some(request_member_value(&request, member))
}

fn response_static_expr(expr: &str) -> Option<Result<String, String>> {
    if let Some(member) = expr.strip_prefix("Response.error()") {
        let response = ResponseProjection {
            type_: "error".into(),
            status: 0,
            status_text: String::new(),
            headers: Headers::new(),
            body_is_null: true,
        };
        return Some(response_member_value(&response, member));
    }
    if let Some(rest) = expr.strip_prefix("Response.json(") {
        let Some((options, member)) = parse_response_json_args_call(rest) else {
            return Some(Err(
                "Response.json smoke needs data and optional options".into()
            ));
        };
        if !(200..=599).contains(&options.status) {
            return Some(Err("Response status smoke must be in 200..=599".into()));
        }
        let mut headers = options.headers;
        if !headers.has("content-type")
            && let Err(err) = headers.set("Content-Type", "application/json")
        {
            return Some(Err(err.to_string()));
        }
        let response = ResponseProjection {
            type_: "default".into(),
            status: options.status,
            status_text: options.status_text,
            headers,
            body_is_null: false,
        };
        return Some(response_member_value(&response, member));
    }
    if let Some(rest) = expr.strip_prefix("Response.redirect(") {
        let Some((url, status, member)) = parse_response_redirect_args_call(rest) else {
            return Some(Err(
                "Response.redirect smoke needs url and optional status".into()
            ));
        };
        if !matches!(status, 301 | 302 | 303 | 307 | 308) {
            return Some(Err(
                "Response.redirect status must be a redirect status".into()
            ));
        }
        let location = match parse_url(&url) {
            Ok(url) => url.serialize(),
            Err(err) => return Some(Err(err.to_string())),
        };
        let headers = match Headers::from_records([("Location".to_owned(), location)]) {
            Ok(headers) => headers,
            Err(err) => return Some(Err(err.to_string())),
        };
        let response = ResponseProjection {
            type_: "default".into(),
            status,
            status_text: String::new(),
            headers,
            body_is_null: true,
        };
        return Some(response_member_value(&response, member));
    }
    None
}

fn response_expr(expr: &str) -> Option<Result<String, String>> {
    let rest = expr.strip_prefix("new Response(")?;
    let (response, member) = match parse_response_constructor_prefix(rest) {
        Some(Ok(parsed)) => parsed,
        Some(Err(err)) => return Some(Err(err)),
        None => return None,
    };
    Some(response_member_value(&response, member))
}

fn parse_request_constructor_prefix(
    input: &str,
) -> Option<Result<(RequestProjection, &str), String>> {
    let (input_url, mut rest) = parse_js_string_prefix(input.trim_start())?;
    let url = match parse_url(&input_url) {
        Ok(url) => url.serialize(),
        Err(err) => return Some(Err(err.to_string())),
    };

    rest = rest.trim_start();
    let mut options = RequestOptions::default();
    if let Some(tail) = rest.strip_prefix(',') {
        let (parsed, tail) = match parse_request_options_prefix(tail.trim_start())? {
            Ok(parsed) => parsed,
            Err(err) => return Some(Err(err)),
        };
        options = parsed;
        rest = tail.trim_start();
    }
    let member = rest.strip_prefix(')')?;
    if !options.body.is_null && matches!(options.method.as_str(), "GET" | "HEAD") {
        return Some(Err("Request GET/HEAD smoke cannot carry a body".into()));
    }
    let mut headers = options.headers;
    if let Some(content_type) = options.body.content_type.filter(|value| !value.is_empty())
        && !headers.has("content-type")
        && let Err(err) = headers.set("Content-Type", &content_type)
    {
        return Some(Err(err.to_string()));
    }
    Some(Ok((
        RequestProjection {
            url,
            method: options.method,
            headers,
            body_is_null: options.body.is_null,
            cache: options.cache,
            credentials: options.credentials,
            mode: options.mode,
            redirect: options.redirect,
            referrer: options.referrer,
            referrer_policy: options.referrer_policy,
            integrity: options.integrity,
            keepalive: options.keepalive,
        },
        member,
    )))
}

fn parse_blob_constructor_prefix(input: &str) -> Option<Result<(BlobProjection, &str), String>> {
    let (parts, mut rest) = match parse_string_array_prefix(input.trim_start())? {
        Ok(parsed) => parsed,
        Err(err) => return Some(Err(err)),
    };
    let mut type_ = String::new();
    rest = rest.trim_start();
    if let Some(tail) = rest.strip_prefix(',') {
        let (parsed_type, tail) = match parse_blob_options_prefix(tail.trim_start())? {
            Ok(parsed) => parsed,
            Err(err) => return Some(Err(err)),
        };
        type_ = parsed_type;
        rest = tail.trim_start();
    }
    let member = rest.strip_prefix(')')?;
    let size = parts
        .iter()
        .map(|part| TextEncoder.encode(part).len())
        .sum();
    Some(Ok((BlobProjection { size, type_ }, member)))
}

fn parse_file_constructor_prefix(input: &str) -> Option<Result<(FileProjection, &str), String>> {
    let (parts, mut rest) = match parse_string_array_prefix(input.trim_start())? {
        Ok(parsed) => parsed,
        Err(err) => return Some(Err(err)),
    };
    rest = rest.trim_start().strip_prefix(',')?.trim_start();
    let (name, tail) = parse_js_string_prefix(rest)?;
    rest = tail.trim_start();
    let mut options = FileOptions::default();
    if let Some(tail) = rest.strip_prefix(',') {
        let (parsed, tail) = match parse_file_options_prefix(tail.trim_start())? {
            Ok(parsed) => parsed,
            Err(err) => return Some(Err(err)),
        };
        options = parsed;
        rest = tail.trim_start();
    }
    let member = rest.strip_prefix(')')?;
    let size = parts
        .iter()
        .map(|part| TextEncoder.encode(part).len())
        .sum();
    Some(Ok((
        FileProjection {
            blob: BlobProjection {
                size,
                type_: options.type_,
            },
            name,
            last_modified: options.last_modified,
        },
        member,
    )))
}

fn parse_response_constructor_prefix(
    input: &str,
) -> Option<Result<(ResponseProjection, &str), String>> {
    let (body, mut rest) = match parse_body_init_prefix(input.trim_start())? {
        Ok(parsed) => parsed,
        Err(err) => return Some(Err(err)),
    };
    rest = rest.trim_start();
    let mut options = ResponseOptions::default();
    if let Some(tail) = rest.strip_prefix(',') {
        let (parsed, tail) = match parse_response_options_prefix(tail.trim_start())? {
            Ok(parsed) => parsed,
            Err(err) => return Some(Err(err)),
        };
        options = parsed;
        rest = tail.trim_start();
    }
    let member = rest.strip_prefix(')')?;
    if !(200..=599).contains(&options.status) {
        return Some(Err("Response status smoke must be in 200..=599".into()));
    }

    let mut headers = options.headers;
    if let Some(type_) = body.content_type.filter(|type_| !type_.is_empty())
        && !headers.has("content-type")
        && let Err(err) = headers.set("Content-Type", &type_)
    {
        return Some(Err(err.to_string()));
    }

    Some(Ok((
        ResponseProjection {
            type_: "default".into(),
            status: options.status,
            status_text: options.status_text,
            headers,
            body_is_null: body.is_null,
        },
        member,
    )))
}

fn parse_request_options_prefix(input: &str) -> Option<Result<(RequestOptions, &str), String>> {
    let mut rest = input.trim_start().strip_prefix('{')?.trim_start();
    let mut options = RequestOptions::default();
    if let Some(tail) = rest.strip_prefix('}') {
        return Some(Ok((options, tail)));
    }
    loop {
        let Some((key, tail)) = parse_object_key_prefix(rest) else {
            return Some(Err("Request options need simple keys".into()));
        };
        rest = tail.trim_start().strip_prefix(':')?.trim_start();
        match key.as_str() {
            "method" => {
                let Some((raw, tail)) = parse_js_string_prefix(rest) else {
                    return Some(Err("Request method option needs a string".into()));
                };
                options.method = match normalize_request_method(&raw) {
                    Ok(method) => method,
                    Err(err) => return Some(Err(err)),
                };
                rest = tail.trim_start();
            }
            "headers" => {
                let (records, tail) = match parse_header_records_prefix(rest)? {
                    Ok(parsed) => parsed,
                    Err(err) => return Some(Err(err)),
                };
                options.headers =
                    match headers_from_records_filtered(records, is_forbidden_request_header) {
                        Ok(headers) => headers,
                        Err(err) => return Some(Err(err)),
                    };
                rest = tail.trim_start();
            }
            "body" => {
                let (body, tail) = match parse_body_init_prefix(rest)? {
                    Ok(parsed) => parsed,
                    Err(err) => return Some(Err(err)),
                };
                options.body = body;
                rest = tail.trim_start();
            }
            "cache" => {
                let Some((value, tail)) = parse_js_string_prefix(rest) else {
                    return Some(Err("Request cache option needs a string".into()));
                };
                if !matches!(
                    value.as_str(),
                    "default"
                        | "no-store"
                        | "reload"
                        | "no-cache"
                        | "force-cache"
                        | "only-if-cached"
                ) {
                    return Some(Err("unsupported Request cache value".into()));
                }
                options.cache = value;
                rest = tail.trim_start();
            }
            "credentials" => {
                let Some((value, tail)) = parse_js_string_prefix(rest) else {
                    return Some(Err("Request credentials option needs a string".into()));
                };
                if !matches!(value.as_str(), "omit" | "same-origin" | "include") {
                    return Some(Err("unsupported Request credentials value".into()));
                }
                options.credentials = value;
                rest = tail.trim_start();
            }
            "mode" => {
                let Some((value, tail)) = parse_js_string_prefix(rest) else {
                    return Some(Err("Request mode option needs a string".into()));
                };
                if !matches!(value.as_str(), "cors" | "no-cors" | "same-origin") {
                    return Some(Err("unsupported Request mode value".into()));
                }
                options.mode = value;
                rest = tail.trim_start();
            }
            "redirect" => {
                let Some((value, tail)) = parse_js_string_prefix(rest) else {
                    return Some(Err("Request redirect option needs a string".into()));
                };
                if !matches!(value.as_str(), "follow" | "error" | "manual") {
                    return Some(Err("unsupported Request redirect value".into()));
                }
                options.redirect = value;
                rest = tail.trim_start();
            }
            "referrer" => {
                let Some((value, tail)) = parse_js_string_prefix(rest) else {
                    return Some(Err("Request referrer option needs a string".into()));
                };
                if value.chars().any(|ch| matches!(ch, '\r' | '\n')) {
                    return Some(Err("Request referrer cannot contain newlines".into()));
                }
                options.referrer = value;
                rest = tail.trim_start();
            }
            "referrerPolicy" => {
                let Some((value, tail)) = parse_js_string_prefix(rest) else {
                    return Some(Err("Request referrerPolicy option needs a string".into()));
                };
                if !matches!(
                    value.as_str(),
                    "" | "no-referrer"
                        | "no-referrer-when-downgrade"
                        | "origin"
                        | "origin-when-cross-origin"
                        | "same-origin"
                        | "strict-origin"
                        | "strict-origin-when-cross-origin"
                        | "unsafe-url"
                ) {
                    return Some(Err("unsupported Request referrerPolicy value".into()));
                }
                options.referrer_policy = value;
                rest = tail.trim_start();
            }
            "integrity" => {
                let Some((value, tail)) = parse_js_string_prefix(rest) else {
                    return Some(Err("Request integrity option needs a string".into()));
                };
                if value.chars().any(|ch| matches!(ch, '\r' | '\n')) {
                    return Some(Err("Request integrity cannot contain newlines".into()));
                }
                options.integrity = value;
                rest = tail.trim_start();
            }
            "keepalive" => {
                let Some((value, tail)) = parse_bool_prefix(rest) else {
                    return Some(Err("Request keepalive option needs a boolean".into()));
                };
                options.keepalive = value;
                rest = tail.trim_start();
            }
            _ => return Some(Err("unsupported Request option".into())),
        }
        if let Some(tail) = rest.strip_prefix('}') {
            return Some(Ok((options, tail)));
        }
        rest = rest.strip_prefix(',')?.trim_start();
    }
}

fn normalize_request_method(raw: &str) -> Result<String, String> {
    if raw.is_empty() || !raw.chars().all(|ch| ch.is_ascii_alphabetic()) {
        return Err("Request method smoke needs an ASCII token".into());
    }
    let method = raw.to_ascii_uppercase();
    if matches!(
        method.as_str(),
        "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS"
    ) {
        Ok(method)
    } else {
        Err("unsupported Request method".into())
    }
}

fn parse_response_redirect_args_call(input: &str) -> Option<(String, u16, &str)> {
    let (url, rest) = parse_js_string_prefix(input.trim_start())?;
    let rest = rest.trim_start();
    if let Some(member) = rest.strip_prefix(')') {
        return Some((url, 302, member));
    }
    let rest = rest.strip_prefix(',')?.trim_start();
    let (status, rest) = parse_u16_prefix(rest)?;
    let member = rest.trim_start().strip_prefix(')')?;
    Some((url, status, member))
}

fn parse_response_json_args_call(input: &str) -> Option<(ResponseOptions, &str)> {
    let (_, mut rest) = parse_structured_clone_value_prefix(input.trim_start())?;
    rest = rest.trim_start();
    let mut options = ResponseOptions::default();
    if let Some(tail) = rest.strip_prefix(',') {
        let (parsed, tail) = match parse_response_options_prefix(tail.trim_start())? {
            Ok(parsed) => parsed,
            Err(_) => return None,
        };
        options = parsed;
        rest = tail.trim_start();
    }
    let member = rest.strip_prefix(')')?;
    Some((options, member))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BodyInitProjection {
    content_type: Option<String>,
    is_null: bool,
}

impl BodyInitProjection {
    fn null() -> Self {
        Self {
            content_type: None,
            is_null: true,
        }
    }
}

fn parse_body_init_prefix(input: &str) -> Option<Result<(BodyInitProjection, &str), String>> {
    let input = input.trim_start();
    if let Some(rest) = input.strip_prefix("null") {
        return Some(Ok((
            BodyInitProjection {
                content_type: None,
                is_null: true,
            },
            rest,
        )));
    }
    if let Some((_, rest)) = parse_js_string_prefix(input) {
        return Some(Ok((
            BodyInitProjection {
                content_type: Some("text/plain;charset=UTF-8".into()),
                is_null: false,
            },
            rest,
        )));
    }
    if let Some(rest) = input.strip_prefix("new Blob(") {
        let (blob, rest) = match parse_blob_constructor_prefix(rest)? {
            Ok(parsed) => parsed,
            Err(err) => return Some(Err(err)),
        };
        if rest.starts_with('.') {
            return Some(Err("Response Blob body cannot have member access".into()));
        }
        return Some(Ok((
            BodyInitProjection {
                content_type: (!blob.type_.is_empty()).then_some(blob.type_),
                is_null: false,
            },
            rest,
        )));
    }
    if let Some(rest) = input.strip_prefix("new URLSearchParams(") {
        let (_, rest) = match parse_url_search_params_constructor_call(rest)? {
            Ok(parsed) => parsed,
            Err(err) => return Some(Err(err)),
        };
        if rest.starts_with('.') {
            return Some(Err("Body URLSearchParams cannot have member access".into()));
        }
        return Some(Ok((
            BodyInitProjection {
                content_type: Some("application/x-www-form-urlencoded;charset=UTF-8".into()),
                is_null: false,
            },
            rest,
        )));
    }
    None
}

fn parse_string_array_prefix(input: &str) -> Option<Result<(Vec<String>, &str), String>> {
    let mut rest = input.trim_start().strip_prefix('[')?.trim_start();
    let mut values = Vec::new();
    if let Some(tail) = rest.strip_prefix(']') {
        return Some(Ok((values, tail)));
    }
    loop {
        let Some((value, tail)) = parse_js_string_prefix(rest) else {
            return Some(Err("Blob/File smoke only accepts string parts".into()));
        };
        values.push(value);
        rest = tail.trim_start();
        if let Some(tail) = rest.strip_prefix(']') {
            return Some(Ok((values, tail)));
        }
        rest = rest.strip_prefix(',')?.trim_start();
    }
}

fn parse_blob_options_prefix(input: &str) -> Option<Result<(String, &str), String>> {
    let mut rest = input.trim_start().strip_prefix('{')?.trim_start();
    let mut type_ = String::new();
    if let Some(tail) = rest.strip_prefix('}') {
        return Some(Ok((type_, tail)));
    }
    loop {
        let Some((key, tail)) = parse_object_key_prefix(rest) else {
            return Some(Err("Blob options need simple keys".into()));
        };
        rest = tail.trim_start().strip_prefix(':')?.trim_start();
        match key.as_str() {
            "type" => {
                let Some((raw, tail)) = parse_js_string_prefix(rest) else {
                    return Some(Err("Blob type option needs a string".into()));
                };
                type_ = normalize_blob_type(&raw);
                rest = tail.trim_start();
            }
            _ => return Some(Err("unsupported Blob option".into())),
        }
        if let Some(tail) = rest.strip_prefix('}') {
            return Some(Ok((type_, tail)));
        }
        rest = rest.strip_prefix(',')?.trim_start();
    }
}

fn parse_file_options_prefix(input: &str) -> Option<Result<(FileOptions, &str), String>> {
    let mut rest = input.trim_start().strip_prefix('{')?.trim_start();
    let mut options = FileOptions::default();
    if let Some(tail) = rest.strip_prefix('}') {
        return Some(Ok((options, tail)));
    }
    loop {
        let Some((key, tail)) = parse_object_key_prefix(rest) else {
            return Some(Err("File options need simple keys".into()));
        };
        rest = tail.trim_start().strip_prefix(':')?.trim_start();
        match key.as_str() {
            "type" => {
                let Some((raw, tail)) = parse_js_string_prefix(rest) else {
                    return Some(Err("File type option needs a string".into()));
                };
                options.type_ = normalize_blob_type(&raw);
                rest = tail.trim_start();
            }
            "lastModified" => {
                let Some((value, tail)) = parse_i64_prefix(rest) else {
                    return Some(Err("File lastModified option needs an integer".into()));
                };
                options.last_modified = value;
                rest = tail.trim_start();
            }
            _ => return Some(Err("unsupported File option".into())),
        }
        if let Some(tail) = rest.strip_prefix('}') {
            return Some(Ok((options, tail)));
        }
        rest = rest.strip_prefix(',')?.trim_start();
    }
}

fn parse_response_options_prefix(input: &str) -> Option<Result<(ResponseOptions, &str), String>> {
    let mut rest = input.trim_start().strip_prefix('{')?.trim_start();
    let mut options = ResponseOptions::default();
    if let Some(tail) = rest.strip_prefix('}') {
        return Some(Ok((options, tail)));
    }
    loop {
        let Some((key, tail)) = parse_object_key_prefix(rest) else {
            return Some(Err("Response options need simple keys".into()));
        };
        rest = tail.trim_start().strip_prefix(':')?.trim_start();
        match key.as_str() {
            "status" => {
                let Some((value, tail)) = parse_u16_prefix(rest) else {
                    return Some(Err("Response status option needs an integer".into()));
                };
                options.status = value;
                rest = tail.trim_start();
            }
            "statusText" => {
                let Some((value, tail)) = parse_js_string_prefix(rest) else {
                    return Some(Err("Response statusText option needs a string".into()));
                };
                if value.chars().any(|ch| matches!(ch, '\r' | '\n')) {
                    return Some(Err("Response statusText cannot contain newlines".into()));
                }
                options.status_text = value;
                rest = tail.trim_start();
            }
            "headers" => {
                let (records, tail) = match parse_header_records_prefix(rest)? {
                    Ok(parsed) => parsed,
                    Err(err) => return Some(Err(err)),
                };
                options.headers =
                    match headers_from_records_filtered(records, is_forbidden_response_header_name)
                    {
                        Ok(headers) => headers,
                        Err(err) => return Some(Err(err)),
                    };
                rest = tail.trim_start();
            }
            _ => return Some(Err("unsupported Response option".into())),
        }
        if let Some(tail) = rest.strip_prefix('}') {
            return Some(Ok((options, tail)));
        }
        rest = rest.strip_prefix(',')?.trim_start();
    }
}

fn normalize_blob_type(raw: &str) -> String {
    if !raw.chars().all(|ch| ('\u{20}'..='\u{7e}').contains(&ch)) {
        return String::new();
    }
    let lower = raw.to_ascii_lowercase();
    MimeType::parse(&lower)
        .map(|mime| mime.serialize())
        .unwrap_or_default()
}

fn blob_member_value(blob: &BlobProjection, member: &str) -> Result<String, String> {
    match member {
        ".size" => Ok(blob.size.to_string()),
        ".type" => Ok(blob.type_.clone()),
        _ => Err("unsupported Blob eval member expression".into()),
    }
}

fn file_member_value(file: &FileProjection, member: &str) -> Result<String, String> {
    match member {
        ".name" => Ok(file.name.clone()),
        ".size" => Ok(file.blob.size.to_string()),
        ".type" => Ok(file.blob.type_.clone()),
        ".lastModified" => Ok(file.last_modified.to_string()),
        _ => Err("unsupported File eval member expression".into()),
    }
}

fn request_member_value(request: &RequestProjection, member: &str) -> Result<String, String> {
    if let Some(member) = member.strip_prefix(".headers") {
        return headers_member_value(&request.headers, member);
    }
    match member {
        ".url" => Ok(request.url.clone()),
        ".method" => Ok(request.method.clone()),
        ".cache" => Ok(request.cache.clone()),
        ".credentials" => Ok(request.credentials.clone()),
        ".mode" => Ok(request.mode.clone()),
        ".redirect" => Ok(request.redirect.clone()),
        ".referrer" => Ok(request.referrer.clone()),
        ".referrerPolicy" => Ok(request.referrer_policy.clone()),
        ".integrity" => Ok(request.integrity.clone()),
        ".keepalive" => Ok(request.keepalive.to_string()),
        ".destination" => Ok(String::new()),
        ".bodyUsed" => Ok("false".into()),
        ".body === null" => Ok(request.body_is_null.to_string()),
        ".signal.aborted" => Ok("false".into()),
        _ => Err("unsupported Request eval member expression".into()),
    }
}

fn response_member_value(response: &ResponseProjection, member: &str) -> Result<String, String> {
    if let Some(member) = member.strip_prefix(".headers") {
        return headers_member_value(&response.headers, member);
    }
    match member {
        ".status" => Ok(response.status.to_string()),
        ".statusText" => Ok(response.status_text.clone()),
        ".ok" => Ok((200..=299).contains(&response.status).to_string()),
        ".bodyUsed" | ".redirected" => Ok("false".into()),
        ".type" => Ok(response.type_.clone()),
        ".url" => Ok(String::new()),
        ".body === null" => Ok(response.body_is_null.to_string()),
        _ => Err("unsupported Response eval member expression".into()),
    }
}

fn abort_expr(expr: &str) -> Option<Result<String, String>> {
    if let Some(member) = expr.strip_prefix("new AbortController().signal") {
        let controller = AbortController::new();
        return Some(abort_signal_member_value(controller.signal(), member));
    }
    if let Some(rest) = expr.strip_prefix("AbortSignal.timeout(") {
        let (delay_ms, member) = parse_u64_arg_call(rest)?;
        let timeout = TimeoutSignal::new(delay_ms);
        return Some(abort_signal_member_value(timeout.signal(), member));
    }
    if let Some(member) = expr.strip_prefix("AbortSignal.any([AbortSignal.timeout(0)])") {
        let timeout = TimeoutSignal::new(0);
        let signal = abort_any(&[timeout.signal()]);
        return Some(abort_signal_member_value(&signal, member));
    }
    None
}

fn abort_signal_member_value(signal: &AbortSignal, member: &str) -> Result<String, String> {
    match member {
        ".aborted" => Ok(signal.aborted().to_string()),
        ".reason" => Ok(signal
            .reason()
            .map(|reason| reason.name().to_owned())
            .unwrap_or_else(|| "null".into())),
        ".reason.name" => Ok(signal
            .reason()
            .map(|reason| reason.name().to_owned())
            .unwrap_or_else(|| "undefined".into())),
        _ => Err("unsupported AbortSignal eval member expression".into()),
    }
}

fn url_pattern_expr(expr: &str) -> Option<Result<String, String>> {
    let rest = expr.strip_prefix("new URLPattern(")?;
    let (pattern, member) = parse_string_or_pathname_object_arg_call(rest)?;
    let pattern = match URLPattern::compile(&pattern) {
        Ok(pattern) => pattern,
        Err(err) => return Some(Err(err.to_string())),
    };
    if let Some(rest) = member.strip_prefix(".test(") {
        let (pathname, tail) = parse_string_or_pathname_object_arg_call(rest)?;
        if !tail.is_empty() {
            return Some(Err("unsupported URLPattern.test eval member".into()));
        }
        return Some(Ok(pattern.test_pathname(&pathname).to_string()));
    }
    if let Some(rest) = member.strip_prefix(".exec(") {
        let (pathname, tail) = parse_string_or_pathname_object_arg_call(rest)?;
        let captures = pattern.match_pathname(&pathname);
        return Some(url_pattern_exec_member_value(captures.as_deref(), tail));
    }
    Some(Err("unsupported URLPattern eval member expression".into()))
}

fn url_pattern_exec_member_value(
    captures: Option<&[(String, String)]>,
    member: &str,
) -> Result<String, String> {
    if member == " === null" {
        return Ok(captures.is_none().to_string());
    }
    let captures = captures.ok_or_else(|| "URLPattern.exec matched nothing".to_owned())?;
    let name = if let Some(name) = member.strip_prefix(".pathname.groups.") {
        name.to_owned()
    } else if let Some(inner) = member
        .strip_prefix(".pathname.groups[")
        .and_then(|rest| rest.strip_suffix(']'))
    {
        js_string_literal(inner).ok_or_else(|| "URLPattern group needs a string key".to_owned())?
    } else {
        return Err("unsupported URLPattern.exec eval member".into());
    };
    Ok(captures
        .iter()
        .find(|(key, _)| key == &name)
        .map(|(_, value)| value.clone())
        .unwrap_or_else(|| "undefined".into()))
}

fn initial_session_history(url: &str) -> SessionHistory {
    SessionHistory::new(HistoryEntry::navigation(url))
}

fn performance_expr(expr: &str) -> Option<Result<String, String>> {
    let normalized = expr.strip_prefix("window.").unwrap_or(expr);
    if matches!(
        expr,
        "typeof performance.now()" | "typeof window.performance.now()"
    ) {
        return Some(Ok("number".into()));
    }
    if matches!(
        expr,
        "typeof performance.timeOrigin" | "typeof window.performance.timeOrigin"
    ) {
        return Some(Ok("number".into()));
    }

    let origin = TimeOrigin::from_unix_ms(0.0);
    let mut clock = MonotonicClock::new(origin);
    let now = clock.now(origin.unix_ms(), false);
    let value = match normalized {
        "performance.timeOrigin" => render_number(origin.unix_ms()),
        "performance.now()" => render_number(now),
        "performance.now() >= 0" => (now >= 0.0).to_string(),
        "performance.timeOrigin >= 0" => (origin.unix_ms() >= 0.0).to_string(),
        "performance.timeOrigin + performance.now() >= performance.timeOrigin" => {
            (origin.relative_to_unix(now) >= origin.unix_ms()).to_string()
        }
        _ => return None,
    };
    Some(Ok(value))
}

fn match_media_expr(expr: &str) -> Option<Result<String, String>> {
    let rest = expr
        .strip_prefix("matchMedia(")
        .or_else(|| expr.strip_prefix("window.matchMedia("))?;
    let (query, member) = parse_single_string_arg_call(rest)?;
    match member {
        ".media" => Some(Ok(query)),
        ".matches" => Some(
            MediaQuery::parse(&query)
                .map(|query| query.matches(&Viewport::default()).to_string())
                .map_err(|err| err.to_string()),
        ),
        _ => Some(Err("unsupported matchMedia eval member expression".into())),
    }
}

fn viewport_expr(expr: &str) -> Option<Result<String, String>> {
    let normalized = expr.strip_prefix("window.").unwrap_or(expr);
    let viewport = Viewport::default();
    let value = match normalized {
        "innerWidth"
        | "outerWidth"
        | "screen.width"
        | "screen.availWidth"
        | "visualViewport.width" => render_number(viewport.width_px),
        "innerHeight"
        | "outerHeight"
        | "screen.height"
        | "screen.availHeight"
        | "visualViewport.height" => render_number(viewport.height_px),
        "devicePixelRatio" | "visualViewport.scale" => render_number(viewport.dpr),
        "visualViewport.offsetLeft"
        | "visualViewport.offsetTop"
        | "visualViewport.pageLeft"
        | "visualViewport.pageTop"
        | "scrollX"
        | "pageXOffset"
        | "scrollY"
        | "pageYOffset" => "0".into(),
        "document.defaultView === window"
        | "self === window"
        | "top === window"
        | "parent === window" => "true".into(),
        "document.scrollingElement.tagName" => "HTML".into(),
        _ => return None,
    };
    Some(Ok(value))
}

fn css_supports_expr(expr: &str) -> Option<Result<String, String>> {
    let rest = expr
        .strip_prefix("CSS.supports(")
        .or_else(|| expr.strip_prefix("window.CSS.supports("))?;
    let Some((first, second, tail)) = parse_one_or_two_string_arg_call(rest) else {
        return Some(Err(
            "CSS.supports smoke needs one or two string arguments".into()
        ));
    };
    if !tail.is_empty() {
        return Some(Err("unsupported CSS.supports eval member expression".into()));
    }
    let condition = second
        .map(|value| format!("({first}: {value})"))
        .unwrap_or(first);
    Some(Ok(css_supports(&condition).to_string()))
}

fn cssom_rule_member_value(rule: AuthorStyleRule<'_>, member: &str) -> Result<String, String> {
    if let Some(property) = method_string_arg(member, ".style.getPropertyValue(") {
        return Ok(rule
            .get_property_value(&property)
            .unwrap_or_default()
            .to_owned());
    }
    if let Some(index) = method_usize_arg(member, ".style.item(") {
        return Ok(rule
            .declaration_property(index)
            .unwrap_or("null")
            .to_owned());
    }
    if let Some(index) = member
        .strip_prefix(".style[")
        .and_then(|tail| tail.strip_suffix(']'))
        .and_then(|index| index.trim().parse::<usize>().ok())
    {
        return Ok(rule
            .declaration_property(index)
            .unwrap_or("undefined")
            .to_owned());
    }
    let value = match member {
        ".selectorText" => rule.selector_text().to_owned(),
        ".cssText" => rule.css_text(),
        ".style.length" => rule.declaration_count().to_string(),
        _ => return Err("unsupported CSSRule eval member expression".into()),
    };
    Ok(value)
}

fn dom_rect_member_value(rect: &DOMRect, member: &str) -> Result<String, String> {
    if let Some(member) = member.strip_prefix(".toJSON()") {
        return dom_rect_member_value(rect, member);
    }
    let value = match member {
        "" => "[object DOMRect]".into(),
        ".x" => render_number(rect.x),
        ".y" => render_number(rect.y),
        ".width" => render_number(rect.width),
        ".height" => render_number(rect.height),
        ".left" => render_number(rect.left()),
        ".top" => render_number(rect.top()),
        ".right" => render_number(rect.right()),
        ".bottom" => render_number(rect.bottom()),
        _ => return Err("unsupported DOMRect eval member expression".into()),
    };
    Ok(value)
}

fn geometry_expr(expr: &str) -> Option<Result<String, String>> {
    if let Some(rest) = expr.strip_prefix("new DOMPoint(") {
        return Some(
            parse_dom_point_constructor(rest)
                .and_then(|(point, member)| dom_point_member_value(point, member)),
        );
    }
    if let Some(rest) = expr.strip_prefix("DOMPoint.fromPoint(") {
        return Some(
            parse_dom_point_init_arg_call(rest)
                .and_then(|(point, member)| dom_point_member_value(point, member)),
        );
    }
    if let Some(rest) = expr.strip_prefix("new DOMRect(") {
        return Some(
            parse_dom_rect_constructor(rest)
                .and_then(|(rect, member)| dom_rect_member_value(&rect, member)),
        );
    }
    if let Some(rest) = expr.strip_prefix("DOMRect.fromRect(") {
        return Some(
            parse_dom_rect_init_arg_call(rest)
                .and_then(|(rect, member)| dom_rect_member_value(&rect, member)),
        );
    }
    if let Some(rest) = expr.strip_prefix("DOMQuad.fromRect(") {
        return Some(
            parse_dom_rect_init_arg_call(rest)
                .and_then(|(rect, member)| dom_quad_member_value(DOMQuad::from_rect(rect), member)),
        );
    }
    if let Some(rest) = expr.strip_prefix("new DOMMatrix(") {
        return Some(
            parse_dom_matrix_constructor(rest)
                .and_then(|(matrix, member)| dom_matrix_member_value(matrix, member)),
        );
    }
    None
}

fn parse_dom_point_constructor(input: &str) -> Result<(DOMPoint, &str), String> {
    let (args, member) = parse_number_args_call(input)?;
    if args.len() > 4 {
        return Err("DOMPoint constructor smoke accepts at most four numbers".into());
    }
    let point = DOMPoint::new(
        args.first().copied().unwrap_or(0.0),
        args.get(1).copied().unwrap_or(0.0),
        args.get(2).copied().unwrap_or(0.0),
        args.get(3).copied().unwrap_or(1.0),
    );
    Ok((point, member))
}

fn parse_dom_rect_constructor(input: &str) -> Result<(DOMRect, &str), String> {
    let (args, member) = parse_number_args_call(input)?;
    if args.len() > 4 {
        return Err("DOMRect constructor smoke accepts at most four numbers".into());
    }
    let rect = DOMRect {
        x: args.first().copied().unwrap_or(0.0),
        y: args.get(1).copied().unwrap_or(0.0),
        width: args.get(2).copied().unwrap_or(0.0),
        height: args.get(3).copied().unwrap_or(0.0),
    };
    Ok((rect, member))
}

fn parse_dom_point_init_arg_call(input: &str) -> Result<(DOMPoint, &str), String> {
    let (values, member) = parse_number_init_arg_call(input, &["x", "y", "z", "w"])?;
    let point = DOMPoint::new(
        values.get("x").copied().unwrap_or(0.0),
        values.get("y").copied().unwrap_or(0.0),
        values.get("z").copied().unwrap_or(0.0),
        values.get("w").copied().unwrap_or(1.0),
    );
    Ok((point, member))
}

fn parse_dom_rect_init_arg_call(input: &str) -> Result<(DOMRect, &str), String> {
    let (values, member) = parse_number_init_arg_call(input, &["x", "y", "width", "height"])?;
    let rect = DOMRect {
        x: values.get("x").copied().unwrap_or(0.0),
        y: values.get("y").copied().unwrap_or(0.0),
        width: values.get("width").copied().unwrap_or(0.0),
        height: values.get("height").copied().unwrap_or(0.0),
    };
    Ok((rect, member))
}

fn parse_number_init_arg_call<'a>(
    input: &'a str,
    allowed_keys: &[&str],
) -> Result<(std::collections::BTreeMap<String, f64>, &'a str), String> {
    let (body, rest) = parse_simple_object_body(input.trim_start())
        .ok_or_else(|| "geometry init smoke needs a simple object".to_owned())?;
    let rest = rest
        .trim_start()
        .strip_prefix(')')
        .ok_or_else(|| "unsupported geometry init tail".to_owned())?;
    let mut values = std::collections::BTreeMap::new();
    for part in body
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let Some((raw_key, raw_value)) = part.split_once(':') else {
            return Err("geometry init entries need key: number".into());
        };
        let raw_key = raw_key.trim();
        let key = js_string_literal(raw_key).unwrap_or_else(|| raw_key.to_owned());
        if !allowed_keys.iter().any(|allowed| *allowed == key) {
            return Err("unsupported geometry init member".into());
        }
        let value = parse_finite_number(raw_value.trim())?;
        values.insert(key, value);
    }
    Ok((values, rest))
}

fn dom_point_member_value(point: DOMPoint, member: &str) -> Result<String, String> {
    let value = match member {
        "" => "[object DOMPoint]".into(),
        ".x" => render_number(point.x),
        ".y" => render_number(point.y),
        ".z" => render_number(point.z),
        ".w" => render_number(point.w),
        _ => return Err("unsupported DOMPoint eval member expression".into()),
    };
    Ok(value)
}

fn dom_quad_member_value(quad: DOMQuad, member: &str) -> Result<String, String> {
    if let Some(tail) = member.strip_prefix(".p1") {
        return dom_point_member_value(quad.p1, tail);
    }
    if let Some(tail) = member.strip_prefix(".p2") {
        return dom_point_member_value(quad.p2, tail);
    }
    if let Some(tail) = member.strip_prefix(".p3") {
        return dom_point_member_value(quad.p3, tail);
    }
    if let Some(tail) = member.strip_prefix(".p4") {
        return dom_point_member_value(quad.p4, tail);
    }
    if let Some(tail) = member.strip_prefix(".getBounds()") {
        return dom_rect_member_value(&quad.bounds(), tail);
    }
    match member {
        "" => Ok("[object DOMQuad]".into()),
        _ => Err("unsupported DOMQuad eval member expression".into()),
    }
}

fn parse_dom_matrix_constructor(input: &str) -> Result<(DOMMatrix, &str), String> {
    let input = input.trim_start();
    if let Some(member) = input.strip_prefix(')') {
        return Ok((DOMMatrix::identity(), member));
    }
    let (values, rest) = parse_number_array_prefix(input)?;
    let member = rest
        .trim_start()
        .strip_prefix(')')
        .ok_or_else(|| "unsupported DOMMatrix constructor tail".to_owned())?;
    let matrix = match values.len() {
        6 => DOMMatrix::from_2d(
            values[0], values[1], values[2], values[3], values[4], values[5],
        ),
        16 => {
            let mut array = [0.0; 16];
            array.copy_from_slice(&values);
            DOMMatrix::from_4x4_column_major(array)
        }
        _ => {
            return Err("DOMMatrix constructor smoke needs a 6- or 16-number array".into());
        }
    };
    Ok((matrix, member))
}

fn dom_matrix_member_value(matrix: DOMMatrix, member: &str) -> Result<String, String> {
    if let Some(rest) = member.strip_prefix(".translate(") {
        let (args, tail) = parse_number_args_call(rest)?;
        if args.is_empty() || args.len() > 3 {
            return Err("DOMMatrix.translate smoke needs one to three numbers".into());
        }
        let next = matrix.translate(
            args[0],
            args.get(1).copied().unwrap_or(0.0),
            args.get(2).copied().unwrap_or(0.0),
        );
        return dom_matrix_member_value(next, tail);
    }
    if let Some(rest) = member.strip_prefix(".scale(") {
        let (args, tail) = parse_number_args_call(rest)?;
        if args.is_empty() || args.len() > 3 {
            return Err("DOMMatrix.scale smoke needs one to three numbers".into());
        }
        let next = matrix.scale(
            args[0],
            args.get(1).copied().unwrap_or(args[0]),
            args.get(2).copied().unwrap_or(1.0),
        );
        return dom_matrix_member_value(next, tail);
    }
    if let Some(rest) = member.strip_prefix(".rotate(") {
        let (args, tail) = parse_number_args_call(rest)?;
        if args.len() != 1 {
            return Err("DOMMatrix.rotate smoke needs one number".into());
        }
        return dom_matrix_member_value(matrix.rotate(args[0]), tail);
    }
    if let Some(rest) = member.strip_prefix(".skewX(") {
        let (args, tail) = parse_number_args_call(rest)?;
        if args.len() != 1 {
            return Err("DOMMatrix.skewX smoke needs one number".into());
        }
        return dom_matrix_member_value(matrix.skew_x(args[0]), tail);
    }
    if let Some(rest) = member.strip_prefix(".skewY(") {
        let (args, tail) = parse_number_args_call(rest)?;
        if args.len() != 1 {
            return Err("DOMMatrix.skewY smoke needs one number".into());
        }
        return dom_matrix_member_value(matrix.skew_y(args[0]), tail);
    }
    if let Some(tail) = member.strip_prefix(".flipX()") {
        return dom_matrix_member_value(matrix.flip_x(), tail);
    }
    if let Some(tail) = member.strip_prefix(".flipY()") {
        return dom_matrix_member_value(matrix.flip_y(), tail);
    }
    if let Some(tail) = member.strip_prefix(".inverse()") {
        let inverse = matrix
            .inverse()
            .ok_or_else(|| "DOMMatrix.inverse smoke got a singular matrix".to_owned())?;
        return dom_matrix_member_value(inverse, tail);
    }
    if let Some(rest) = member.strip_prefix(".transformPoint(") {
        let rest = rest
            .trim_start()
            .strip_prefix("new DOMPoint(")
            .ok_or_else(|| "DOMMatrix.transformPoint smoke needs new DOMPoint(...)".to_owned())?;
        let (point, tail) = parse_dom_point_constructor(rest)?;
        let tail = tail
            .trim_start()
            .strip_prefix(')')
            .ok_or_else(|| "unsupported DOMMatrix.transformPoint tail".to_owned())?;
        return dom_point_member_value(matrix.transform_point(point), tail);
    }

    let value = match member {
        "" => "[object DOMMatrix]".into(),
        ".is2D" => matrix.is_2d().to_string(),
        ".a" | ".m11" => render_number(matrix.a()),
        ".b" | ".m12" => render_number(matrix.b()),
        ".c" | ".m21" => render_number(matrix.c()),
        ".d" | ".m22" => render_number(matrix.d()),
        ".e" | ".m41" => render_number(matrix.e()),
        ".f" | ".m42" => render_number(matrix.f()),
        ".m13" => render_number(matrix.m13),
        ".m14" => render_number(matrix.m14),
        ".m23" => render_number(matrix.m23),
        ".m24" => render_number(matrix.m24),
        ".m31" => render_number(matrix.m31),
        ".m32" => render_number(matrix.m32),
        ".m33" => render_number(matrix.m33),
        ".m34" => render_number(matrix.m34),
        ".m43" => render_number(matrix.m43),
        ".m44" => render_number(matrix.m44),
        _ => return Err("unsupported DOMMatrix eval member expression".into()),
    };
    Ok(value)
}

fn navigator_expr(expr: &str) -> Option<Result<String, String>> {
    let normalized = expr.strip_prefix("window.").unwrap_or(expr);
    let value = match normalized {
        "navigator.onLine" => "true".into(),
        "navigator.cookieEnabled" => "true".into(),
        "navigator.language" => "en-US".into(),
        "navigator.languages.length" => "1".into(),
        "navigator.languages[0]" => "en-US".into(),
        "navigator.userAgent" => user_agent_string(),
        "navigator.hardwareConcurrency >= 1" => "true".into(),
        _ => {
            let needle = normalized
                .strip_prefix("navigator.userAgent.includes(")
                .and_then(|tail| tail.strip_suffix(')'))
                .and_then(js_string_literal)?;
            return Some(Ok(user_agent_string().contains(&needle).to_string()));
        }
    };
    Some(Ok(value))
}

fn user_agent_string() -> String {
    "Vixen/0.1".into()
}

fn storage_expr(expr: &str) -> Option<Result<String, String>> {
    let normalized = expr.strip_prefix("window.").unwrap_or(expr);
    let member = normalized
        .strip_prefix("localStorage")
        .or_else(|| normalized.strip_prefix("sessionStorage"))?;
    let value = match member {
        ".length" => Ok("0".into()),
        _ => {
            if method_usize_arg(member, ".key(").is_some() {
                Ok("null".into())
            } else if let Some(key) = method_string_arg(member, ".getItem(") {
                validate_storage_lookup_key(&key).map(|()| "null".into())
            } else if let Some(key) = method_string_arg(member, ".removeItem(") {
                validate_storage_lookup_key(&key).map(|()| "undefined".into())
            } else if member == ".clear()" {
                Ok("undefined".into())
            } else {
                Err("unsupported Storage eval member expression".into())
            }
        }
    };
    Some(value)
}

fn validate_storage_lookup_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Ok(());
    }
    validate_storage_key(key).map_err(|err| err.to_string())
}

fn event_expr(expr: &str) -> Option<Result<String, String>> {
    if let Some(rest) = expr.strip_prefix("new Event(") {
        return Some(
            parse_event_constructor_call(rest, false)
                .and_then(|(event, member)| event_member_value(&event, member)),
        );
    }
    if let Some(rest) = expr.strip_prefix("new CustomEvent(") {
        return Some(
            parse_event_constructor_call(rest, true)
                .and_then(|(event, member)| event_member_value(&event, member)),
        );
    }
    None
}

#[derive(Debug, Clone)]
struct EventProjection {
    event_type: String,
    bubbles: bool,
    cancelable: bool,
    composed: bool,
    detail: Option<String>,
}

fn parse_event_constructor_call(
    input: &str,
    custom: bool,
) -> Result<(EventProjection, &str), String> {
    let (event_type, rest) = parse_js_string_prefix(input)
        .ok_or_else(|| "Event constructor smoke needs a string type".to_owned())?;
    let mut event = EventProjection {
        event_type,
        bubbles: false,
        cancelable: false,
        composed: false,
        detail: None,
    };

    let rest = rest.trim_start();
    let rest = if let Some(rest) = rest.strip_prefix(')') {
        rest
    } else {
        let rest = rest
            .strip_prefix(',')
            .ok_or_else(|| "unsupported Event constructor arguments".to_owned())?
            .trim_start();
        let (body, rest) = parse_simple_object_body(rest)
            .ok_or_else(|| "Event init smoke needs a simple object".to_owned())?;
        apply_event_init(&mut event, &body, custom)?;
        rest.trim_start()
            .strip_prefix(')')
            .ok_or_else(|| "unsupported Event constructor tail".to_owned())?
    };
    Ok((event, rest))
}

fn parse_simple_object_body(input: &str) -> Option<(String, &str)> {
    let body = input.strip_prefix('{')?;
    let end = body.find('}')?;
    let (body, rest) = body.split_at(end);
    Some((body.to_owned(), rest.strip_prefix('}')?))
}

fn apply_event_init(event: &mut EventProjection, body: &str, custom: bool) -> Result<(), String> {
    for part in body
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let Some((key, value)) = part.split_once(':') else {
            return Err("Event init entries need key: value".into());
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "bubbles" => event.bubbles = parse_bool_literal(value)?,
            "cancelable" => event.cancelable = parse_bool_literal(value)?,
            "composed" => event.composed = parse_bool_literal(value)?,
            "detail" if custom => {
                event.detail = Some(
                    js_string_literal(value)
                        .ok_or_else(|| "CustomEvent.detail smoke needs a string".to_owned())?,
                );
            }
            "detail" => {}
            _ => return Err("unsupported Event init member".into()),
        }
    }
    Ok(())
}

fn parse_bool_literal(value: &str) -> Result<bool, String> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err("Event init boolean value must be true or false".into()),
    }
}

fn parse_bool_prefix(input: &str) -> Option<(bool, &str)> {
    let input = input.trim_start();
    if let Some(rest) = input.strip_prefix("true") {
        return Some((true, rest));
    }
    input.strip_prefix("false").map(|rest| (false, rest))
}

fn event_member_value(event: &EventProjection, member: &str) -> Result<String, String> {
    match member {
        ".type" => Ok(event.event_type.clone()),
        ".bubbles" => Ok(event.bubbles.to_string()),
        ".cancelable" => Ok(event.cancelable.to_string()),
        ".composed" => Ok(event.composed.to_string()),
        ".defaultPrevented" | ".isTrusted" => Ok("false".into()),
        ".eventPhase" => Ok("0".into()),
        ".target === null" | ".currentTarget === null" => Ok("true".into()),
        ".composedPath().length" => Ok("0".into()),
        ".detail" => Ok(event.detail.clone().unwrap_or_else(|| "null".into())),
        _ => Err("unsupported Event eval member expression".into()),
    }
}

fn dispatch_event_value(inner: &str) -> Result<String, String> {
    let (_, tail) = if let Some(rest) = inner.strip_prefix("new Event(") {
        parse_event_constructor_call(rest, false)?
    } else if let Some(rest) = inner.strip_prefix("new CustomEvent(") {
        parse_event_constructor_call(rest, true)?
    } else {
        return Err("dispatchEvent smoke needs new Event or new CustomEvent".into());
    };
    if tail.is_empty() {
        Ok("true".into())
    } else {
        Err("unsupported dispatchEvent tail".into())
    }
}

fn history_state_value(history: &SessionHistory) -> String {
    match history.state() {
        Some(_) => "[object Object]".into(),
        None => "null".into(),
    }
}

fn document_range() -> DomRange {
    let document = NodeRef::new(0, DocumentOrder(0));
    let boundary = Boundary::at(document, 0);
    DomRange::new_unchecked(boundary, boundary)
}

fn url_can_parse_expr(expr: &str) -> Option<Result<String, String>> {
    let rest = expr
        .strip_prefix("URL.canParse(")
        .or_else(|| expr.strip_prefix("window.URL.canParse("))?;
    let Some((input, base, tail)) = parse_one_or_two_string_arg_call(rest) else {
        return Some(Err("URL.canParse smoke needs one or two strings".into()));
    };
    if !tail.is_empty() {
        return Some(Err("unsupported URL.canParse eval member expression".into()));
    }
    Some(Ok(can_parse_url(&input, base.as_deref()).to_string()))
}

fn can_parse_url(input: &str, base: Option<&str>) -> bool {
    match base {
        Some(base) => parse_url(base)
            .and_then(|base| parse_url_with_base(input, &base))
            .is_ok(),
        None => parse_url(input).is_ok(),
    }
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

fn url_expr(expr: &str) -> Option<Result<String, String>> {
    let rest = expr.strip_prefix("new URL(")?;
    let (input, base, member) = parse_one_or_two_string_arg_call(rest)?;
    let url = match base {
        Some(base) => parse_url(&base)
            .and_then(|base| parse_url_with_base(&input, &base))
            .map_err(|err| err.to_string()),
        None => parse_url(&input).map_err(|err| err.to_string()),
    };
    Some(url.and_then(|url| url_member_value(&url, member)))
}

fn url_member_value(url: &WhatwgUrl, member: &str) -> Result<String, String> {
    match member {
        ".href" | ".toString()" => Ok(url.serialize()),
        ".origin" => Ok(url.origin().unwrap_or_else(|| "null".into())),
        ".protocol" => Ok(format!("{}:", url.scheme)),
        ".username" => Ok(url.username.clone()),
        ".password" => Ok(url.password.clone()),
        ".host" => Ok(url_host(url, true)),
        ".hostname" => Ok(url_host(url, false)),
        ".port" => Ok(url_port(url)),
        ".pathname" => Ok(url_pathname(url)),
        ".search" => Ok(url
            .query
            .as_ref()
            .map(|query| format!("?{query}"))
            .unwrap_or_default()),
        ".hash" => Ok(url
            .fragment
            .as_ref()
            .map(|fragment| format!("#{fragment}"))
            .unwrap_or_default()),
        _ => {
            if member == ".searchParams.size" {
                let params = UrlSearchParams::parse(url.query.as_deref().unwrap_or_default());
                return Ok(params.len().to_string());
            }
            if let Some(name) = member.strip_prefix(".searchParams.get(") {
                let name = js_string_literal(name.strip_suffix(')').unwrap_or(name))
                    .ok_or_else(|| "URL.searchParams.get smoke needs a string arg".to_owned())?;
                let params = UrlSearchParams::parse(url.query.as_deref().unwrap_or_default());
                return Ok(params.get(&name).unwrap_or_else(|| "null".into()));
            }
            if let Some(name) = member.strip_prefix(".searchParams.has(") {
                let name = js_string_literal(name.strip_suffix(')').unwrap_or(name))
                    .ok_or_else(|| "URL.searchParams.has smoke needs a string arg".to_owned())?;
                let params = UrlSearchParams::parse(url.query.as_deref().unwrap_or_default());
                return Ok(params.has(&name).to_string());
            }
            if let Some(rest) = member.strip_prefix(".searchParams.getAll(") {
                let (name, tail) = parse_single_string_arg_call(rest)
                    .ok_or_else(|| "URL.searchParams.getAll smoke needs a string arg".to_owned())?;
                let params = UrlSearchParams::parse(url.query.as_deref().unwrap_or_default());
                let values = params.get_all(&name);
                return match tail {
                    ".length" => Ok(values.len().to_string()),
                    _ => bracket_usize(tail)
                        .map(|index| {
                            values
                                .get(index)
                                .cloned()
                                .unwrap_or_else(|| "undefined".into())
                        })
                        .ok_or_else(|| "unsupported URL.searchParams.getAll eval member".into()),
                };
            }
            Err("unsupported URL eval member expression".into())
        }
    }
}

fn url_host(url: &WhatwgUrl, include_port: bool) -> String {
    let Some(host) = url.host.as_ref().filter(|host| !host.is_empty()) else {
        return String::new();
    };
    let mut out = String::new();
    if host.contains(':') {
        out.push('[');
        out.push_str(host);
        out.push(']');
    } else {
        out.push_str(host);
    }
    if include_port {
        let port = url_port(url);
        if !port.is_empty() {
            out.push(':');
            out.push_str(&port);
        }
    }
    out
}

fn url_port(url: &WhatwgUrl) -> String {
    url.port
        .filter(|port| Some(*port) != crate::whatwg_url::default_port(&url.scheme))
        .map(|port| port.to_string())
        .unwrap_or_default()
}

fn url_pathname(url: &WhatwgUrl) -> String {
    if crate::whatwg_url::is_special_scheme(&url.scheme) || url.host.is_some() {
        if url.path.is_empty() {
            return "/".into();
        }
        return format!("/{}", url.path.join("/"));
    }
    url.path.first().cloned().unwrap_or_default()
}

fn url_search_params_expr(expr: &str) -> Option<Result<String, String>> {
    let rest = expr.strip_prefix("new URLSearchParams(")?;
    let (params, member) = match parse_url_search_params_constructor_call(rest) {
        Some(Ok(parsed)) => parsed,
        Some(Err(err)) => return Some(Err(err)),
        None => return None,
    };
    Some(url_search_params_member_value(&params, member))
}

fn parse_url_search_params_constructor_call(
    input: &str,
) -> Option<Result<(UrlSearchParams, &str), String>> {
    let rest = input.trim_start();
    if let Some(member) = rest.strip_prefix(')') {
        return Some(Ok((UrlSearchParams::new(), member)));
    }
    if let Some((input, member)) = parse_single_string_arg_call(rest) {
        return Some(Ok((UrlSearchParams::parse(&input), member)));
    }
    let (records, rest) = match parse_header_records_prefix(rest)? {
        Ok(parsed) => parsed,
        Err(err) => return Some(Err(err)),
    };
    let member = rest.trim_start().strip_prefix(')')?;
    let mut params = UrlSearchParams::new();
    for (name, value) in records {
        params.append(name, value);
    }
    Some(Ok((params, member)))
}

fn url_search_params_member_value(
    params: &UrlSearchParams,
    member: &str,
) -> Result<String, String> {
    let value = match member {
        ".size" => params.len().to_string(),
        ".toString()" => params.serialize(),
        _ => {
            if let Some((name, value)) = method_two_string_args(member, ".has(") {
                return Ok(params.has_pair(&name, &value).to_string());
            }
            if let Some(name) = method_string_arg(member, ".get(") {
                return Ok(params.get(&name).unwrap_or_else(|| "null".to_owned()));
            }
            if let Some(name) = method_string_arg(member, ".has(") {
                return Ok(params.has(&name).to_string());
            }
            if let Some(rest) = member.strip_prefix(".getAll(") {
                let (name, tail) = parse_single_string_arg_call(rest)
                    .ok_or_else(|| "URLSearchParams.getAll smoke needs a string arg".to_owned())?;
                let values = params.get_all(&name);
                return match tail {
                    ".length" => Ok(values.len().to_string()),
                    _ => bracket_usize(tail)
                        .map(|index| {
                            values
                                .get(index)
                                .cloned()
                                .unwrap_or_else(|| "undefined".into())
                        })
                        .ok_or_else(|| "unsupported URLSearchParams.getAll eval member".into()),
                };
            }
            if let Some(value) = url_search_params_iterator_value(params, member) {
                return value;
            }
            return Err("unsupported URLSearchParams eval member expression".into());
        }
    };
    Ok(value)
}

fn url_search_params_iterator_value(
    params: &UrlSearchParams,
    member: &str,
) -> Option<Result<String, String>> {
    let first = params.iter().next();
    match member {
        ".keys().next().done" | ".values().next().done" | ".entries().next().done" => {
            Some(Ok(first.is_none().to_string()))
        }
        ".keys().next().value" => Some(Ok(first
            .as_ref()
            .map(|(name, _)| (*name).to_owned())
            .unwrap_or_else(|| "undefined".into()))),
        ".values().next().value" => Some(Ok(first
            .as_ref()
            .map(|(_, value)| (*value).to_owned())
            .unwrap_or_else(|| "undefined".into()))),
        ".entries().next().value[0]" => Some(Ok(first
            .as_ref()
            .map(|(name, _)| (*name).to_owned())
            .unwrap_or_else(|| "undefined".into()))),
        ".entries().next().value[1]" => Some(Ok(first
            .as_ref()
            .map(|(_, value)| (*value).to_owned())
            .unwrap_or_else(|| "undefined".into()))),
        _ => None,
    }
}

fn parse_form_data_constructor_expr(expr: &str) -> Option<Result<(String, &str), String>> {
    if let Some(rest) = expr.strip_prefix("new FormData(document.querySelector(") {
        let Some((selector, tail)) = parse_single_string_arg_call(rest) else {
            return Some(Err(
                "FormData smoke needs document.querySelector with a string selector".into(),
            ));
        };
        let Some(member) = tail.trim_start().strip_prefix(')') else {
            return Some(Err("unsupported FormData constructor tail".into()));
        };
        let Some(form_id) = simple_id_from_hash_selector(&selector) else {
            return Some(Err(
                "FormData smoke only accepts a simple id selector".into()
            ));
        };
        return Some(Ok((form_id, member)));
    }
    if let Some(rest) = expr.strip_prefix("new FormData(document.getElementById(") {
        let Some((id, tail)) = parse_single_string_arg_call(rest) else {
            return Some(Err(
                "FormData smoke needs document.getElementById with a string id".into(),
            ));
        };
        let Some(member) = tail.trim_start().strip_prefix(')') else {
            return Some(Err("unsupported FormData constructor tail".into()));
        };
        if !is_simple_id_selector(&id) {
            return Some(Err("FormData smoke only accepts simple ids".into()));
        }
        return Some(Ok((id, member)));
    }
    None
}

fn simple_id_from_hash_selector(selector: &str) -> Option<String> {
    let id = selector.strip_prefix('#')?;
    is_simple_id_selector(id).then(|| id.to_owned())
}

fn form_data_member_value(entries: &[FormEntry], member: &str) -> Result<String, String> {
    if let Some(rest) = member.strip_prefix(".get(") {
        let (name, tail) = parse_single_string_arg_call(rest)
            .ok_or_else(|| "FormData.get smoke needs a string name".to_owned())?;
        let entry = entries.iter().find(|entry| entry.name == name);
        return match (entry, tail) {
            (None, "") => Ok("null".into()),
            (None, " === null") => Ok("true".into()),
            (Some(_), " === null") => Ok("false".into()),
            (None, _) => Err("FormData.get smoke matched no entry".into()),
            (Some(entry), tail) => form_data_entry_member_value(entry, tail),
        };
    }
    if let Some(name) = method_string_arg(member, ".has(") {
        return Ok(entries.iter().any(|entry| entry.name == name).to_string());
    }
    if let Some(rest) = member.strip_prefix(".getAll(") {
        let (name, tail) = parse_single_string_arg_call(rest)
            .ok_or_else(|| "FormData.getAll smoke needs a string name".to_owned())?;
        let values: Vec<_> = entries.iter().filter(|entry| entry.name == name).collect();
        return match tail {
            ".length" => Ok(values.len().to_string()),
            _ => bracket_usize(tail)
                .map(|index| {
                    values
                        .get(index)
                        .map(|entry| form_data_entry_member_value(entry, ""))
                        .unwrap_or_else(|| Ok("undefined".into()))
                })
                .unwrap_or_else(|| Err("unsupported FormData.getAll eval member".into())),
        };
    }
    if let Some(value) = form_data_iterator_member_value(entries, member) {
        return value;
    }
    Err("unsupported FormData eval member expression".into())
}

fn form_data_iterator_member_value(
    entries: &[FormEntry],
    member: &str,
) -> Option<Result<String, String>> {
    let first = entries.first();
    match member {
        ".keys().next().done" | ".values().next().done" | ".entries().next().done" => {
            Some(Ok(first.is_none().to_string()))
        }
        ".keys().next().value" => Some(Ok(first
            .map(|entry| entry.name.clone())
            .unwrap_or_else(|| "undefined".into()))),
        ".values().next().value" => Some(first.map_or_else(
            || Ok("undefined".into()),
            |entry| form_data_entry_member_value(entry, ""),
        )),
        ".entries().next().value[0]" => Some(Ok(first
            .map(|entry| entry.name.clone())
            .unwrap_or_else(|| "undefined".into()))),
        ".entries().next().value[1]" => Some(first.map_or_else(
            || Ok("undefined".into()),
            |entry| form_data_entry_member_value(entry, ""),
        )),
        _ => None,
    }
}

fn form_data_entry_member_value(entry: &FormEntry, member: &str) -> Result<String, String> {
    match (&entry.value, member) {
        (FormEntryValue::Text(value), "") => Ok(value.clone()),
        (
            FormEntryValue::File {
                filename,
                content_type,
                body,
            },
            member,
        ) => match member {
            "" => Ok("[object File]".into()),
            ".name" => Ok(filename.clone()),
            ".type" => Ok(content_type.clone()),
            ".size" => Ok(body.len().to_string()),
            _ => Err("unsupported FormData file eval member".into()),
        },
        (FormEntryValue::Text(_), _) => Err("unsupported FormData text eval member".into()),
    }
}

fn text_encoder_expr(expr: &str) -> Option<Result<String, String>> {
    let member = expr.strip_prefix("new TextEncoder()")?;
    match member {
        ".encoding" => Some(Ok(TEXT_ENCODER_ENCODING.to_owned())),
        _ => {
            if let Some(rest) = member.strip_prefix(".encodeInto(") {
                let Some((input, dest_len, tail)) = parse_text_encoder_encode_into_call(rest)
                else {
                    return Some(Err(
                        "TextEncoder.encodeInto smoke needs a string and Uint8Array".into(),
                    ));
                };
                if dest_len > 1_000_000 {
                    return Some(Err("TextEncoder.encodeInto destination too large".into()));
                }
                let mut dest = vec![0; dest_len];
                let result = TextEncoder.encode_into(&input, &mut dest);
                return Some(match tail {
                    ".read" => Ok(result.read_utf16.to_string()),
                    ".written" => Ok(result.written.to_string()),
                    _ => Err("unsupported TextEncoder.encodeInto eval member".into()),
                });
            }
            let rest = member.strip_prefix(".encode(")?;
            let (input, tail) = parse_single_string_arg_call(rest)?;
            let bytes = TextEncoder.encode(&input);
            Some(match tail {
                ".length" => Ok(bytes.len().to_string()),
                _ => bracket_usize(tail)
                    .map(|index| {
                        bytes
                            .get(index)
                            .map(u8::to_string)
                            .unwrap_or_else(|| "undefined".into())
                    })
                    .ok_or_else(|| "unsupported TextEncoder.encode eval member".into()),
            })
        }
    }
}

fn parse_text_encoder_encode_into_call(input: &str) -> Option<(String, usize, &str)> {
    let (source, rest) = parse_js_string_prefix(input)?;
    let rest = rest.trim_start().strip_prefix(',')?.trim_start();
    let rest = rest.strip_prefix("new Uint8Array(")?;
    let (len, rest) = parse_usize_arg_call(rest)?;
    let tail = rest.trim_start().strip_prefix(')')?;
    Some((source, len, tail))
}

fn text_decoder_expr(expr: &str) -> Option<Result<String, String>> {
    let rest = expr.strip_prefix("new TextDecoder(")?;
    let (decoder, member) = match parse_text_decoder_constructor_call(rest) {
        Some(Ok(parsed)) => parsed,
        Some(Err(err)) => return Some(Err(err)),
        None => return None,
    };
    match member {
        ".encoding" => Some(Ok(TEXT_DECODER_ENCODING.to_owned())),
        ".fatal" => Some(Ok(decoder.fatal().to_string())),
        ".ignoreBOM" => Some(Ok(decoder.ignore_bom().to_string())),
        _ => {
            let rest = member.strip_prefix(".decode(")?;
            let (bytes, tail) = parse_byte_array_arg_call(rest)?;
            if !tail.is_empty() {
                return Some(Err("unsupported TextDecoder.decode eval member".into()));
            }
            Some(decoder.decode(&bytes).map_err(|err| err.to_string()))
        }
    }
}

fn parse_text_decoder_constructor_call(input: &str) -> Option<Result<(TextDecoder, &str), String>> {
    let mut rest = input.trim_start();
    if let Some(member) = rest.strip_prefix(')') {
        return Some(Ok((TextDecoder::utf8(), member)));
    }

    let (label, tail) = parse_js_string_prefix(rest)?;
    rest = tail.trim_start();
    let mut fatal = false;
    let mut ignore_bom = false;
    if let Some(tail) = rest.strip_prefix(',') {
        let (options, tail) = match parse_text_decoder_options_prefix(tail.trim_start())? {
            Ok(parsed) => parsed,
            Err(err) => return Some(Err(err)),
        };
        fatal = options.fatal;
        ignore_bom = options.ignore_bom;
        rest = tail.trim_start();
    }

    let member = rest.strip_prefix(')')?;
    Some(
        TextDecoder::new(&label, fatal, ignore_bom)
            .map(|decoder| (decoder, member))
            .map_err(|err| err.to_string()),
    )
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct TextDecoderOptions {
    fatal: bool,
    ignore_bom: bool,
}

fn parse_text_decoder_options_prefix(
    input: &str,
) -> Option<Result<(TextDecoderOptions, &str), String>> {
    let mut options = TextDecoderOptions::default();
    let mut rest = input.trim_start().strip_prefix('{')?.trim_start();
    if let Some(tail) = rest.strip_prefix('}') {
        return Some(Ok((options, tail)));
    }

    loop {
        let Some((key, tail)) = parse_object_key_prefix(rest) else {
            return Some(Err("TextDecoder options need simple keys".into()));
        };
        rest = tail.trim_start().strip_prefix(':')?.trim_start();
        let Some((value, tail)) = parse_bool_prefix(rest) else {
            return Some(Err("TextDecoder option values need booleans".into()));
        };
        match key.as_str() {
            "fatal" => options.fatal = value,
            "ignoreBOM" => options.ignore_bom = value,
            _ => return Some(Err("unsupported TextDecoder option".into())),
        }
        rest = tail.trim_start();
        if let Some(tail) = rest.strip_prefix('}') {
            return Some(Ok((options, tail)));
        }
        rest = rest.strip_prefix(',')?.trim_start();
    }
}

fn base64_expr(expr: &str) -> Option<Result<String, String>> {
    if let Some(rest) = expr
        .strip_prefix("btoa(")
        .or_else(|| expr.strip_prefix("window.btoa("))
    {
        let Some((input, tail)) = parse_single_string_arg_call(rest) else {
            return Some(Err("btoa smoke needs a string argument".into()));
        };
        if !tail.is_empty() {
            return Some(Err("unsupported btoa eval member expression".into()));
        }
        let mut bytes = Vec::with_capacity(input.len());
        for ch in input.chars() {
            if ch as u32 > 0xff {
                return Some(Err("btoa input must be Latin-1".into()));
            }
            bytes.push(ch as u8);
        }
        return Some(Ok(BASE64_STANDARD.encode(bytes)));
    }
    if let Some(rest) = expr
        .strip_prefix("atob(")
        .or_else(|| expr.strip_prefix("window.atob("))
    {
        let Some((input, tail)) = parse_single_string_arg_call(rest) else {
            return Some(Err("atob smoke needs a string argument".into()));
        };
        if !tail.is_empty() {
            return Some(Err("unsupported atob eval member expression".into()));
        }
        return Some(
            BASE64_STANDARD
                .decode(input.as_bytes())
                .map(|bytes| bytes.into_iter().map(char::from).collect())
                .map_err(|err| err.to_string()),
        );
    }
    None
}

fn dom_parser_expr(expr: &str) -> Option<Result<String, String>> {
    let rest = expr.strip_prefix("new DOMParser().parseFromString(")?;
    let Some((html, mime, member)) = parse_one_or_two_string_arg_call(rest) else {
        return Some(Err(
            "DOMParser.parseFromString smoke needs html and mime strings".into(),
        ));
    };
    let Some(mime) = mime else {
        return Some(Err(
            "DOMParser.parseFromString smoke needs a mime string".into()
        ));
    };
    if !mime.eq_ignore_ascii_case("text/html") {
        return Some(Err("DOMParser smoke only supports text/html".into()));
    }
    Some(
        Page::from_html("about:blank", &html)
            .map_err(|err| err.to_string())
            .and_then(|page| {
                let nested = format!("document{member}");
                page.evaluate_dom_expression(&nested)
                    .unwrap_or_else(|| Err("unsupported DOMParser eval member expression".into()))
            }),
    )
}

fn collection_length_arg(expr: &str, prefix: &str) -> Option<String> {
    let inner = expr.strip_prefix(prefix)?.strip_suffix(").length")?;
    js_string_literal(inner)
}

fn parse_byte_array_arg_call(input: &str) -> Option<(Vec<u8>, &str)> {
    let body = input.strip_prefix('[')?;
    let end = body.find(']')?;
    let (items, tail) = body.split_at(end);
    let tail = tail.strip_prefix(']')?;
    let tail = tail.strip_prefix(')')?;
    let mut bytes = Vec::new();
    if !items.trim().is_empty() {
        for item in items.split(',') {
            let value = item.trim().parse::<u8>().ok()?;
            bytes.push(value);
        }
    }
    Some((bytes, tail))
}

fn parse_u64_arg_call(input: &str) -> Option<(u64, &str)> {
    let end = input.find(')')?;
    let (raw, tail) = input.split_at(end);
    let value = raw.trim().parse::<u64>().ok()?;
    Some((value, tail.strip_prefix(')')?))
}

fn parse_usize_arg_call(input: &str) -> Option<(usize, &str)> {
    let end = input.find(')')?;
    let (raw, tail) = input.split_at(end);
    let value = raw.trim().parse::<usize>().ok()?;
    Some((value, tail.strip_prefix(')')?))
}

fn parse_number_prefix(input: &str) -> Option<(f64, &str)> {
    let end = input
        .char_indices()
        .take_while(|(_, ch)| ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+'))
        .map(|(idx, ch)| idx + ch.len_utf8())
        .last()?;
    let (raw, rest) = input.split_at(end);
    Some((raw.parse::<f64>().ok()?, rest))
}

fn parse_i64_prefix(input: &str) -> Option<(i64, &str)> {
    let input = input.trim_start();
    let mut end = 0usize;
    for (idx, ch) in input.char_indices() {
        if idx == 0 && matches!(ch, '-' | '+') {
            end = ch.len_utf8();
            continue;
        }
        if ch.is_ascii_digit() {
            end = idx + ch.len_utf8();
        } else {
            break;
        }
    }
    if end == 0 || matches!(input[..end].chars().last(), Some('-' | '+')) {
        return None;
    }
    Some((input[..end].parse().ok()?, &input[end..]))
}

fn parse_u16_prefix(input: &str) -> Option<(u16, &str)> {
    let input = input.trim_start();
    let end = input
        .char_indices()
        .take_while(|(_, ch)| ch.is_ascii_digit())
        .map(|(idx, ch)| idx + ch.len_utf8())
        .last()?;
    Some((input[..end].parse().ok()?, &input[end..]))
}

fn parse_number_args_call(input: &str) -> Result<(Vec<f64>, &str), String> {
    let end = input
        .find(')')
        .ok_or_else(|| "numeric argument list needs a closing ')'".to_owned())?;
    let (raw_args, tail) = input.split_at(end);
    let values = parse_comma_number_list(raw_args)?;
    Ok((values, tail.strip_prefix(')').expect("split at ')'")))
}

fn parse_number_array_prefix(input: &str) -> Result<(Vec<f64>, &str), String> {
    let body = input
        .trim_start()
        .strip_prefix('[')
        .ok_or_else(|| "numeric array needs '['".to_owned())?;
    let end = body
        .find(']')
        .ok_or_else(|| "numeric array needs ']'".to_owned())?;
    let (raw_args, tail) = body.split_at(end);
    let values = parse_comma_number_list(raw_args)?;
    Ok((values, tail.strip_prefix(']').expect("split at ']'")))
}

fn parse_comma_number_list(raw_args: &str) -> Result<Vec<f64>, String> {
    if raw_args.trim().is_empty() {
        return Ok(Vec::new());
    }
    raw_args
        .split(',')
        .map(str::trim)
        .map(parse_finite_number)
        .collect()
}

fn parse_finite_number(raw: &str) -> Result<f64, String> {
    let value = raw
        .parse::<f64>()
        .map_err(|_| "numeric smoke argument must be a number".to_owned())?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err("numeric smoke argument must be finite".into())
    }
}

fn render_number(value: f64) -> String {
    if value.fract() == 0.0 {
        (value as i64).to_string()
    } else {
        value.to_string()
    }
}

fn parse_object_key_prefix(input: &str) -> Option<(String, &str)> {
    if let Some(parsed) = parse_js_string_prefix(input) {
        return Some(parsed);
    }
    let end = input
        .char_indices()
        .take_while(|(idx, ch)| {
            if *idx == 0 {
                ch.is_ascii_alphabetic() || *ch == '_'
            } else {
                ch.is_ascii_alphanumeric() || *ch == '_'
            }
        })
        .map(|(idx, ch)| idx + ch.len_utf8())
        .last()?;
    let (ident, rest) = input.split_at(end);
    if is_simple_ident(ident) {
        Some((ident.to_owned(), rest))
    } else {
        None
    }
}

fn parse_string_or_pathname_object_arg_call(input: &str) -> Option<(String, &str)> {
    let input = input.trim_start();
    if input.starts_with('{') {
        let (pathname, rest) = parse_pathname_object_prefix(input)?;
        let rest = rest.trim_start().strip_prefix(')')?;
        return Some((pathname, rest));
    }
    parse_single_string_arg_call(input)
}

fn parse_pathname_object_prefix(input: &str) -> Option<(String, &str)> {
    let rest = input.strip_prefix('{')?.trim_start();
    let (key, rest) = parse_object_key_prefix(rest)?;
    if key != "pathname" {
        return None;
    }
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let (pathname, rest) = parse_js_string_prefix(rest)?;
    let rest = rest.trim_start().strip_prefix('}')?;
    Some((pathname, rest))
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

fn image_current_src(info: &ElementInfo) -> String {
    if info.tag != "img" {
        return String::new();
    }
    let fallback = element_attr(info, "src").unwrap_or_default();
    let Some(srcset) = element_attr(info, "srcset") else {
        return fallback;
    };
    let sizes = element_attr(info, "sizes").unwrap_or_default();
    let viewport = Viewport::new(800.0, 600.0, 1.0);
    select_responsive_image_source(&srcset, &sizes, &viewport).unwrap_or(fallback)
}

fn token_list_member_value(
    info: &ElementInfo,
    attribute: &str,
    member: &str,
) -> Result<String, String> {
    let list = DomTokenList::parse(&element_attr(info, attribute).unwrap_or_default());
    match member {
        ".length" => Ok(list.len().to_string()),
        ".value" | ".toString()" => Ok(list.serialize()),
        _ => {
            if let Some(token) = method_string_arg(member, ".contains(") {
                return list
                    .contains(&token)
                    .map(|contains| contains.to_string())
                    .map_err(|err| err.to_string());
            }
            if let Some(index) = method_usize_arg(member, ".item(") {
                return Ok(list.item(index).unwrap_or("null").to_owned());
            }
            if let Some(index) = bracket_usize(member) {
                return Ok(list.item(index).unwrap_or("undefined").to_owned());
            }
            Err("unsupported DOMTokenList eval member expression".into())
        }
    }
}

fn dataset_member_value(info: &ElementInfo, member: &str) -> Result<String, String> {
    let Some(property) = dataset_member_property(member) else {
        return Err("unsupported dataset eval member expression".into());
    };
    let dataset = collect_dataset(
        info.attributes
            .iter()
            .map(|(name, value)| (name.as_str(), value.clone())),
    );
    Ok(dataset
        .into_iter()
        .find(|(name, _)| name == &property)
        .map(|(_, value)| value)
        .unwrap_or_else(|| "undefined".to_owned()))
}

fn dataset_member_property(member: &str) -> Option<String> {
    if let Some(property) = member.strip_prefix('.') {
        return is_simple_ident(property).then(|| property.to_owned());
    }
    let inner = member.strip_prefix('[')?.strip_suffix(']')?;
    js_string_literal(inner)
}

fn method_usize_arg(member: &str, prefix: &str) -> Option<usize> {
    let inner = member.strip_prefix(prefix)?.strip_suffix(')')?;
    inner.trim().parse().ok()
}

fn bracket_usize(member: &str) -> Option<usize> {
    let inner = member.strip_prefix('[')?.strip_suffix(']')?;
    inner.trim().parse().ok()
}

fn will_validate(info: &ElementInfo) -> bool {
    if element_has_attr(info, "disabled") {
        return false;
    }

    match info.tag.as_str() {
        "input" => {
            !matches!(
                element_attr(info, "type")
                    .unwrap_or_else(|| default_type(info))
                    .to_ascii_lowercase()
                    .as_str(),
                "hidden" | "button" | "reset" | "submit" | "image"
            ) && !element_has_attr(info, "readonly")
        }
        "select" => true,
        "textarea" => !element_has_attr(info, "readonly"),
        _ => false,
    }
}

fn merge_validity(target: &mut Validity, source: &Validity) {
    target.value_missing |= source.value_missing;
    target.type_mismatch |= source.type_mismatch;
    target.pattern_mismatch |= source.pattern_mismatch;
    target.too_long |= source.too_long;
    target.too_short |= source.too_short;
    target.range_underflow |= source.range_underflow;
    target.range_overflow |= source.range_overflow;
    target.step_mismatch |= source.step_mismatch;
    target.bad_input |= source.bad_input;
    target.custom_error |= source.custom_error;
}

fn parse_single_string_arg_call(input: &str) -> Option<(String, &str)> {
    let (arg, rest) = parse_js_string_prefix(input)?;
    if rest.as_bytes().first().copied() != Some(b')') {
        return None;
    }
    Some((arg, &rest[1..]))
}

fn parse_one_or_two_string_arg_call(input: &str) -> Option<(String, Option<String>, &str)> {
    let (first, rest) = parse_js_string_prefix(input)?;
    let rest = rest.trim_start();
    if let Some(rest) = rest.strip_prefix(')') {
        return Some((first, None, rest));
    }
    let rest = rest.strip_prefix(',')?.trim_start();
    let (second, rest) = parse_js_string_prefix(rest)?;
    let rest = rest.trim_start().strip_prefix(')')?;
    Some((first, Some(second), rest))
}

fn parse_js_string_prefix(input: &str) -> Option<(String, &str)> {
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
    let arg = &input[1..end_quote];
    let rest = &input[end_quote + 1..];
    Some((arg.to_owned(), rest))
}

fn method_string_arg(member: &str, prefix: &str) -> Option<String> {
    let inner = member.strip_prefix(prefix)?.strip_suffix(')')?;
    js_string_literal(inner)
}

fn method_two_string_args(member: &str, prefix: &str) -> Option<(String, String)> {
    let inner = member.strip_prefix(prefix)?.strip_suffix(')')?;
    let (first, rest) = parse_js_string_prefix(inner.trim())?;
    let rest = rest.trim_start().strip_prefix(',')?.trim_start();
    let (second, rest) = parse_js_string_prefix(rest)?;
    rest.trim().is_empty().then_some((first, second))
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
            "<html><head><base href='https://example.com/app/page'><title>T</title></head><body><p class='x'>one</p><p>two</p></body></html>",
        )
        .unwrap();
        assert_eq!(
            page.evaluate_dom_expression("document.title"),
            Some(Ok("T".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.documentURI"),
            Some(Ok("file:///fixture.html".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.baseURI"),
            Some(Ok("https://example.com/app/page".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.hasFocus()"),
            Some(Ok("true".into()))
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
    fn page_evaluates_get_computed_style_subset() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<style>p { color: red; margin-left: 4px; } #copy { color: blue; font-size: 20px !important; --Token: A:B; }</style><p id='copy' style='font-size: 18px; margin-left: 10px'>Text</p>",
        )
        .unwrap();

        assert_eq!(
            page.evaluate_dom_expression("getComputedStyle(document.querySelector('#copy')).color"),
            Some(Ok("blue".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "getComputedStyle(document.querySelector('#copy')).fontSize"
            ),
            Some(Ok("20px".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "window.getComputedStyle(document.querySelector('#copy')).getPropertyValue('margin-left')"
            ),
            Some(Ok("10px".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "getComputedStyle(document.querySelector('#copy')).getPropertyValue('--Token')"
            ),
            Some(Ok("A:B".into()))
        );
    }

    #[test]
    fn page_evaluates_cssom_supports_and_geometry_subset() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<style>#box { width: 40px; height: 20px; }</style><main><div id='box'>Box</div></main>",
        )
        .unwrap();

        assert_eq!(
            page.evaluate_dom_expression("CSS.supports('display', 'grid')"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("CSS.supports('(unknown-prop: yes)')"),
            Some(Ok("false".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.styleSheets.length"),
            Some(Ok("1".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.styleSheets[0].cssRules.length"),
            Some(Ok("1".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.styleSheets[0].href === null"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.styleSheets[0].cssRules[0].selectorText"),
            Some(Ok("#box".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.styleSheets[0].cssRules[0].style.length"),
            Some(Ok("2".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "document.styleSheets[0].cssRules[0].style.getPropertyValue('width')"
            ),
            Some(Ok("40px".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.styleSheets[0].cssRules[0].style[1]"),
            Some(Ok("height".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "document.querySelector('#box').getBoundingClientRect().x"
            ),
            Some(Ok("8".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "document.querySelector('#box').getBoundingClientRect().width"
            ),
            Some(Ok("40".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "document.querySelector('#box').getBoundingClientRect().right"
            ),
            Some(Ok("48".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#box').getClientRects().length"),
            Some(Ok("1".into()))
        );
    }

    #[test]
    fn page_evaluates_geometry_interface_value_subset() {
        let page = Page::from_html("file:///fixture.html", "<main>geometry</main>").unwrap();

        assert_eq!(
            page.evaluate_dom_expression("new DOMPoint(1,2,3,4).z"),
            Some(Ok("3".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("DOMPoint.fromPoint({x:5,y:6}).w"),
            Some(Ok("1".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new DOMRect(10,20,-5,7).left"),
            Some(Ok("5".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("DOMRect.fromRect({x:1,y:2,width:3,height:4}).bottom"),
            Some(Ok("6".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("DOMQuad.fromRect({x:1,y:2,width:3,height:4}).p3.x"),
            Some(Ok("4".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "DOMQuad.fromRect({x:1,y:2,width:3,height:4}).getBounds().height"
            ),
            Some(Ok("4".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new DOMMatrix().is2D"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new DOMMatrix([1,0,0,1,5,6]).e"),
            Some(Ok("5".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new DOMMatrix().translate(10,20).transformPoint(new DOMPoint(1,2)).y"
            ),
            Some(Ok("22".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new DOMMatrix().scale(2,3).transformPoint(new DOMPoint(5,5)).x"
            ),
            Some(Ok("10".into()))
        );
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
            page.evaluate_dom_expression("document.activeElement.tagName"),
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
            page.evaluate_dom_expression("document.querySelector('#lead').nodeName"),
            Some(Ok("P".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#lead').localName"),
            Some(Ok("p".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#lead').nodeType"),
            Some(Ok("1".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#lead').isConnected"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "document.querySelector('#lead').ownerDocument === document"
            ),
            Some(Ok("true".into()))
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
            page.evaluate_dom_expression("document.querySelector('#bold').closest('main').id"),
            Some(Ok("root".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "document.querySelector('#lead').closest('p.note') !== null"
            ),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "document.querySelector('#bold').closest('.missing') === null"
            ),
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
    fn page_evaluates_form_data_subset() {
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

        assert_eq!(
            page.evaluate_dom_expression(
                "new FormData(document.querySelector('#contact')).get('name')"
            ),
            Some(Ok("Ada".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new FormData(document.querySelector('#contact')).get('body')"
            ),
            Some(Ok("Hello, world!".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new FormData(document.querySelector('#contact')).get('urgency')"
            ),
            Some(Ok("normal".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new FormData(document.querySelector('#contact')).getAll('format').length"
            ),
            Some(Ok("1".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new FormData(document.querySelector('#contact')).has('skip')"
            ),
            Some(Ok("false".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new FormData(document.querySelector('#contact')).get('missing') === null"
            ),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new FormData(document.querySelector('#contact')).entries().next().value[0]"
            ),
            Some(Ok("name".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new FormData(document.querySelector('#contact')).entries().next().value[1]"
            ),
            Some(Ok("Ada".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new FormData(document.querySelector('#contact')).keys().next().value"
            ),
            Some(Ok("name".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new FormData(document.querySelector('#contact')).values().next().value"
            ),
            Some(Ok("Ada".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new FormData(document.getElementById('upload')).get('attachment').type"
            ),
            Some(Ok("application/octet-stream".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new FormData(document.getElementById('upload')).get('attachment').size"
            ),
            Some(Ok("0".into()))
        );
    }

    #[test]
    fn page_evaluates_token_list_and_dataset_subset() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<html><head><link id='theme' rel='stylesheet alternate'></head>\
             <body><div id='dupes' class='a b a c b' data-user-id='42' data-api-base='/v1'>x</div></body></html>",
        )
        .unwrap();

        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#dupes').classList.length"),
            Some(Ok("3".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#dupes').classList.item(1)"),
            Some(Ok("b".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "document.querySelector('#dupes').classList.contains('a')"
            ),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "document.querySelector('#theme').relList.contains('alternate')"
            ),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#dupes').dataset.userId"),
            Some(Ok("42".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#dupes').dataset['apiBase']"),
            Some(Ok("/v1".into()))
        );
    }

    #[test]
    fn page_evaluates_form_validity_subset() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<form id='f'>\
                <input id='email' type='email' required value='bad'>\
                <input id='age' type='number' min='10' max='20' step='2' value='13'>\
                <select id='plan' required><option value=''>pick</option><option value='pro' selected>Pro</option></select>\
                <textarea id='notes' readonly>ok</textarea>\
             </form>",
        )
        .unwrap();

        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#email').willValidate"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#email').validity.typeMismatch"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#age').validity.stepMismatch"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#plan').checkValidity()"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#notes').willValidate"),
            Some(Ok("false".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#f').checkValidity()"),
            Some(Ok("false".into()))
        );
    }

    #[test]
    fn page_evaluates_history_range_selection_subset() {
        let page = Page::from_html(
            "file:///fixture.html#initial",
            "<main><p>history and selection smoke</p></main>",
        )
        .unwrap();

        assert_eq!(
            page.evaluate_dom_expression("history.length"),
            Some(Ok("1".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("window.history.length"),
            Some(Ok("1".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("history.state"),
            Some(Ok("null".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("history.scrollRestoration"),
            Some(Ok("auto".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.createRange().collapsed"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.createRange().startOffset"),
            Some(Ok("0".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("window.getSelection().rangeCount"),
            Some(Ok("0".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.getSelection().isCollapsed"),
            Some(Ok("true".into()))
        );
    }

    #[test]
    fn page_evaluates_traversal_mutation_and_clone_subset() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<main><div id='walk-root'><article id='art-1'><h2 id='heading'>Heading</h2><p id='para-1'>first</p><p id='para-2'>second</p></article><aside id='aside-1'><span id='aside-span'>aside</span></aside></div></main>",
        )
        .unwrap();

        assert_eq!(
            page.evaluate_dom_expression("structuredClone('hello')"),
            Some(Ok("hello".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("structuredClone([1,2,3]).length"),
            Some(Ok("3".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("structuredClone({greeting:'hello'}).greeting"),
            Some(Ok("hello".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("structuredClone(new Date(42)).getTime()"),
            Some(Ok("42".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "structuredClone(new Map([['answer', 42]])).get('answer')"
            ),
            Some(Ok("42".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "structuredClone(new Map([['answer', 42]])).entries().next().value[0]"
            ),
            Some(Ok("answer".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("structuredClone(new Set(['alpha','beta'])).has('beta')"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("structuredClone(new TypeError('boom')).name"),
            Some(Ok("TypeError".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("structuredClone(new TypeError('boom')).message"),
            Some(Ok("boom".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new MutationObserver(() => {}).takeRecords().length"),
            Some(Ok("0".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new MutationObserver(() => {}).disconnect()"),
            Some(Ok("undefined".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.createTreeWalker(document.getElementById('walk-root'), NodeFilter.SHOW_ELEMENT).root.id"),
            Some(Ok("walk-root".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.createTreeWalker(document.getElementById('walk-root'), NodeFilter.SHOW_ELEMENT).firstChild().id"),
            Some(Ok("art-1".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.createTreeWalker(document.getElementById('walk-root'), NodeFilter.SHOW_ELEMENT).lastChild().id"),
            Some(Ok("aside-1".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.createNodeIterator(document.getElementById('walk-root'), NodeFilter.SHOW_ELEMENT).nextNode().id"),
            Some(Ok("art-1".into()))
        );
    }

    #[test]
    fn page_evaluates_fetch_host_prep_subset() {
        let page = Page::from_html("file:///fixture.html", "<p>x</p>").unwrap();

        assert_eq!(
            page.evaluate_dom_expression("new Headers([['Content-Type',' text/plain '], ['X-Test','a'], ['X-Test','b']]).get('content-type')"),
            Some(Ok("text/plain".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new Headers([['Content-Type',' text/plain '], ['X-Test','a'], ['X-Test','b']]).get('x-test')"),
            Some(Ok("a, b".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new Headers([['Content-Type',' text/plain '], ['X-Test','a'], ['X-Test','b']]).entries().next().value[0]"),
            Some(Ok("content-type".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new Headers([['Content-Type',' text/plain '], ['X-Test','a'], ['X-Test','b']]).entries().next().value[1]"),
            Some(Ok("text/plain".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new Headers([['Content-Type',' text/plain '], ['X-Test','a']]).keys().next().value"
            ),
            Some(Ok("content-type".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new Headers([['Content-Type',' text/plain '], ['X-Test','a']]).has('x-test')"
            ),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new Blob(['Hi', 'é'], { type: 'TEXT/PLAIN' }).size"),
            Some(Ok("4".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new Blob(['Hi'], { type: 'TEXT/PLAIN' }).type"),
            Some(Ok("text/plain".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new File(['hello'], 'note.txt', { type: 'text/plain', lastModified: 42 }).name"
            ),
            Some(Ok("note.txt".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new File(['hello'], 'note.txt', { type: 'text/plain', lastModified: 42 }).size"
            ),
            Some(Ok("5".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new File(['hello'], 'note.txt', { type: 'text/plain', lastModified: 42 }).lastModified"
            ),
            Some(Ok("42".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new Response('Created', { status: 201, statusText: 'Created' }).status"
            ),
            Some(Ok("201".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new Response('Created', { status: 201, statusText: 'Created' }).ok"
            ),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new Response('Created', { status: 201, statusText: 'Created' }).headers.get('content-type')"
            ),
            Some(Ok("text/plain;charset=UTF-8".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new Response('Created', { status: 201, headers: [['X-Test','yes'], ['Set-Cookie','a=b']] }).headers.get('x-test')"
            ),
            Some(Ok("yes".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new Response('Created', { status: 201, headers: [['X-Test','yes'], ['Set-Cookie','a=b']] }).headers.has('set-cookie')"
            ),
            Some(Ok("false".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new Response(new Blob(['hello'], { type: 'text/plain' })).headers.get('content-type')"
            ),
            Some(Ok("text/plain".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new Response(null, { status: 204 }).body === null"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new Response('missing', { status: 404 }).ok"),
            Some(Ok("false".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("Response.json({ok:true}, { status: 201 }).status"),
            Some(Ok("201".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "Response.json({ok:true}, { status: 201 }).headers.get('content-type')"
            ),
            Some(Ok("application/json".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new Request('https://example.com/api').method"),
            Some(Ok("GET".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new Request('https://example.com/api').url"),
            Some(Ok("https://example.com/api".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new Request('https://example.com/api', { method: 'post', headers: [['Accept','application/json']], body: 'hello', credentials: 'include', cache: 'reload', redirect: 'manual', referrerPolicy: 'no-referrer', integrity: 'sha256-test', keepalive: true }).method"
            ),
            Some(Ok("POST".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new Request('https://example.com/api', { method: 'post', headers: [['Accept','application/json']], body: 'hello', credentials: 'include', cache: 'reload', redirect: 'manual', referrerPolicy: 'no-referrer', integrity: 'sha256-test', keepalive: true }).headers.get('content-type')"
            ),
            Some(Ok("text/plain;charset=UTF-8".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new Request('https://example.com/api', { method: 'post', headers: [['Accept','application/json']], body: 'hello', credentials: 'include', cache: 'reload', redirect: 'manual', referrerPolicy: 'no-referrer', integrity: 'sha256-test', keepalive: true }).headers.get('accept')"
            ),
            Some(Ok("application/json".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new Request('https://example.com/api', { headers: [['Host','evil.test'], ['Accept','text/html']] }).headers.has('host')"
            ),
            Some(Ok("false".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new Request('https://example.com/api', { headers: [['Host','evil.test'], ['Accept','text/html']] }).headers.get('accept')"
            ),
            Some(Ok("text/html".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new Request('https://example.com/api', { method: 'post', body: 'hello' }).body === null"
            ),
            Some(Ok("false".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new Request('https://example.com/api', { method: 'post', body: new URLSearchParams('a=1') }).headers.get('content-type')"
            ),
            Some(Ok("application/x-www-form-urlencoded;charset=UTF-8".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new Request('https://example.com/api', { method: 'post', credentials: 'include', cache: 'reload', redirect: 'manual', referrerPolicy: 'no-referrer', integrity: 'sha256-test', keepalive: true }).credentials"
            ),
            Some(Ok("include".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new Request('https://example.com/api', { method: 'post', credentials: 'include', cache: 'reload', redirect: 'manual', referrerPolicy: 'no-referrer', integrity: 'sha256-test', keepalive: true }).keepalive"
            ),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("Response.error().type"),
            Some(Ok("error".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("Response.error().status"),
            Some(Ok("0".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "Response.redirect('https://example.com/target', 302).headers.get('location')"
            ),
            Some(Ok("https://example.com/target".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("Response.redirect('https://example.com/target', 302).ok"),
            Some(Ok("false".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new AbortController().signal.aborted"),
            Some(Ok("false".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("AbortSignal.timeout(0).aborted"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("AbortSignal.any([AbortSignal.timeout(0)]).aborted"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new URLPattern({ pathname: '/posts/:id' }).test({ pathname: '/posts/42' })"
            ),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new URLPattern({ pathname: '/posts/:id' }).exec({ pathname: '/posts/42' }).pathname.groups.id"),
            Some(Ok("42".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new URLPattern({ pathname: '/assets/*' }).exec({ pathname: '/assets/img/logo.png' }).pathname.groups['*']"),
            Some(Ok("img/logo.png".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("URL.canParse('/other', 'https://example.com/app/page')"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("URL.canParse('://bad')"),
            Some(Ok("false".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("URL.canParse('data:text/plain,Hello')"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new URL('data:text/plain,Hello').protocol"),
            Some(Ok("data:".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new URL('data:text/plain,Hello').origin"),
            Some(Ok("null".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new URL('data:text/plain,Hello').pathname"),
            Some(Ok("text/plain,Hello".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new URL('https://example.com/path?q=1&tag=web&tag=engine').toString()"
            ),
            Some(Ok("https://example.com/path?q=1&tag=web&tag=engine".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new URL('https://example.com/path?q=1&tag=web&tag=engine').searchParams.size"
            ),
            Some(Ok("3".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new URL('https://example.com/path?q=1&tag=web&tag=engine').searchParams.has('tag')"
            ),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new URL('https://example.com/path?q=1&tag=web&tag=engine').searchParams.getAll('tag')[1]"),
            Some(Ok("engine".into()))
        );
    }

    #[test]
    fn page_evaluates_performance_and_media_query_subset() {
        let page = Page::from_html("file:///fixture.html", "<p>x</p>").unwrap();

        assert_eq!(
            page.evaluate_dom_expression("typeof performance.now()"),
            Some(Ok("number".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("performance.now() >= 0"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "performance.timeOrigin + performance.now() >= performance.timeOrigin"
            ),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("matchMedia('(min-width: 800px)').matches"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("matchMedia('(max-width: 799px)').matches"),
            Some(Ok("false".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("window.matchMedia('print').matches"),
            Some(Ok("false".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("matchMedia('(min-width: 800px)').media"),
            Some(Ok("(min-width: 800px)".into()))
        );
    }

    #[test]
    fn page_evaluates_document_navigator_storage_and_event_subset() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<html><head><meta id='charset' charset='utf-8'><meta id='referrer' name='referrer' content='strict-origin'><title>T</title></head>\
             <body><iframe id='frame' sandbox='allow-scripts allow-forms'></iframe><button id='btn'>go</button></body></html>",
        )
        .unwrap();

        assert_eq!(
            page.evaluate_dom_expression("document.readyState"),
            Some(Ok("complete".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.compatMode"),
            Some(Ok("CSS1Compat".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("window.innerWidth"),
            Some(Ok("800".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("window.innerHeight"),
            Some(Ok("600".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("devicePixelRatio"),
            Some(Ok("1".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("screen.width"),
            Some(Ok("800".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("visualViewport.scale"),
            Some(Ok("1".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.defaultView === window"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.scrollingElement.tagName"),
            Some(Ok("HTML".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.characterSet"),
            Some(Ok("UTF-8".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#referrer').content"),
            Some(Ok("strict-origin".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#charset').charset"),
            Some(Ok("utf-8".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "document.querySelector('#frame').sandbox.contains('allow-forms')"
            ),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("navigator.userAgent.includes('Vixen')"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("navigator.languages[0]"),
            Some(Ok("en-US".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("localStorage.length"),
            Some(Ok("0".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("sessionStorage.getItem('missing')"),
            Some(Ok("null".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new Event('ready', {bubbles:true}).bubbles"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new CustomEvent('ready', {detail:'ok'}).detail"),
            Some(Ok("ok".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "document.querySelector('#btn').dispatchEvent(new Event('click'))"
            ),
            Some(Ok("true".into()))
        );
    }

    #[test]
    fn page_evaluates_network_host_prep_subset() {
        let page = Page::from_html("file:///fixture.html", "<p>x</p>").unwrap();

        assert_eq!(
            page.evaluate_dom_expression(
                "new URL('https://example.com:8443/path?q=1#frag').origin"
            ),
            Some(Ok("https://example.com:8443".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new URL('/other', 'https://example.com/app/page').href"),
            Some(Ok("https://example.com/other".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new URL('https://example.com/path?q=1#frag').searchParams.get('q')"
            ),
            Some(Ok("1".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new URLSearchParams('?q=rust+lang&tag=web&tag=engine').get('q')"
            ),
            Some(Ok("rust lang".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new URLSearchParams('tag=web&tag=engine').getAll('tag').length"
            ),
            Some(Ok("2".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new URLSearchParams('a=1&b=2').has('b')"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new URLSearchParams('space=a b').toString()"),
            Some(Ok("space=a+b".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new URLSearchParams([['q','rust lang'], ['tag','web'], ['tag','engine']]).toString()"
            ),
            Some(Ok("q=rust+lang&tag=web&tag=engine".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new URLSearchParams([['q','rust lang'], ['tag','web']]).entries().next().value[0]"
            ),
            Some(Ok("q".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new URLSearchParams([['q','rust lang'], ['tag','web']]).entries().next().value[1]"
            ),
            Some(Ok("rust lang".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new URLSearchParams([['q','rust lang'], ['tag','web']]).keys().next().value"
            ),
            Some(Ok("q".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new URLSearchParams([['q','rust lang'], ['tag','web']]).values().next().value"
            ),
            Some(Ok("rust lang".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new URLSearchParams('tag=web').has('tag', 'web')"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new TextEncoder().encoding"),
            Some(Ok("utf-8".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new TextEncoder().encode('é').length"),
            Some(Ok("2".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new TextEncoder().encode('A')[0]"),
            Some(Ok("65".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new TextEncoder().encodeInto('aé', new Uint8Array(3)).read"
            ),
            Some(Ok("2".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new TextEncoder().encodeInto('aé', new Uint8Array(3)).written"
            ),
            Some(Ok("3".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new TextDecoder().decode([65,13,10,66])"),
            Some(Ok("A\nB".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new TextDecoder('utf-8').encoding"),
            Some(Ok("utf-8".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new TextDecoder('UTF-8', { fatal: true }).fatal"),
            Some(Ok("true".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("new TextDecoder('utf-8', { ignoreBOM: true }).ignoreBOM"),
            Some(Ok("true".into()))
        );
        assert!(matches!(
            page.evaluate_dom_expression("new TextDecoder('utf-8', { fatal: true }).decode([255])"),
            Some(Err(_))
        ));
        assert_eq!(
            page.evaluate_dom_expression("btoa('Vixen')"),
            Some(Ok("Vml4ZW4=".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("atob('Vml4ZW4=')"),
            Some(Ok("Vixen".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression(
                "new DOMParser().parseFromString(\"<main><p id='parsed'>Parsed</p></main>\", 'text/html').querySelector('#parsed').textContent"
            ),
            Some(Ok("Parsed".into()))
        );
    }

    #[test]
    fn page_evaluates_html_serialization_subset() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<main id='root'><h1 id='title'>DOM <span>Basic</span></h1><p id='outro'>Closing text.</p></main>",
        )
        .unwrap();

        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#title').innerHTML"),
            Some(Ok("DOM <span>Basic</span>".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#outro').outerHTML"),
            Some(Ok("<p id=\"outro\">Closing text.</p>".into()))
        );
    }

    #[test]
    fn page_evaluates_responsive_image_current_src_subset() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<img id='widths' src='small.jpg' srcset='small.jpg 480w, medium.jpg 800w, large.jpg 1200w' sizes='100vw'>\
             <img id='density' srcset='one.png 1x, two.png 2x'>\
             <img id='fallback' src='fallback.jpg'>",
        )
        .unwrap();

        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#widths').currentSrc"),
            Some(Ok("medium.jpg".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#density').currentSrc"),
            Some(Ok("one.png".into()))
        );
        assert_eq!(
            page.evaluate_dom_expression("document.querySelector('#fallback').currentSrc"),
            Some(Ok("fallback.jpg".into()))
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
