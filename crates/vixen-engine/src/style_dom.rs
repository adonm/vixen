//! Stylo DOM adapter — implements `selectors::Element` (via Stylo's servo
//! `SelectorImpl`) over the html5ever `RcDom`. Phase 3 step 2 (docs/PLAN.md):
//! unblocks every selector-based surface (`vixen-headless --extract-selector`,
//! the WPT `selector-*` checks) ahead of the full Stylo cascade.
//!
//! ## Why a precomputed arena
//!
//! `markup5ever_rcdom::Node` stores its parent as a `Cell<Option<Weak<Node>>>`,
//! so walking up to the parent needs `Weak::upgrade` plus an `Rc`→`&Node`
//! promotion that's only sound via `unsafe`. To keep this module under the
//! crate-wide `forbid(unsafe_code)` policy, [`ElementArena`] pre-computes
//! every node's parent / sibling / child indices with a top-down DFS walk.
//! [`LayoutDom`] is then a `(arena ref, index)` pair — pure, `Copy`, safe.
//!
//! Atoms: Stylo's servo `SelectorImpl` uses `GenericAtomIdent<web_atoms::…>` for
//! local names while the html5ever `RcDom` uses `markup5ever::LocalName` (a
//! *different* `string_cache` static set). Comparison goes through `&str`;
//! both atom families deref to `str` and the per-match string compare is
//! dwarfed by the selector matcher's own work.
//!
//! Crate-name note: the published package is `stylo`, but its `[lib] name` is
//! `style`, so source uses `style::…` even though `Cargo.toml` says `stylo`.

#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ptr;
use std::rc::Rc;

use cssparser::{CowRcStr, Parser as CssParser, ParserInput, SourceLocation};
use markup5ever_rcdom::{Node, NodeData};
use selectors::attr::{
    AttrSelectorOperation, AttrSelectorOperator, CaseSensitivity, NamespaceConstraint,
};
use selectors::matching::{ElementSelectorFlags, MatchingContext};
use selectors::parser::{ParseRelative, SelectorList};
use selectors::{Element, OpaqueElement};
// Stylo publishes its crate under the lib name `style`.
use style::context::QuirksMode;
use style::dom_apis;
use style::selector_parser::{NonTSPseudoClass, SelectorImpl};
use style::stylesheets::{Namespaces, Origin, UrlExtraData};
use style_traits::{ParseError, StyleParseErrorKind};

use crate::doc::Document;

// ---------------------------------------------------------------------------
// ElementArena — pre-computed tree topology, indexed by usize.
// ---------------------------------------------------------------------------

/// A precomputed view of an RcDom's *element* nodes with their parent /
/// sibling / child topology, built once per query. Stores owned `Rc<Node>`
/// handles so the arena can outlive the function that builds it; all node
/// access returns `&Node` tied to the arena's borrow.
pub(crate) struct ElementArena {
    nodes: Vec<Rc<Node>>,
    parents: Vec<Option<usize>>,
    prev_sibling_element: Vec<Option<usize>>,
    next_sibling_element: Vec<Option<usize>>,
    first_element_child: Vec<Option<usize>>,
}

impl ElementArena {
    /// Build an arena from a parsed [`Document`]. The document's first
    /// element (typically `<html>`) is index 0.
    pub(crate) fn build(root_node: &Node) -> Self {
        let mut arena = Self {
            nodes: Vec::new(),
            parents: Vec::new(),
            prev_sibling_element: Vec::new(),
            next_sibling_element: Vec::new(),
            first_element_child: Vec::new(),
        };
        arena.collect_children(root_node, None);
        arena.fill_next_siblings();
        arena
    }

    /// DFS walk: visit every element descendant of `node` in document order,
    /// recording parent / prev-sibling / first-child topology as we go.
    fn collect_children(&mut self, node: &Node, parent_idx: Option<usize>) {
        // Snapshot the children out of the RefCell so the borrow closes
        // before we recurse (otherwise nested borrows trip RefCell).
        let children: Vec<Rc<Node>> = node.children.borrow().clone();
        let mut prev_element_idx: Option<usize> = None;
        for child in children.iter() {
            // Recurse into non-element nodes too — their element descendants
            // still belong in our flat document-order view.
            if !matches!(child.data, NodeData::Element { .. }) {
                self.collect_children(child, parent_idx);
                continue;
            }
            let idx = self.nodes.len();
            self.nodes.push(Rc::clone(child));
            self.parents.push(parent_idx);
            self.prev_sibling_element.push(prev_element_idx);
            self.next_sibling_element.push(None);
            // Push the current node's own first-child slot so all topology
            // vectors stay parallel before we read [p] below.
            self.first_element_child.push(None);
            if let Some(p) = parent_idx
                && self.first_element_child[p].is_none()
            {
                self.first_element_child[p] = Some(idx);
            }
            prev_element_idx = Some(idx);
            self.collect_children(child, Some(idx));
        }
    }

    /// Populate `next_sibling_element` from the prev-sibling chain. Couldn't
    /// be done in `collect_children` because the next sibling isn't known
    /// until we visit it.
    fn fill_next_siblings(&mut self) {
        for idx in 0..self.nodes.len() {
            if let Some(prev) = self.prev_sibling_element[idx] {
                self.next_sibling_element[prev] = Some(idx);
            }
        }
    }

    fn parent(&self, idx: usize) -> Option<usize> {
        self.parents[idx]
    }
    fn prev_element(&self, idx: usize) -> Option<usize> {
        self.prev_sibling_element[idx]
    }
    fn next_element(&self, idx: usize) -> Option<usize> {
        self.next_sibling_element[idx]
    }
    fn first_element_child_of(&self, idx: usize) -> Option<usize> {
        self.first_element_child[idx]
    }
    fn node(&self, idx: usize) -> &Node {
        &self.nodes[idx]
    }
    fn len(&self) -> usize {
        self.nodes.len()
    }
}

// ---------------------------------------------------------------------------
// LayoutDom — the Copy wrapper that implements selectors::Element.
// ---------------------------------------------------------------------------

/// A borrowed, indexed view of an element inside an [`ElementArena`].
#[derive(Copy, Clone)]
pub struct LayoutDom<'a> {
    arena: &'a ElementArena,
    idx: usize,
}

impl<'a> LayoutDom<'a> {
    pub(crate) fn new(arena: &'a ElementArena, idx: usize) -> Self {
        Self { arena, idx }
    }

    fn node(&self) -> &Node {
        self.arena.node(self.idx)
    }

    fn parent_idx(&self) -> Option<usize> {
        self.arena.parent(self.idx)
    }

    fn local_name_str(&self) -> &str {
        match &self.node().data {
            NodeData::Element { name, .. } => name.local.as_ref(),
            _ => "",
        }
    }

    fn namespace_str(&self) -> &str {
        match &self.node().data {
            NodeData::Element { name, .. } => name.ns.as_ref(),
            _ => "",
        }
    }

    /// First attribute named `name` in the no-namespace slot.
    fn attr_value(&self, name: &str) -> Option<String> {
        let NodeData::Element { attrs, .. } = &self.node().data else {
            return None;
        };
        let attrs = attrs.borrow();
        attrs
            .iter()
            .find(|a| a.name.local.as_ref() == name && a.name.ns.as_ref() == "")
            .map(|a| a.value.to_string())
    }

    fn has_attr(&self, name: &str) -> bool {
        let NodeData::Element { attrs, .. } = &self.node().data else {
            return false;
        };
        let attrs = attrs.borrow();
        attrs.iter().any(|a| a.name.local.as_ref() == name)
    }

    fn is_form_control(&self) -> bool {
        matches!(
            self.local_name_str(),
            "input" | "select" | "textarea" | "button"
        )
    }

    fn is_link(&self) -> bool {
        matches!(self.local_name_str(), "a" | "area") && self.has_attr("href")
    }

    /// Iterate this element's own attributes (no inheritance).
    fn each_attr<F: FnMut(&str, &str)>(&self, mut f: F) {
        let NodeData::Element { attrs, .. } = &self.node().data else {
            return;
        };
        let attrs = attrs.borrow();
        for a in attrs.iter() {
            f(a.name.local.as_ref(), a.value.as_ref());
        }
    }
}

impl<'a> PartialEq for LayoutDom<'a> {
    fn eq(&self, other: &Self) -> bool {
        ptr::eq(self.node(), other.node())
    }
}
impl<'a> Eq for LayoutDom<'a> {}
impl<'a> Hash for LayoutDom<'a> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Rc::as_ptr(&self.arena.nodes[self.idx]).hash(state)
    }
}

impl<'a> fmt::Debug for LayoutDom<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tag = self.local_name_str();
        if tag.is_empty() {
            write!(f, "LayoutDom(#{})", self.idx)
        } else {
            write!(f, "LayoutDom(<{}>)", tag)
        }
    }
}

// ---------------------------------------------------------------------------
// selectors::Element impl
// ---------------------------------------------------------------------------

impl<'a> Element for LayoutDom<'a> {
    type Impl = SelectorImpl;

    fn opaque(&self) -> OpaqueElement {
        OpaqueElement::new(self.node())
    }

    fn parent_element(&self) -> Option<Self> {
        self.parent_idx().map(|p| LayoutDom::new(self.arena, p))
    }

    fn parent_node_is_shadow_root(&self) -> bool {
        false
    }
    fn containing_shadow_host(&self) -> Option<Self> {
        None
    }
    fn is_pseudo_element(&self) -> bool {
        false
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        self.arena
            .prev_element(self.idx)
            .map(|i| Self::new(self.arena, i))
    }
    fn next_sibling_element(&self) -> Option<Self> {
        self.arena
            .next_element(self.idx)
            .map(|i| Self::new(self.arena, i))
    }
    fn first_element_child(&self) -> Option<Self> {
        self.arena
            .first_element_child_of(self.idx)
            .map(|i| Self::new(self.arena, i))
    }

    fn is_html_element_in_html_document(&self) -> bool {
        self.namespace_str() == "http://www.w3.org/1999/xhtml"
    }

    fn has_local_name(
        &self,
        local_name: &<Self::Impl as selectors::SelectorImpl>::BorrowedLocalName,
    ) -> bool {
        self.local_name_str() == local_name.as_ref()
    }

    fn has_namespace(
        &self,
        ns: &<Self::Impl as selectors::SelectorImpl>::BorrowedNamespaceUrl,
    ) -> bool {
        self.namespace_str() == ns.as_ref()
    }

    fn is_same_type(&self, other: &Self) -> bool {
        self.local_name_str() == other.local_name_str()
            && self.namespace_str() == other.namespace_str()
    }

    fn attr_matches(
        &self,
        ns: &NamespaceConstraint<&<Self::Impl as selectors::SelectorImpl>::NamespaceUrl>,
        local_name: &<Self::Impl as selectors::SelectorImpl>::LocalName,
        operation: &AttrSelectorOperation<&<Self::Impl as selectors::SelectorImpl>::AttrValue>,
    ) -> bool {
        let want_local: &str = local_name.0.as_ref();
        let want_ns: Option<&str> = match ns {
            NamespaceConstraint::Specific(n) => Some(n.0.as_ref()),
            NamespaceConstraint::Any => None,
        };
        let mut matched = false;
        self.each_attr(|name, value| {
            if matched || name != want_local {
                return;
            }
            if let Some(want_ns) = want_ns
                // html5ever stores HTML attrs with empty ns; treat "" as
                // the HTML namespace for selector comparison.
                && !want_ns.is_empty()
                && want_ns != "http://www.w3.org/1999/xhtml"
            {
                return;
            }
            matched = match operation {
                AttrSelectorOperation::Exists => true,
                AttrSelectorOperation::WithValue {
                    operator,
                    case_sensitivity,
                    value: expected,
                } => attr_op_matches(value, *operator, *case_sensitivity, expected.0.as_ref()),
            };
        });
        matched
    }

    fn match_non_ts_pseudo_class(
        &self,
        pc: &NonTSPseudoClass,
        _context: &mut MatchingContext<Self::Impl>,
    ) -> bool {
        use NonTSPseudoClass::*;
        match pc {
            // User-action pseudos need live DOM state we don't track.
            Active | Hover | Focus | FocusVisible | FocusWithin | Visited | Fullscreen | Modal
            | Target | PopoverOpen | Open => false,

            Disabled => self.has_attr("disabled"),
            Enabled => self.is_form_control() && !self.has_attr("disabled"),
            Checked => {
                self.local_name_str() == "input"
                    && matches!(
                        self.attr_value("type").as_deref(),
                        Some("checkbox") | Some("radio")
                    )
                    && self.has_attr("checked")
            }
            Required => self.is_form_control() && self.has_attr("required"),
            Optional => self.is_form_control() && !self.has_attr("required"),
            ReadOnly => {
                self.is_form_control() && self.has_attr("readonly")
                    || self.attr_value("contenteditable").as_deref() == Some("false")
            }
            ReadWrite => {
                self.is_form_control() && !self.has_attr("readonly")
                    || self.attr_value("contenteditable").as_deref() == Some("true")
            }
            Link | AnyLink => self.is_link(),
            PlaceholderShown => self.local_name_str() == "input" && self.has_attr("placeholder"),

            // Validation pseudos need the forms module (Phase 6); fail closed.
            Valid | Invalid | UserValid | UserInvalid | InRange | OutOfRange | Indeterminate
            | Default => false,

            Defined => true,
            Autofill => self.has_attr("autofill"),

            MozMeterOptimum | MozMeterSubOptimum | MozMeterSubSubOptimum | ServoNonZeroBorder => {
                false
            }
            Lang(_) | CustomState(_) => false,
        }
    }

    fn match_pseudo_element(
        &self,
        _pe: &<Self::Impl as selectors::SelectorImpl>::PseudoElement,
        _context: &mut MatchingContext<Self::Impl>,
    ) -> bool {
        false
    }

    fn apply_selector_flags(&self, _flags: ElementSelectorFlags) {}

    fn is_link(&self) -> bool {
        self.is_link()
    }

    fn is_html_slot_element(&self) -> bool {
        self.local_name_str() == "slot"
    }

    fn has_id(
        &self,
        id: &<Self::Impl as selectors::SelectorImpl>::Identifier,
        case_sensitivity: CaseSensitivity,
    ) -> bool {
        match self.attr_value("id") {
            Some(actual) => case_sensitivity.eq(actual.as_bytes(), id.0.as_ref().as_bytes()),
            None => false,
        }
    }

    fn has_class(
        &self,
        name: &<Self::Impl as selectors::SelectorImpl>::Identifier,
        case_sensitivity: CaseSensitivity,
    ) -> bool {
        let needle: &str = name.0.as_ref();
        match self.attr_value("class") {
            Some(actual) => actual
                .split_ascii_whitespace()
                .any(|c| case_sensitivity.eq(c.as_bytes(), needle.as_bytes())),
            None => false,
        }
    }

    fn has_custom_state(
        &self,
        _name: &<Self::Impl as selectors::SelectorImpl>::Identifier,
    ) -> bool {
        false
    }

    fn imported_part(
        &self,
        _name: &<Self::Impl as selectors::SelectorImpl>::Identifier,
    ) -> Option<<Self::Impl as selectors::SelectorImpl>::Identifier> {
        None
    }

    fn is_part(&self, name: &<Self::Impl as selectors::SelectorImpl>::Identifier) -> bool {
        let needle: &str = name.0.as_ref();
        match self.attr_value("part") {
            Some(actual) => actual.split_ascii_whitespace().any(|p| p == needle),
            None => false,
        }
    }

    fn is_empty(&self) -> bool {
        for child in self.node().children.borrow().iter() {
            match &child.data {
                NodeData::Element { .. } => return false,
                NodeData::Text { contents } if !contents.borrow().is_empty() => {
                    return false;
                }
                _ => {}
            }
        }
        true
    }

    fn is_root(&self) -> bool {
        self.parent_idx().is_none()
    }

    fn add_element_unique_hashes(&self, _filter: &mut selectors::bloom::BloomFilter) -> bool {
        // We never construct a MatchingContext with a bloom filter; this
        // method is required by the trait but unreachable here. Conservative
        // answer per the trait contract.
        false
    }
}

/// Apply a CSS attribute selector operator. `actual` is the element's
/// attribute value; `expected` is the value from the selector.
/// Implements the cases at <https://drafts.csswg.org/selectors/#attribute-selectors>.
fn attr_op_matches(
    actual: &str,
    operator: AttrSelectorOperator,
    case_sensitivity: CaseSensitivity,
    expected: &str,
) -> bool {
    let eq = |a: &str, b: &str| case_sensitivity.eq(a.as_bytes(), b.as_bytes());
    match operator {
        AttrSelectorOperator::Equal => eq(actual, expected),
        AttrSelectorOperator::Includes => {
            actual.split_ascii_whitespace().any(|tok| eq(tok, expected))
        }
        AttrSelectorOperator::DashMatch => {
            eq(actual, expected)
                || (actual.starts_with(expected)
                    && actual.as_bytes().get(expected.len()) == Some(&b'-'))
        }
        AttrSelectorOperator::Prefix => !expected.is_empty() && actual.starts_with(expected),
        AttrSelectorOperator::Suffix => !expected.is_empty() && actual.ends_with(expected),
        AttrSelectorOperator::Substring => !expected.is_empty() && actual.contains(expected),
    }
}

// ---------------------------------------------------------------------------
// Public API: parse a selector list with Stylo + walk the tree.
// ---------------------------------------------------------------------------

/// A parsed CSS selector list, parsed by Stylo's selector parser.
#[derive(Debug, Clone)]
pub struct Selector {
    list: SelectorList<SelectorImpl>,
}

/// Wrapper around Stylo's Servo selector parser that enables `:has()` for DOM
/// query surfaces. All pseudo-class/pseudo-element parsing delegates back to
/// Stylo; the only policy difference is `parse_has() -> true`.
struct VixenSelectorParser<'a> {
    inner: style::selector_parser::SelectorParser<'a>,
}

impl<'a> VixenSelectorParser<'a> {
    fn author_no_namespace(url_data: &'a UrlExtraData, namespaces: &'a Namespaces) -> Self {
        Self {
            inner: style::selector_parser::SelectorParser {
                stylesheet_origin: Origin::Author,
                namespaces,
                url_data,
                for_supports_rule: false,
            },
        }
    }
}

impl<'a, 'i> selectors::Parser<'i> for VixenSelectorParser<'a> {
    type Impl = SelectorImpl;
    type Error = StyleParseErrorKind<'i>;

    fn parse_nth_child_of(&self) -> bool {
        selectors::Parser::parse_nth_child_of(&self.inner)
    }

    fn parse_is_and_where(&self) -> bool {
        selectors::Parser::parse_is_and_where(&self.inner)
    }

    fn parse_has(&self) -> bool {
        true
    }

    fn parse_parent_selector(&self) -> bool {
        selectors::Parser::parse_parent_selector(&self.inner)
    }

    fn parse_part(&self) -> bool {
        selectors::Parser::parse_part(&self.inner)
    }

    fn allow_forgiving_selectors(&self) -> bool {
        selectors::Parser::allow_forgiving_selectors(&self.inner)
    }

    fn parse_non_ts_pseudo_class(
        &self,
        location: SourceLocation,
        name: CowRcStr<'i>,
    ) -> Result<NonTSPseudoClass, ParseError<'i>> {
        selectors::Parser::parse_non_ts_pseudo_class(&self.inner, location, name)
    }

    fn parse_non_ts_functional_pseudo_class<'t>(
        &self,
        name: CowRcStr<'i>,
        parser: &mut CssParser<'i, 't>,
        after_part: bool,
    ) -> Result<NonTSPseudoClass, ParseError<'i>> {
        selectors::Parser::parse_non_ts_functional_pseudo_class(
            &self.inner,
            name,
            parser,
            after_part,
        )
    }

    fn parse_pseudo_element(
        &self,
        location: SourceLocation,
        name: CowRcStr<'i>,
    ) -> Result<<SelectorImpl as selectors::SelectorImpl>::PseudoElement, ParseError<'i>> {
        selectors::Parser::parse_pseudo_element(&self.inner, location, name)
    }

    fn default_namespace(&self) -> Option<<SelectorImpl as selectors::SelectorImpl>::NamespaceUrl> {
        selectors::Parser::default_namespace(&self.inner)
    }

    fn namespace_for_prefix(
        &self,
        prefix: &<SelectorImpl as selectors::SelectorImpl>::NamespacePrefix,
    ) -> Option<<SelectorImpl as selectors::SelectorImpl>::NamespaceUrl> {
        selectors::Parser::namespace_for_prefix(&self.inner, prefix)
    }

    fn parse_host(&self) -> bool {
        selectors::Parser::parse_host(&self.inner)
    }

    fn parse_slotted(&self) -> bool {
        selectors::Parser::parse_slotted(&self.inner)
    }
}

impl Selector {
    /// Parse a comma-separated selector list (`a, div#x, .y > z`). Errors
    /// for malformed input (the CLI surfaces `invalid-selector`, SPEC.md).
    pub fn parse(input: &str) -> Result<Self, SelectorError> {
        let url_data =
            UrlExtraData::from(url::Url::parse("about:blank").expect("about:blank parses"));
        let namespaces = Namespaces::default();
        let parser = VixenSelectorParser::author_no_namespace(&url_data, &namespaces);
        let mut input_parser = ParserInput::new(input);
        match SelectorList::parse(
            &parser,
            &mut CssParser::new(&mut input_parser),
            ParseRelative::No,
        ) {
            Ok(list) => Ok(Selector { list }),
            Err(_) => Err(SelectorError::Parse(input.to_owned())),
        }
    }

    pub(crate) fn as_stylo_list(&self) -> &SelectorList<SelectorImpl> {
        &self.list
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SelectorError {
    #[error("invalid selector: {0:?}")]
    Parse(String),
}

/// A matched element projected into the shape the WPT harness / CLI expects.
/// `node_id` is the 1-based document-order index among elements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedElement {
    pub node_id: usize,
    pub tag: String,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub attributes: Vec<(String, String)>,
    pub text: String,
}

/// DOM data needed by Page's accessibility projection. This deliberately
/// remains engine-internal so semantic decisions stay in `Page`.
pub(crate) struct AccessibilityElement {
    pub node_id: usize,
    pub tag: String,
    pub role: Option<String>,
    pub aria_labelledby: Option<String>,
    pub aria_label: Option<String>,
    pub title: Option<String>,
    pub alt: Option<String>,
    pub value: Option<String>,
    pub input_type: Option<String>,
    pub aria_disabled: Option<String>,
    pub aria_checked: Option<String>,
    pub aria_selected: Option<String>,
    pub aria_expanded: Option<String>,
    pub tabindex: Option<String>,
    pub text: String,
    pub label: String,
    pub href: bool,
    pub disabled: bool,
    pub checked: bool,
    pub selected: bool,
    pub multiple: bool,
    pub contenteditable: bool,
}

/// Element-tree relation for read-only DOM host projections.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ElementRelation {
    Parent,
    FirstChild,
    LastChild,
    PreviousSibling,
    NextSibling,
}

impl MatchedElement {
    fn from_node(node: &Node, node_id: usize) -> Self {
        let NodeData::Element { name, attrs, .. } = &node.data else {
            return Self {
                node_id,
                tag: String::new(),
                id: None,
                classes: vec![],
                attributes: vec![],
                text: String::new(),
            };
        };
        let attrs = attrs.borrow();
        let mut id = None;
        let mut classes = Vec::new();
        let mut pairs = Vec::with_capacity(attrs.len());
        for a in attrs.iter() {
            let key = a.name.local.as_ref().to_owned();
            let val = a.value.to_string();
            if key == "id" {
                id = Some(val.clone());
            } else if key == "class" {
                classes.extend(val.split_ascii_whitespace().map(str::to_owned));
            }
            pairs.push((key, val));
        }
        Self {
            node_id,
            tag: name.local.as_ref().to_owned(),
            id,
            classes,
            attributes: pairs,
            text: direct_text(node),
        }
    }

    /// Project into the engine-API [`ElementInfo`] DTO that the WPT harness
    /// and the shell's inspector surface expect.
    pub fn into_element_info(self) -> vixen_api::ElementInfo {
        vixen_api::ElementInfo {
            node_id: self.node_id,
            tag: self.tag,
            id: self.id,
            classes: self.classes,
            attributes: self.attributes,
            text: self.text,
            bbox: None,
        }
    }
}

fn direct_text(node: &Node) -> String {
    let mut s = String::new();
    for child in node.children.borrow().iter() {
        if let NodeData::Text { contents } = &child.data {
            s.push_str(&contents.borrow());
        }
    }
    s.trim().to_owned()
}

fn descendant_text(node: &Node) -> String {
    let mut out = String::new();
    collect_text(node, &mut out);
    out.trim().to_owned()
}

fn collect_text(node: &Node, out: &mut String) {
    if let NodeData::Text { contents } = &node.data {
        out.push_str(&contents.borrow());
    }
    let children: Vec<Rc<Node>> = node.children.borrow().clone();
    for child in children {
        collect_text(&child, out);
    }
}

// ---------------------------------------------------------------------------
// Document query methods
// ---------------------------------------------------------------------------

impl Document {
    /// Return at most `maximum` visible, potentially semantic elements in
    /// document order. Relevant attributes and descendant text are copied with
    /// a per-string byte bound before Page assigns roles and states.
    pub(crate) fn accessibility_elements<F>(
        &self,
        maximum: usize,
        max_string_bytes: usize,
        mut is_visible: F,
    ) -> (Vec<AccessibilityElement>, bool)
    where
        F: FnMut(usize) -> bool,
    {
        let arena = ElementArena::build(&self.dom.document);
        const MAX_DOM_SCAN: usize = 65_536;
        const MAX_NAME_INDEX_ENTRIES: usize = 4096;
        const MAX_NAME_WORK: usize = 65_536;
        let scan_len = arena.len().min(MAX_DOM_SCAN);
        let mut hidden = vec![false; scan_len];
        let mut rendered = vec![false; scan_len];
        let mut inherited_disabled = vec![false; scan_len];
        let mut disables_descendants = vec![false; scan_len];
        let mut ids = HashMap::new();
        let mut labels_for: HashMap<String, Vec<usize>> = HashMap::new();
        let mut label_count = 0;
        let mut truncated = arena.len() > scan_len;

        for idx in 0..scan_len {
            let node = arena.node(idx);
            let NodeData::Element { name, attrs, .. } = &node.data else {
                continue;
            };
            let attrs = attrs.borrow();
            let own_hidden = attrs.iter().any(|attr| {
                let name = attr.name.local.as_ref();
                name == "hidden"
                    || (name == "aria-hidden" && attr.value.trim().eq_ignore_ascii_case("true"))
            }) || (name.local.as_ref() == "input"
                && attrs.iter().any(|attr| {
                    attr.name.local.as_ref() == "type"
                        && attr.value.trim().eq_ignore_ascii_case("hidden")
                }));
            hidden[idx] = own_hidden || arena.parent(idx).is_some_and(|parent| hidden[parent]);
            rendered[idx] = !hidden[idx] && is_visible(idx + 1);
            inherited_disabled[idx] = arena
                .parent(idx)
                .is_some_and(|parent| inherited_disabled[parent] || disables_descendants[parent]);
            disables_descendants[idx] = matches!(name.local.as_ref(), "fieldset" | "optgroup")
                && attrs
                    .iter()
                    .any(|attr| attr.name.local.as_ref() == "disabled");

            for attr in attrs.iter() {
                let attr_name = attr.name.local.as_ref();
                if attr_name == "id"
                    && !attr.value.is_empty()
                    && attr.value.len() <= max_string_bytes
                {
                    if ids.len() < MAX_NAME_INDEX_ENTRIES {
                        ids.entry(attr.value.to_string()).or_insert(idx);
                    } else {
                        truncated = true;
                    }
                } else if name.local.as_ref() == "label"
                    && attr_name == "for"
                    && !attr.value.is_empty()
                    && attr.value.len() <= max_string_bytes
                {
                    if label_count < MAX_NAME_INDEX_ENTRIES {
                        labels_for
                            .entry(attr.value.to_string())
                            .or_default()
                            .push(idx);
                        label_count += 1;
                    } else {
                        truncated = true;
                    }
                }
            }
        }

        let mut out = Vec::with_capacity(maximum.min(scan_len));
        let mut remaining_name_work = MAX_NAME_WORK;
        for idx in 0..scan_len {
            if !rendered[idx] {
                continue;
            }
            let node = arena.node(idx);
            let NodeData::Element { name, attrs, .. } = &node.data else {
                continue;
            };
            let node_id = idx + 1;
            let mut element = AccessibilityElement {
                node_id,
                tag: name.local.as_ref().to_owned(),
                role: None,
                aria_labelledby: None,
                aria_label: None,
                title: None,
                alt: None,
                value: None,
                input_type: None,
                aria_disabled: None,
                aria_checked: None,
                aria_selected: None,
                aria_expanded: None,
                tabindex: None,
                text: String::new(),
                label: String::new(),
                href: false,
                disabled: inherited_disabled[idx],
                checked: false,
                selected: false,
                multiple: false,
                contenteditable: false,
            };
            let attrs = attrs.borrow();
            for attr in attrs.iter() {
                let attr_name = attr.name.local.as_ref();
                match attr_name {
                    "href" => element.href = true,
                    "disabled" => element.disabled = true,
                    "checked" => element.checked = true,
                    "selected" => element.selected = true,
                    "multiple" => element.multiple = true,
                    "contenteditable" => {
                        element.contenteditable = !attr.value.trim().eq_ignore_ascii_case("false")
                    }
                    "role" => {
                        let (roles, was_truncated) =
                            bounded_accessibility_string(attr.value.trim(), max_string_bytes);
                        truncated |= was_truncated;
                        element.role = first_supported_aria_role(&roles).map(str::to_owned);
                    }
                    "aria-labelledby" => copy_accessibility_attr(
                        &mut element.aria_labelledby,
                        attr.value.as_ref(),
                        max_string_bytes,
                        &mut truncated,
                    ),
                    "aria-label" => copy_accessibility_attr(
                        &mut element.aria_label,
                        attr.value.as_ref(),
                        max_string_bytes,
                        &mut truncated,
                    ),
                    "title" => copy_accessibility_attr(
                        &mut element.title,
                        attr.value.as_ref(),
                        max_string_bytes,
                        &mut truncated,
                    ),
                    "alt" => copy_accessibility_attr(
                        &mut element.alt,
                        attr.value.as_ref(),
                        max_string_bytes,
                        &mut truncated,
                    ),
                    "value" => copy_accessibility_attr(
                        &mut element.value,
                        attr.value.as_ref(),
                        max_string_bytes,
                        &mut truncated,
                    ),
                    "type" => copy_accessibility_attr(
                        &mut element.input_type,
                        attr.value.as_ref(),
                        max_string_bytes,
                        &mut truncated,
                    ),
                    "aria-disabled" => copy_accessibility_attr(
                        &mut element.aria_disabled,
                        attr.value.as_ref(),
                        max_string_bytes,
                        &mut truncated,
                    ),
                    "aria-checked" => copy_accessibility_attr(
                        &mut element.aria_checked,
                        attr.value.as_ref(),
                        max_string_bytes,
                        &mut truncated,
                    ),
                    "aria-selected" => copy_accessibility_attr(
                        &mut element.aria_selected,
                        attr.value.as_ref(),
                        max_string_bytes,
                        &mut truncated,
                    ),
                    "aria-expanded" => copy_accessibility_attr(
                        &mut element.aria_expanded,
                        attr.value.as_ref(),
                        max_string_bytes,
                        &mut truncated,
                    ),
                    "tabindex" => copy_accessibility_attr(
                        &mut element.tabindex,
                        attr.value.as_ref(),
                        max_string_bytes,
                        &mut truncated,
                    ),
                    _ => {}
                }
            }
            drop(attrs);

            let hidden_input = element.tag == "input"
                && element
                    .input_type
                    .as_deref()
                    .is_some_and(|value| value.eq_ignore_ascii_case("hidden"));
            if hidden_input {
                continue;
            }
            if element
                .role
                .as_deref()
                .is_some_and(|role| matches!(role, "none" | "presentation"))
            {
                if presentational_role_conflicts(&element) {
                    element.role = None;
                } else {
                    continue;
                }
            }
            let native = native_accessibility_element(&element);
            let explicit = element
                .role
                .as_deref()
                .is_some_and(|role| role != "generic");
            let direct_text =
                bounded_direct_accessibility_text(node, max_string_bytes, &mut truncated);
            let has_rendered_child = has_rendered_element_child(&arena, idx, &rendered);
            element.text = if native || explicit {
                bounded_accessibility_text(
                    &arena,
                    idx,
                    &rendered,
                    max_string_bytes,
                    &mut remaining_name_work,
                    &mut truncated,
                )
            } else if !direct_text.is_empty() {
                direct_text
            } else if !has_rendered_child {
                bounded_accessibility_text(
                    &arena,
                    idx,
                    &rendered,
                    max_string_bytes,
                    &mut remaining_name_work,
                    &mut truncated,
                )
            } else {
                String::new()
            };
            let has_authored_name = element
                .aria_labelledby
                .as_deref()
                .is_some_and(|value| !value.is_empty())
                || element
                    .aria_label
                    .as_deref()
                    .is_some_and(|value| !value.is_empty());
            let redundant_generic =
                !native && !explicit && ancestor_consumes_accessibility_text(&arena, idx, 64);
            let potentially_semantic = element.tag != "label"
                && !redundant_generic
                && (native
                    || explicit
                    || has_authored_name
                    || !element.text.is_empty()
                    || element.tabindex.is_some()
                    || element.contenteditable);
            if !potentially_semantic {
                continue;
            }
            if out.len() == maximum {
                truncated = true;
                break;
            }
            element.label = AccessibilityNameResolver {
                arena: &arena,
                rendered: &rendered,
                ids: &ids,
                labels_for: &labels_for,
                maximum: max_string_bytes,
                remaining_work: &mut remaining_name_work,
                truncated: &mut truncated,
            }
            .resolve(idx, &element);
            out.push(element);
        }
        (out, truncated)
    }

    /// Element by the stable 1-based document-order `node_id` used by WPT
    /// checks and inspector DTOs.
    pub fn element_by_node_id(&self, node_id: usize) -> Option<MatchedElement> {
        let idx = node_id.checked_sub(1)?;
        let arena = ElementArena::build(&self.dom.document);
        if idx >= arena.len() {
            return None;
        }
        Some(MatchedElement::from_node(arena.node(idx), node_id))
    }

    /// Full descendant text content for an element, by stable 1-based
    /// document-order `node_id`.
    pub fn element_text_content(&self, node_id: usize) -> Option<String> {
        let idx = node_id.checked_sub(1)?;
        let arena = ElementArena::build(&self.dom.document);
        if idx >= arena.len() {
            return None;
        }
        Some(descendant_text(arena.node(idx)))
    }

    /// HTML serialisation of an element's children (`Element.innerHTML`) by
    /// stable 1-based document-order `node_id`.
    pub fn element_inner_html(&self, node_id: usize) -> Option<String> {
        let idx = node_id.checked_sub(1)?;
        let arena = ElementArena::build(&self.dom.document);
        if idx >= arena.len() {
            return None;
        }
        Some(crate::html_serialize::serialize_children_to_string(
            &arena.nodes[idx],
            crate::html_serialize::Scripting::Enabled,
        ))
    }

    /// HTML serialisation of an element and its descendants
    /// (`Element.outerHTML`) by stable 1-based document-order `node_id`.
    pub fn element_outer_html(&self, node_id: usize) -> Option<String> {
        let idx = node_id.checked_sub(1)?;
        let arena = ElementArena::build(&self.dom.document);
        if idx >= arena.len() {
            return None;
        }
        Some(crate::html_serialize::serialize_to_string(
            &arena.nodes[idx],
            crate::html_serialize::Scripting::Enabled,
        ))
    }

    /// Immediate element-child count for read-only DOM host projections.
    pub fn element_child_count(&self, node_id: usize) -> Option<usize> {
        let idx = node_id.checked_sub(1)?;
        let arena = ElementArena::build(&self.dom.document);
        if idx >= arena.len() {
            return None;
        }
        let mut count = 0;
        let mut child = arena.first_element_child_of(idx);
        while let Some(child_idx) = child {
            count += 1;
            child = arena.next_element(child_idx);
        }
        Some(count)
    }

    /// Related element by stable 1-based document-order `node_id`.
    pub fn related_element_by_node_id(
        &self,
        node_id: usize,
        relation: ElementRelation,
    ) -> Option<MatchedElement> {
        let idx = node_id.checked_sub(1)?;
        let arena = ElementArena::build(&self.dom.document);
        if idx >= arena.len() {
            return None;
        }
        let related = match relation {
            ElementRelation::Parent => arena.parent(idx),
            ElementRelation::FirstChild => arena.first_element_child_of(idx),
            ElementRelation::LastChild => {
                let mut last = arena.first_element_child_of(idx)?;
                while let Some(next) = arena.next_element(last) {
                    last = next;
                }
                Some(last)
            }
            ElementRelation::PreviousSibling => arena.prev_element(idx),
            ElementRelation::NextSibling => arena.next_element(idx),
        }?;
        Some(MatchedElement::from_node(arena.node(related), related + 1))
    }

    /// Whether a stable 1-based `node_id` matches the parsed selector list.
    pub fn matches_selector(&self, node_id: usize, selector: &Selector) -> bool {
        let Some(idx) = node_id.checked_sub(1) else {
            return false;
        };
        let arena = ElementArena::build(&self.dom.document);
        if idx >= arena.len() {
            return false;
        }
        let layout = LayoutDom::new(&arena, idx);
        dom_apis::element_matches(&layout, selector.as_stylo_list(), QuirksMode::NoQuirks)
    }

    /// All elements matching `selector`, in document order. Each element's
    /// `node_id` is its 1-based document-order index among elements — the
    /// stable correlation key for WPT `computed-style`/`element-attribute`.
    pub fn query_all(&self, selector: &Selector) -> Vec<MatchedElement> {
        let arena = ElementArena::build(&self.dom.document);
        let list = selector.as_stylo_list();
        let mut out = Vec::new();
        for idx in 0..arena.len() {
            let layout = LayoutDom::new(&arena, idx);
            if dom_apis::element_matches(&layout, list, QuirksMode::NoQuirks) {
                let node_id = idx + 1;
                out.push(MatchedElement::from_node(arena.node(idx), node_id));
            }
        }
        out
    }

    /// First element matching `selector`, or `None`.
    pub fn query_first(&self, selector: &Selector) -> Option<MatchedElement> {
        let arena = ElementArena::build(&self.dom.document);
        let list = selector.as_stylo_list();
        for idx in 0..arena.len() {
            let layout = LayoutDom::new(&arena, idx);
            if dom_apis::element_matches(&layout, list, QuirksMode::NoQuirks) {
                let node_id = idx + 1;
                return Some(MatchedElement::from_node(arena.node(idx), node_id));
            }
        }
        None
    }
}

fn native_accessibility_element(element: &AccessibilityElement) -> bool {
    matches!(
        element.tag.as_str(),
        "a" | "button"
            | "input"
            | "select"
            | "textarea"
            | "img"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "ul"
            | "ol"
            | "li"
            | "main"
            | "nav"
            | "header"
            | "footer"
            | "form"
            | "option"
    ) && (element.tag != "a" || element.href)
}

fn presentational_role_conflicts(element: &AccessibilityElement) -> bool {
    element.contenteditable
        || element
            .tabindex
            .as_deref()
            .and_then(|value| value.parse::<i32>().ok())
            .is_some_and(|value| value >= 0)
        || matches!(
            element.tag.as_str(),
            "button" | "input" | "select" | "textarea" | "option"
        )
        || (element.tag == "a" && element.href)
}

fn first_supported_aria_role(value: &str) -> Option<&'static str> {
    for token in value.split_ascii_whitespace() {
        if token.len() > 32 {
            continue;
        }
        let role = match token.to_ascii_lowercase().as_str() {
            "alert" => "alert",
            "alertdialog" => "alertdialog",
            "application" => "application",
            "article" => "article",
            "banner" => "banner",
            "button" => "button",
            "cell" => "cell",
            "checkbox" => "checkbox",
            "columnheader" => "columnheader",
            "combobox" => "combobox",
            "complementary" => "complementary",
            "contentinfo" => "contentinfo",
            "definition" => "definition",
            "dialog" => "dialog",
            "document" => "document",
            "feed" => "feed",
            "figure" => "figure",
            "form" => "form",
            "generic" => "generic",
            "grid" => "grid",
            "gridcell" => "gridcell",
            "group" => "group",
            "heading" => "heading",
            "img" | "image" => "image",
            "link" => "link",
            "list" => "list",
            "listbox" => "listbox",
            "listitem" => "listitem",
            "log" => "log",
            "main" => "main",
            "marquee" => "marquee",
            "math" => "math",
            "menu" => "menu",
            "menubar" => "menubar",
            "menuitem" => "menuitem",
            "menuitemcheckbox" => "menuitemcheckbox",
            "menuitemradio" => "menuitemradio",
            "meter" => "meter",
            "navigation" => "navigation",
            "none" => "none",
            "note" => "note",
            "option" => "option",
            "presentation" => "presentation",
            "progressbar" => "progressbar",
            "radio" => "radio",
            "radiogroup" => "radiogroup",
            "region" => "region",
            "row" => "row",
            "rowgroup" => "rowgroup",
            "rowheader" => "rowheader",
            "scrollbar" => "scrollbar",
            "search" => "search",
            "searchbox" => "searchbox",
            "separator" => "separator",
            "slider" => "slider",
            "spinbutton" => "spinbutton",
            "status" => "status",
            "switch" => "switch",
            "tab" => "tab",
            "table" => "table",
            "tablist" => "tablist",
            "tabpanel" => "tabpanel",
            "term" => "term",
            "textbox" => "textbox",
            "timer" => "timer",
            "toolbar" => "toolbar",
            "tooltip" => "tooltip",
            "tree" => "tree",
            "treegrid" => "treegrid",
            "treeitem" => "treeitem",
            _ => continue,
        };
        return Some(role);
    }
    None
}

struct AccessibilityNameResolver<'a> {
    arena: &'a ElementArena,
    rendered: &'a [bool],
    ids: &'a HashMap<String, usize>,
    labels_for: &'a HashMap<String, Vec<usize>>,
    maximum: usize,
    remaining_work: &'a mut usize,
    truncated: &'a mut bool,
}

impl AccessibilityNameResolver<'_> {
    fn resolve(&mut self, idx: usize, element: &AccessibilityElement) -> String {
        let mut visited = HashSet::new();
        visited.insert(idx);
        if let Some(labelledby) = element.aria_labelledby.as_deref() {
            let name = self.labelledby_name(labelledby, &mut visited);
            if !name.is_empty() {
                return name;
            }
        }
        if let Some(label) = element
            .aria_label
            .as_deref()
            .filter(|label| !label.is_empty())
        {
            return label.to_owned();
        }
        if is_labelable_control(&element.tag) {
            let mut name = String::new();
            if let Some(id) =
                bounded_node_attr(self.arena.node(idx), "id", self.maximum, self.truncated)
                && let Some(labels) = self.labels_for.get(&id)
            {
                for label_idx in labels.iter().copied() {
                    let part = self.reference_name(label_idx, &mut visited);
                    append_name_part(&mut name, &part, self.maximum, self.truncated);
                }
            }
            let mut parent = self.arena.parent(idx);
            for _ in 0..64 {
                let Some(parent_idx) = parent else { break };
                if node_tag(self.arena.node(parent_idx)) == Some("label") {
                    let part = self.reference_name(parent_idx, &mut visited);
                    append_name_part(&mut name, &part, self.maximum, self.truncated);
                    parent = None;
                    break;
                }
                parent = self.arena.parent(parent_idx);
            }
            if parent.is_some() {
                *self.truncated = true;
            }
            if !name.is_empty() {
                return name;
            }
        }
        element
            .alt
            .as_deref()
            .filter(|value| !value.is_empty())
            .or_else(|| element.title.as_deref().filter(|value| !value.is_empty()))
            .or_else(|| {
                (element.tag == "input")
                    .then_some(element.value.as_deref())
                    .flatten()
                    .filter(|value| !value.is_empty())
            })
            .or_else(|| accessibility_name_from_content(element).then_some(element.text.as_str()))
            .unwrap_or("")
            .to_owned()
    }

    fn labelledby_name(&mut self, labelledby: &str, visited: &mut HashSet<usize>) -> String {
        let mut name = String::new();
        for (references, id) in labelledby.split_ascii_whitespace().enumerate() {
            if references == 32 {
                *self.truncated = true;
                break;
            }
            if let Some(idx) = self.ids.get(id).copied() {
                let part = self.reference_name(idx, visited);
                append_name_part(&mut name, &part, self.maximum, self.truncated);
            }
        }
        name
    }

    fn reference_name(&mut self, idx: usize, visited: &mut HashSet<usize>) -> String {
        if idx >= self.rendered.len() || !visited.insert(idx) {
            return String::new();
        }
        if !consume_name_work(self.remaining_work, self.truncated) {
            return String::new();
        }
        let node = self.arena.node(idx);
        if let Some(labelledby) =
            bounded_node_attr(node, "aria-labelledby", self.maximum, self.truncated)
        {
            let name = self.labelledby_name(&labelledby, visited);
            if !name.is_empty() {
                return name;
            }
        }
        if let Some(label) = bounded_node_attr(node, "aria-label", self.maximum, self.truncated)
            && !label.is_empty()
        {
            return label;
        }
        if self.rendered[idx] {
            bounded_accessibility_text(
                self.arena,
                idx,
                self.rendered,
                self.maximum,
                self.remaining_work,
                self.truncated,
            )
        } else {
            bounded_referenced_accessibility_text(
                self.arena,
                idx,
                self.rendered.len(),
                self.maximum,
                self.remaining_work,
                self.truncated,
            )
        }
    }
}

fn is_labelable_control(tag: &str) -> bool {
    matches!(tag, "button" | "input" | "select" | "textarea")
}

fn accessibility_name_from_content(element: &AccessibilityElement) -> bool {
    if let Some(role) = element.role.as_deref() {
        return matches!(
            role.to_ascii_lowercase().as_str(),
            "button"
                | "link"
                | "checkbox"
                | "radio"
                | "heading"
                | "listitem"
                | "menuitem"
                | "option"
                | "tab"
        );
    }
    matches!(
        element.tag.as_str(),
        "a" | "button" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "li" | "option"
    ) || !native_accessibility_element(element)
}

fn ancestor_consumes_accessibility_text(
    arena: &ElementArena,
    idx: usize,
    maximum_depth: usize,
) -> bool {
    let mut parent = arena.parent(idx);
    for _ in 0..maximum_depth {
        let Some(parent_idx) = parent else {
            return false;
        };
        let node = arena.node(parent_idx);
        let tag = node_tag(node).unwrap_or("");
        let role = bounded_role_token(node);
        if role.as_deref().is_some_and(|role| {
            matches!(
                role,
                "button"
                    | "link"
                    | "checkbox"
                    | "radio"
                    | "heading"
                    | "listitem"
                    | "menuitem"
                    | "option"
                    | "tab"
            )
        }) || matches!(
            tag,
            "button" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "li"
        ) || (tag == "a" && node_has_attr(node, "href"))
        {
            return true;
        }
        parent = arena.parent(parent_idx);
    }
    false
}

fn node_has_attr(node: &Node, name: &str) -> bool {
    let NodeData::Element { attrs, .. } = &node.data else {
        return false;
    };
    attrs
        .borrow()
        .iter()
        .any(|attr| attr.name.local.as_ref() == name)
}

fn bounded_role_token(node: &Node) -> Option<String> {
    let NodeData::Element { attrs, .. } = &node.data else {
        return None;
    };
    let attrs = attrs.borrow();
    let role = attrs
        .iter()
        .find(|attr| attr.name.local.as_ref() == "role")?;
    first_supported_aria_role(role.value.as_ref()).map(str::to_owned)
}

fn node_tag(node: &Node) -> Option<&str> {
    let NodeData::Element { name, .. } = &node.data else {
        return None;
    };
    Some(name.local.as_ref())
}

fn copy_accessibility_attr(
    target: &mut Option<String>,
    value: &str,
    maximum: usize,
    truncated: &mut bool,
) {
    let (value, was_truncated) = bounded_accessibility_string(value.trim(), maximum);
    *target = Some(value);
    *truncated |= was_truncated;
}

fn bounded_node_attr(
    node: &Node,
    name: &str,
    maximum: usize,
    truncated: &mut bool,
) -> Option<String> {
    let NodeData::Element { attrs, .. } = &node.data else {
        return None;
    };
    let attrs = attrs.borrow();
    let value = attrs.iter().find(|attr| attr.name.local.as_ref() == name)?;
    let (value, was_truncated) = bounded_accessibility_string(value.value.trim(), maximum);
    *truncated |= was_truncated;
    Some(value)
}

fn has_rendered_element_child(arena: &ElementArena, idx: usize, rendered: &[bool]) -> bool {
    let mut child = arena.first_element_child_of(idx);
    while let Some(child_idx) = child {
        if child_idx < rendered.len() && rendered[child_idx] {
            return true;
        }
        child = arena.next_element(child_idx);
    }
    false
}

fn bounded_direct_accessibility_text(node: &Node, maximum: usize, truncated: &mut bool) -> String {
    let mut out = String::with_capacity(maximum.min(64));
    if !matches!(node_tag(node), Some("script" | "style")) {
        for child in node.children.borrow().iter() {
            if let NodeData::Text { contents } = &child.data {
                append_accessibility_text(&mut out, &contents.borrow(), maximum, truncated);
            }
        }
    }
    out.trim().to_owned()
}

fn bounded_accessibility_text(
    arena: &ElementArena,
    idx: usize,
    rendered: &[bool],
    maximum: usize,
    remaining_work: &mut usize,
    truncated: &mut bool,
) -> String {
    let mut out = String::with_capacity(maximum.min(64));
    collect_accessibility_text(
        arena,
        idx,
        rendered,
        &mut out,
        maximum,
        remaining_work,
        truncated,
    );
    out.trim().to_owned()
}

fn bounded_referenced_accessibility_text(
    arena: &ElementArena,
    idx: usize,
    scan_len: usize,
    maximum: usize,
    remaining_work: &mut usize,
    truncated: &mut bool,
) -> String {
    let mut out = String::with_capacity(maximum.min(64));
    collect_referenced_accessibility_text(
        arena,
        idx,
        scan_len,
        &mut out,
        maximum,
        remaining_work,
        truncated,
    );
    out.trim().to_owned()
}

fn collect_referenced_accessibility_text(
    arena: &ElementArena,
    idx: usize,
    scan_len: usize,
    out: &mut String,
    maximum: usize,
    remaining_work: &mut usize,
    truncated: &mut bool,
) {
    if idx >= scan_len || out.len() >= maximum || !consume_name_work(remaining_work, truncated) {
        return;
    }
    let node = arena.node(idx);
    if matches!(node_tag(node), Some("script" | "style")) {
        return;
    }
    let mut element_child = arena.first_element_child_of(idx);
    for child in node.children.borrow().iter() {
        match &child.data {
            NodeData::Text { contents } => {
                append_accessibility_text(out, &contents.borrow(), maximum, truncated);
            }
            NodeData::Element { .. } => {
                if let Some(child_idx) = element_child {
                    collect_referenced_accessibility_text(
                        arena,
                        child_idx,
                        scan_len,
                        out,
                        maximum,
                        remaining_work,
                        truncated,
                    );
                    element_child = arena.next_element(child_idx);
                }
            }
            _ => {}
        }
        if out.len() >= maximum {
            return;
        }
    }
}

fn collect_accessibility_text(
    arena: &ElementArena,
    idx: usize,
    rendered: &[bool],
    out: &mut String,
    maximum: usize,
    remaining_work: &mut usize,
    truncated: &mut bool,
) {
    if idx >= rendered.len()
        || !rendered[idx]
        || out.len() >= maximum
        || !consume_name_work(remaining_work, truncated)
    {
        return;
    }
    let node = arena.node(idx);
    if matches!(node_tag(node), Some("script" | "style")) {
        return;
    }
    let mut element_child = arena.first_element_child_of(idx);
    for child in node.children.borrow().iter() {
        match &child.data {
            NodeData::Text { contents } => {
                append_accessibility_text(out, &contents.borrow(), maximum, truncated);
            }
            NodeData::Element { .. } => {
                if let Some(child_idx) = element_child {
                    collect_accessibility_text(
                        arena,
                        child_idx,
                        rendered,
                        out,
                        maximum,
                        remaining_work,
                        truncated,
                    );
                    element_child = arena.next_element(child_idx);
                }
            }
            _ => {}
        }
        if out.len() >= maximum {
            return;
        }
    }
}

fn consume_name_work(remaining: &mut usize, truncated: &mut bool) -> bool {
    if *remaining == 0 {
        *truncated = true;
        return false;
    }
    *remaining -= 1;
    true
}

fn append_name_part(out: &mut String, part: &str, maximum: usize, truncated: &mut bool) {
    if part.is_empty() {
        return;
    }
    if !out.is_empty() {
        append_accessibility_text(out, " ", maximum, truncated);
    }
    append_accessibility_text(out, part, maximum, truncated);
}

fn append_accessibility_text(out: &mut String, value: &str, maximum: usize, truncated: &mut bool) {
    for character in value.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if out.len() + character.len_utf8() > maximum {
            *truncated = true;
            return;
        }
        out.push(character);
    }
}

fn bounded_accessibility_string(value: &str, maximum: usize) -> (String, bool) {
    let mut out = String::with_capacity(value.len().min(maximum));
    let mut truncated = false;
    append_accessibility_text(&mut out, value, maximum, &mut truncated);
    (out, truncated)
}

// ---------------------------------------------------------------------------
// RefCell-ish helper to keep tests' borrow scope tight.
// ---------------------------------------------------------------------------
#[allow(dead_code)]
fn _refcell_witness<'a, T: 'a>(c: &'a RefCell<T>) -> &'a T {
    // Demo that &RefCell access stays within scope — never actually called.
    let _ = c;
    panic!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(html: &str) -> Document {
        Document::parse(html).unwrap()
    }

    fn sel(s: &str) -> Selector {
        Selector::parse(s).unwrap_or_else(|e| panic!("selector {s:?} failed: {e:?}"))
    }

    #[test]
    fn parses_simple_selectors() {
        for s in [
            "*",
            "div",
            "#main",
            ".row",
            "div.foo",
            "a[href]",
            "a[href='/x']",
            "a[href^='https']",
            "a, div, span",
            "div > p",
            "div p",
            "div + p",
            "div ~ p",
            ":is(.a, .b)",
            ":where(.a)",
            "section:has(> img)",
            "p:nth-child(odd)",
            "li:first-child",
            "li:last-child",
            "input:disabled",
            "input:checked",
            "a:link",
            "div:empty",
            ":root",
        ] {
            assert!(Selector::parse(s).is_ok(), "should parse: {s}");
        }
    }

    #[test]
    fn rejects_malformed_selector() {
        for s in ["div >", "", ":::weird", "div[", ">>>", ":nonexistent"] {
            assert!(Selector::parse(s).is_err(), "should reject: {s:?}");
        }
    }

    #[test]
    fn matches_tag_id_class() {
        let d = doc("<html><body>\
             <div id='main' class='row big'>one</div>\
             <div class='row'>two</div>\
             <span class='row'>three</span>\
             </body></html>");
        assert_eq!(d.query_all(&sel("div")).len(), 2);
        let main = d.query_all(&sel("#main"));
        assert_eq!(main.len(), 1);
        assert_eq!(main[0].tag, "div");
        assert_eq!(d.query_all(&sel(".row")).len(), 3);
        assert_eq!(d.query_all(&sel("span.row")).len(), 1);
        assert_eq!(d.query_all(&sel("span.row"))[0].text, "three");
        let first = d.query_first(&sel(".row")).unwrap();
        assert_eq!(first.id.as_deref(), Some("main"));
    }

    #[test]
    fn matches_attribute_selectors() {
        let d = doc("<a href='https://a.test/'>A</a>\
             <a href='mailto:x@y.test'>M</a>\
             <a name='toc'>N</a>");
        assert_eq!(d.query_all(&sel("a[href]")).len(), 2);
        let https = d.query_all(&sel("a[href^='https']"));
        assert_eq!(https.len(), 1);
        assert_eq!(https[0].text, "A");
        assert_eq!(d.query_all(&sel("a[href^='mailto']")).len(), 1);
        assert_eq!(d.query_all(&sel("a[name]")).len(), 1);
        assert_eq!(d.query_all(&sel("a[href$='.test/']")).len(), 1);
        assert_eq!(d.query_all(&sel("a[href*='a.test']")).len(), 1);
        let d2 = doc("<div data-x='a b c'></div><div data-x='ab'></div>");
        assert_eq!(d2.query_all(&sel("div[data-x~='b']")).len(), 1);
        let d3 = doc("<div lang='en-US'></div><div lang='en'></div><div lang='end'></div>");
        assert_eq!(d3.query_all(&sel("div[lang|='en']")).len(), 2);
    }

    #[test]
    fn matches_combinators() {
        let d = doc("<ul><li>1</li><li>2</li><li>3</li></ul>\
             <ol><li>x</li></ol>");
        assert_eq!(d.query_all(&sel("li")).len(), 4);
        assert_eq!(d.query_all(&sel("li:first-child")).len(), 2); // <ul>'s first + <ol>'s first
        assert_eq!(d.query_all(&sel("ul > li")).len(), 3);
        assert_eq!(d.query_all(&sel("li + li")).len(), 2);
    }

    #[test]
    fn matches_is_where_has() {
        let d = doc("<header><h1>T</h1></header>\
             <section><h2>S</h2></section>\
             <article><img src='x.png'></article>");
        assert_eq!(d.query_all(&sel(":is(h1, h2)")).len(), 2);
        let articles_with_img = d.query_all(&sel("article:has(> img[src='x.png'])"));
        assert_eq!(articles_with_img.len(), 1);
        assert_eq!(articles_with_img[0].tag, "article");
        assert_eq!(d.query_all(&sel("section:not(:has(img))")).len(), 1);
    }

    #[test]
    fn matches_nth_child_and_pseudos() {
        let d = doc("<ul><li>a</li><li>b</li><li>c</li><li>d</li><li>e</li></ul>");
        assert_eq!(d.query_all(&sel("li:nth-child(odd)")).len(), 3);
        assert_eq!(d.query_all(&sel("li:first-child")).len(), 1);
        assert_eq!(d.query_all(&sel("li:last-child")).len(), 1);
        let d2 = doc("<div></div><div>x</div>");
        assert_eq!(d2.query_all(&sel("div:empty")).len(), 1);
        let root = doc("<html></html>").query_all(&sel(":root"));
        assert_eq!(root.len(), 1);
        assert_eq!(root[0].tag, "html");
    }

    #[test]
    fn matches_form_pseudos_from_attrs() {
        let d = doc("<form>\
             <input type='checkbox' checked>\
             <input type='text' disabled>\
             <input type='email' required>\
             <button disabled>ok</button>\
             <input type='radio'>\
             </form>");
        assert_eq!(d.query_all(&sel("input:checked")).len(), 1);
        assert_eq!(d.query_all(&sel("input:disabled")).len(), 1);
        assert_eq!(d.query_all(&sel(":disabled")).len(), 2);
        assert_eq!(d.query_all(&sel("input:required")).len(), 1);
        assert_eq!(d.query_all(&sel("input:optional")).len(), 3);
    }

    #[test]
    fn matches_link_pseudos() {
        let d = doc("<a href='/x'>x</a><a name='n'>n</a><area href='/y'>");
        assert_eq!(d.query_all(&sel(":link")).len(), 2);
        assert_eq!(d.query_all(&sel(":any-link")).len(), 2);
    }

    #[test]
    fn matched_element_projects_attributes() {
        let d = doc("<input type='email' name='e' required value='x@y.test'>");
        let m = &d.query_all(&sel("input[name='e']"))[0];
        assert_eq!(m.tag, "input");
        let map: std::collections::HashMap<&str, &str> = m
            .attributes
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(map.get("type").copied(), Some("email"));
        assert_eq!(map.get("required").copied(), Some(""));
        assert_eq!(map.get("value").copied(), Some("x@y.test"));
    }

    #[test]
    fn deep_nested_walk_collects_in_document_order() {
        // html5ever synthesises the implicit <html><head><body> shell, so
        // the three <div>s land at element-ids 4, 5, 6 (html=1, head=2,
        // body=3). The point of the test is that ordering + ids are stable
        // and document-ordered, not any particular offset.
        let d = doc("<div id='a'>\
                <div id='b'>\
                    <div id='c'></div>\
                </div>\
             </div>");
        let matches = d.query_all(&sel("div"));
        let ids: Vec<_> = matches.iter().map(|m| m.id.clone().unwrap()).collect();
        assert_eq!(ids, vec!["a".to_owned(), "b".into(), "c".into()]);
        // IDs are sequential in document order.
        let nids: Vec<_> = matches.iter().map(|m| m.node_id).collect();
        assert_eq!(nids, (1..=6).skip(3).collect::<Vec<_>>());
    }

    #[test]
    fn element_by_node_id_uses_same_stable_ids_as_queries() {
        let d = doc(
            "<html><body><section><p id='target' style='display: grid'></p></section></body></html>",
        );
        let target = d.query_first(&sel("#target")).unwrap();
        let by_id = d.element_by_node_id(target.node_id).unwrap();
        assert_eq!(by_id.id.as_deref(), Some("target"));
        assert_eq!(by_id.tag, "p");
        assert_eq!(by_id.attributes, target.attributes);
        assert!(d.element_by_node_id(0).is_none());
        assert!(d.element_by_node_id(usize::MAX).is_none());
    }

    #[test]
    fn matches_selector_uses_stable_node_ids() {
        let d = doc("<html><body><p id='a' class='lead'>one</p><p id='b'>two</p></body></html>");
        let a = d.query_first(&sel("#a")).unwrap();
        let b = d.query_first(&sel("#b")).unwrap();
        assert!(d.matches_selector(a.node_id, &sel("p.lead")));
        assert!(!d.matches_selector(b.node_id, &sel("p.lead")));
        assert!(!d.matches_selector(usize::MAX, &sel("p")));
    }

    #[test]
    fn element_tree_projection_uses_stable_node_ids() {
        let d = doc(
            "<main id='root'><section id='first'><p id='child'>Alpha <b>Beta</b></p></section><aside id='next'>Next</aside></main>",
        );
        let root = d.query_first(&sel("#root")).unwrap();
        let first = d.query_first(&sel("#first")).unwrap();
        let child = d.query_first(&sel("#child")).unwrap();

        assert_eq!(d.element_child_count(root.node_id), Some(2));
        assert_eq!(
            d.element_text_content(child.node_id).as_deref(),
            Some("Alpha Beta")
        );
        assert_eq!(
            d.related_element_by_node_id(child.node_id, ElementRelation::Parent)
                .unwrap()
                .id
                .as_deref(),
            Some("first")
        );
        assert_eq!(
            d.related_element_by_node_id(root.node_id, ElementRelation::FirstChild)
                .unwrap()
                .id
                .as_deref(),
            Some("first")
        );
        assert_eq!(
            d.related_element_by_node_id(root.node_id, ElementRelation::LastChild)
                .unwrap()
                .id
                .as_deref(),
            Some("next")
        );
        assert_eq!(
            d.related_element_by_node_id(first.node_id, ElementRelation::NextSibling)
                .unwrap()
                .id
                .as_deref(),
            Some("next")
        );
        assert!(
            d.related_element_by_node_id(first.node_id, ElementRelation::PreviousSibling)
                .is_none()
        );
    }

    #[test]
    fn arena_topology_is_consistent() {
        // html5ever inserts an implicit <head>, so we get 8 elements:
        // html, head, body, header, h1, main, p, p.
        let d = doc("<html><body>\
             <header><h1>T</h1></header>\
             <main><p>1</p><p>2</p></main>\
             </body></html>");
        let arena = ElementArena::build(&d.dom.document);
        assert_eq!(arena.len(), 8);
        // The root has no parent.
        assert_eq!(arena.parent(0), None);
        // body has parent html.
        let body_idx = arena
            .nodes
            .iter()
            .position(|n| matches!(&n.data, NodeData::Element { name, .. } if name.local.as_ref() == "body"))
            .unwrap();
        assert_eq!(arena.parent(body_idx), Some(0)); // 0 = html
        // The two <p>s are siblings.
        let p_idx = arena
            .nodes
            .iter()
            .position(
                |n| matches!(&n.data, NodeData::Element { name, .. } if name.local.as_ref() == "p"),
            )
            .unwrap();
        assert_eq!(arena.next_element(p_idx), Some(p_idx + 1));
        assert_eq!(arena.prev_element(p_idx + 1), Some(p_idx));
    }
}
