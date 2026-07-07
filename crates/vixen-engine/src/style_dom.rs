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
