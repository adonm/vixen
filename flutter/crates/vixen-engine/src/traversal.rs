//! DOM § 6 `TreeWalker` + `NodeIterator` — the filtered traversal model
//! the two `NodeFilter`-based iterators reduce to (Phase 6 DOM prep). Pure
//! over a [`Tree`] trait the host hook implements on the real DOM; the JS
//! `NodeFilter` callback is the host hook's job ([`NodeFilter`] trait).
//!
//! What lives here:
//! - [`NodeType`] — the DOM `nodeType` numeric codes (Element / Text /
//!   Comment / Document / …).
//! - [`WhatToShow`] — the § 6.1 `whatToShow` bitmask (`SHOW_*` constants;
//!   `SHOW_ALL` = all bits).
//! - [`FilterResult`] — § 6.1 `FILTER_ACCEPT` / `FILTER_REJECT` /
//!   `FILTER_SKIP`.
//! - [`NodeFilter`] — the trait the host hook implements over the JS
//!   callback (`accept(node) → FilterResult`).
//! - [`Tree`] — the tree-access trait the host hook implements over the
//!   real DOM (`parent` / `first_child` / `last_child` / `prev_sibling` /
//!   `next_sibling` / `node_type`).
//! - [`TreeWalker`] — § 6.2 the rooted, stateful walker with
//!   `parent_node` / `first_child` / `last_child` / `next_sibling` /
//!   `previous_sibling` / `next_node` / `previous_node`. `FILTER_REJECT`
//!   skips the rejected node's subtree; `FILTER_SKIP` traverses into it.
//! - [`NodeIterator`] — § 6.3 the flat preorder iterator with `next_node`
//!   / `previous_node` + the reference-node state + the `adjust_for_removal`
//!   step the host hook consults when a node is removed from the tree.
//!
//! What does *not* live here:
//! - The real DOM tree — the host hook implements [`Tree`] over it; this
//!   module is the pure traversal algorithm.
//! - The JS `NodeFilter` callback invocation — the host hook implements
//!   [`NodeFilter`] over the JS function (or the `whatToShow`-only case
//!   with no callback).
//! - The § 6.2 "the root is never returned" invariant is the caller's to
//!   respect — [`TreeWalker`] starts `current = root` and never moves to a
//!   node outside `root`'s subtree.
//!
//! ## Reject vs Skip
//!
//! Per § 6.2, `TreeWalker` honours the distinction: `FILTER_REJECT` skips
//! the node **and** its subtree; `FILTER_SKIP` skips only the node but
//! traverses its descendants. Per § 6.3, `NodeIterator` treats `REJECT`
//! and `SKIP` identically (the flat cursor has no subtree state) — both
//! continue to the next preorder node.
//!
//! Reference: <https://dom.spec.whatwg.org/#interface-nodefilter>,
//! TreeWalker <https://dom.spec.whatwg.org/#interface-treewalker>,
//! NodeIterator <https://dom.spec.whatwg.org/#interface-nodeiterator>.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// NodeType + WhatToShow + FilterResult + NodeFilter
// ---------------------------------------------------------------------------

/// The DOM `nodeType` numeric codes (the § 1 `Node.nodeType` constants).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum NodeType {
    Element = 1,
    Text = 3,
    Comment = 8,
    Document = 9,
    DocumentType = 10,
    DocumentFragment = 11,
}

impl NodeType {
    /// The numeric `nodeType` code.
    pub fn code(self) -> u16 {
        self as u16
    }

    /// The `whatToShow` bit for this node type (`1 << (code − 1)`).
    pub fn show_bit(self) -> u32 {
        1u32 << (self.code() as u32 - 1)
    }
}

/// The § 6.1 `whatToShow` bitmask. The `SHOW_*` constants are the
/// `1 << (nodeType − 1)` bits; [`WhatToShow::ALL`] is every bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct WhatToShow(pub u32);

impl WhatToShow {
    /// `SHOW_ALL` — every bit set.
    pub const ALL: Self = Self(0xFFFF_FFFF);
    /// `SHOW_ELEMENT` (nodeType 1).
    pub const ELEMENT: Self = Self(0x1);
    /// `SHOW_TEXT` (nodeType 3).
    pub const TEXT: Self = Self(0x4);
    /// `SHOW_COMMENT` (nodeType 8).
    pub const COMMENT: Self = Self(0x80);
    /// `SHOW_DOCUMENT` (nodeType 9).
    pub const DOCUMENT: Self = Self(0x100);
    /// `SHOW_DOCUMENT_TYPE` (nodeType 10).
    pub const DOCUMENT_TYPE: Self = Self(0x200);
    /// `SHOW_DOCUMENT_FRAGMENT` (nodeType 11).
    pub const DOCUMENT_FRAGMENT: Self = Self(0x400);

    /// `true` iff `node_type`'s bit is set in this mask.
    pub fn shows(self, node_type: NodeType) -> bool {
        (self.0 & node_type.show_bit()) != 0
    }

    /// Combine two masks (bitwise OR).
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// The § 6.1 `NodeFilter` result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FilterResult {
    /// `FILTER_ACCEPT` — the node is returned by the iterator/walker.
    Accept,
    /// `FILTER_REJECT` — skip the node; for `TreeWalker`, skip its subtree
    /// too. For `NodeIterator`, equivalent to `Skip`.
    Reject,
    /// `FILTER_SKIP` — skip the node but traverse its descendants.
    Skip,
}

/// The `NodeFilter` trait the host hook implements over the JS callback
/// (the `acceptNode(node)` function). The host hook combines the
/// `whatToShow` mask first (a node not in the mask ⇒ [`FilterResult::Skip`])
/// before calling the user callback.
pub trait NodeFilter {
    /// Run the filter against `node` (a host-hook node id).
    fn accept(&self, node: usize) -> FilterResult;
}

/// A `NodeFilter` that accepts every node (the `whatToShow`-only case with
/// no JS callback). Used by tests + as the default when the host hook has
/// only a `whatToShow` mask.
#[derive(Debug, Clone, Copy, Default)]
pub struct AcceptAll;

impl NodeFilter for AcceptAll {
    fn accept(&self, _node: usize) -> FilterResult {
        FilterResult::Accept
    }
}

// ---------------------------------------------------------------------------
// Tree trait
// ---------------------------------------------------------------------------

/// The tree-access trait the host hook implements over the real DOM. Every
/// method takes a node id (`usize`) the host hook's table maps to a real
/// `Node`. The `root` is the traversal root the iterator/walker was created
/// with; traversal never crosses above it.
pub trait Tree {
    /// The parent of `node`, or `None` for the root / detached nodes.
    fn parent(&self, node: usize) -> Option<usize>;
    /// The first child of `node`, or `None`.
    fn first_child(&self, node: usize) -> Option<usize>;
    /// The last child of `node`, or `None`.
    fn last_child(&self, node: usize) -> Option<usize>;
    /// The previous sibling of `node`, or `None`.
    fn prev_sibling(&self, node: usize) -> Option<usize>;
    /// The next sibling of `node`, or `None`.
    fn next_sibling(&self, node: usize) -> Option<usize>;
    /// The `NodeType` of `node`.
    fn node_type(&self, node: usize) -> NodeType;
}

// ---------------------------------------------------------------------------
// Preorder helpers (filtered)
// ---------------------------------------------------------------------------

/// Run the combined `whatToShow` + user filter against `node`. A node not in
/// the `whatToShow` mask ⇒ [`FilterResult::Skip`] (the § 6.1 precedence).
fn filter_node<T: Tree, F: NodeFilter>(
    tree: &T,
    what_to_show: WhatToShow,
    filter: &F,
    node: usize,
) -> FilterResult {
    if !what_to_show.shows(tree.node_type(node)) {
        FilterResult::Skip
    } else {
        filter.accept(node)
    }
}

/// The preorder successor of `node` within `root`'s subtree: first child if
/// any, else next sibling, else the parent's next sibling (walking up, not
/// above `root`). `None` at the end of the subtree.
fn preorder_successor<T: Tree>(tree: &T, node: usize, root: usize) -> Option<usize> {
    if let Some(fc) = tree.first_child(node) {
        return Some(fc);
    }
    let mut n = node;
    while n != root {
        if let Some(ns) = tree.next_sibling(n) {
            return Some(ns);
        }
        n = tree.parent(n)?;
    }
    None
}

/// The preorder predecessor of `node` within `root`'s subtree: previous
/// sibling's last descendant if any, else the parent (which is `≥ root`).
/// `None` when `node == root`.
fn preorder_predecessor<T: Tree>(tree: &T, node: usize, root: usize) -> Option<usize> {
    if node == root {
        return None;
    }
    if let Some(ps) = tree.prev_sibling(node) {
        return Some(last_descendant(tree, ps));
    }
    tree.parent(node)
}

/// The last (rightmost-deepest) descendant of `node`, or `node` itself if
/// it has no children.
fn last_descendant<T: Tree>(tree: &T, mut node: usize) -> usize {
    while let Some(lc) = tree.last_child(node) {
        node = lc;
    }
    node
}

/// The next node after `node` skipping `node`'s subtree: next sibling, or
/// the parent's next sibling (walking up, not above `root`). Used for
/// `FILTER_REJECT` (skip the subtree).
fn subtree_skip_successor<T: Tree>(tree: &T, node: usize, root: usize) -> Option<usize> {
    let mut n = node;
    while n != root {
        if let Some(ns) = tree.next_sibling(n) {
            return Some(ns);
        }
        n = tree.parent(n)?;
    }
    None
}

/// The previous node before `node` skipping `node`'s subtree: previous
/// sibling, or the parent (not above `root`). Used for `FILTER_REJECT` in
/// the backward walk.
fn subtree_skip_predecessor<T: Tree>(tree: &T, node: usize, root: usize) -> Option<usize> {
    if node == root {
        return None;
    }
    if let Some(ps) = tree.prev_sibling(node) {
        return Some(ps);
    }
    let p = tree.parent(node)?;
    if p == root { None } else { Some(p) }
}

/// Walk preorder forward from `start` (within `root`'s subtree) to the first
/// node the filter accepts. `FILTER_REJECT` skips the subtree; `FILTER_SKIP`
/// traverses into it.
fn walk_accept_forward<T: Tree, F: NodeFilter>(
    tree: &T,
    what_to_show: WhatToShow,
    filter: &F,
    start: usize,
    root: usize,
) -> Option<usize> {
    let mut node = Some(start);
    while let Some(n) = node {
        match filter_node(tree, what_to_show, filter, n) {
            FilterResult::Accept => return Some(n),
            FilterResult::Reject => node = subtree_skip_successor(tree, n, root),
            FilterResult::Skip => node = preorder_successor(tree, n, root),
        }
    }
    None
}

/// Walk preorder backward from `start` (within `root`'s subtree) to the
/// first node the filter accepts. `FILTER_REJECT` skips the subtree;
/// `FILTER_SKIP` traverses into it (the previous-sibling's last descendant).
fn walk_accept_backward<T: Tree, F: NodeFilter>(
    tree: &T,
    what_to_show: WhatToShow,
    filter: &F,
    start: usize,
    root: usize,
) -> Option<usize> {
    let mut node = Some(start);
    while let Some(n) = node {
        match filter_node(tree, what_to_show, filter, n) {
            FilterResult::Accept => return Some(n),
            FilterResult::Reject => node = subtree_skip_predecessor(tree, n, root),
            FilterResult::Skip => node = preorder_predecessor(tree, n, root),
        }
    }
    None
}

// ---------------------------------------------------------------------------
// TreeWalker
// ---------------------------------------------------------------------------

/// DOM § 6.2 `TreeWalker` — a rooted, stateful filtered walker. The
/// `current` node is the last-accepted node (initially the root). Every
/// method moves `current` + returns the new node, or `None` if no acceptable
/// node lies in the movement direction.
#[derive(Debug, Clone, Copy)]
pub struct TreeWalker {
    /// The traversal root (never moved above).
    pub root: usize,
    /// The current node (initially `root`).
    pub current: usize,
    /// The `whatToShow` mask.
    pub what_to_show: WhatToShow,
}

impl TreeWalker {
    /// Construct a walker rooted at `root` with `current = root`.
    pub const fn new(root: usize, what_to_show: WhatToShow) -> Self {
        Self {
            root,
            current: root,
            what_to_show,
        }
    }

    /// § 6.2 `parentNode()` — the nearest accepted ancestor (the root is a
    /// candidate; once `current` reaches the root, no further).
    pub fn parent_node<T: Tree, F: NodeFilter>(&mut self, tree: &T, filter: &F) -> Option<usize> {
        let mut node = self.current;
        while node != self.root {
            let parent = tree.parent(node)?;
            if filter_node(tree, self.what_to_show, filter, parent) == FilterResult::Accept {
                self.current = parent;
                return Some(parent);
            }
            node = parent;
        }
        None
    }

    /// § 6.2 `firstChild()` — the first accepted node in preorder within
    /// `current`'s subtree.
    pub fn first_child<T: Tree, F: NodeFilter>(&mut self, tree: &T, filter: &F) -> Option<usize> {
        let start = tree.first_child(self.current)?;
        let found = walk_accept_forward(tree, self.what_to_show, filter, start, self.current);
        if let Some(n) = found {
            self.current = n;
        }
        found
    }

    /// § 6.2 `lastChild()` — the last accepted node in reverse-preorder
    /// within `current`'s subtree (the rightmost-deepest acceptable child).
    pub fn last_child<T: Tree, F: NodeFilter>(&mut self, tree: &T, filter: &F) -> Option<usize> {
        let start = tree.last_child(self.current)?;
        let found = walk_accept_backward(tree, self.what_to_show, filter, start, self.current);
        if let Some(n) = found {
            self.current = n;
        }
        found
    }

    /// § 6.2 `nextSibling()` — the next accepted node after `current` within
    /// `current`'s parent's subtree (a skipped sibling's descendants are
    /// traversed; a rejected sibling's subtree is skipped).
    pub fn next_sibling<T: Tree, F: NodeFilter>(&mut self, tree: &T, filter: &F) -> Option<usize> {
        let parent = tree.parent(self.current)?;
        let start = tree.next_sibling(self.current)?;
        let found = walk_accept_forward(tree, self.what_to_show, filter, start, parent);
        if let Some(n) = found {
            self.current = n;
        }
        found
    }

    /// § 6.2 `previousSibling()` — the previous accepted node before
    /// `current` within `current`'s parent's subtree.
    pub fn previous_sibling<T: Tree, F: NodeFilter>(
        &mut self,
        tree: &T,
        filter: &F,
    ) -> Option<usize> {
        let parent = tree.parent(self.current)?;
        let start = tree.prev_sibling(self.current)?;
        let found = walk_accept_backward(tree, self.what_to_show, filter, start, parent);
        if let Some(n) = found {
            self.current = n;
        }
        found
    }

    /// § 6.2 `nextNode()` — the next accepted node in preorder (across the
    /// whole subtree, starting after `current`).
    pub fn next_node<T: Tree, F: NodeFilter>(&mut self, tree: &T, filter: &F) -> Option<usize> {
        let start = preorder_successor(tree, self.current, self.root)?;
        let found = walk_accept_forward(tree, self.what_to_show, filter, start, self.root);
        if let Some(n) = found {
            self.current = n;
        }
        found
    }

    /// § 6.2 `previousNode()` — the previous accepted node in preorder
    /// (across the whole subtree, starting before `current`).
    pub fn previous_node<T: Tree, F: NodeFilter>(&mut self, tree: &T, filter: &F) -> Option<usize> {
        let start = preorder_predecessor(tree, self.current, self.root)?;
        let found = walk_accept_backward(tree, self.what_to_show, filter, start, self.root);
        if let Some(n) = found {
            self.current = n;
        }
        found
    }
}

// ---------------------------------------------------------------------------
// NodeIterator
// ---------------------------------------------------------------------------

/// DOM § 6.3 `NodeIterator` — a flat preorder iterator with a reference
/// node. `FILTER_REJECT` and `FILTER_SKIP` are treated identically (the
/// flat cursor has no subtree state).
#[derive(Debug, Clone, Copy)]
pub struct NodeIterator {
    /// The traversal root.
    pub root: usize,
    /// The reference node (the last-returned node, initially `root`).
    pub reference: usize,
    /// The `whatToShow` mask.
    pub what_to_show: WhatToShow,
}

impl NodeIterator {
    /// Construct an iterator rooted at `root` with `reference = root`.
    pub const fn new(root: usize, what_to_show: WhatToShow) -> Self {
        Self {
            root,
            reference: root,
            what_to_show,
        }
    }

    /// § 6.3 `nextNode()` — the next accepted node in preorder after the
    /// reference. `REJECT` and `SKIP` both continue to the next preorder
    /// node (no subtree skip).
    pub fn next_node<T: Tree, F: NodeFilter>(&mut self, tree: &T, filter: &F) -> Option<usize> {
        let mut node = preorder_successor(tree, self.reference, self.root);
        while let Some(n) = node {
            if filter_node(tree, self.what_to_show, filter, n) == FilterResult::Accept {
                self.reference = n;
                return Some(n);
            }
            // NodeIterator: REJECT == SKIP → continue to next preorder node.
            node = preorder_successor(tree, n, self.root);
        }
        None
    }

    /// § 6.3 `previousNode()` — the previous accepted node in preorder
    /// before the reference.
    pub fn previous_node<T: Tree, F: NodeFilter>(&mut self, tree: &T, filter: &F) -> Option<usize> {
        let mut node = preorder_predecessor(tree, self.reference, self.root);
        while let Some(n) = node {
            if filter_node(tree, self.what_to_show, filter, n) == FilterResult::Accept {
                self.reference = n;
                return Some(n);
            }
            node = preorder_predecessor(tree, n, self.root);
        }
        None
    }

    /// § 6.3 "node removal" adjustment — the host hook consults this when a
    /// node (a subtree root) is removed from the tree. If the reference is
    /// the removed node or a descendant, the reference moves to the removed
    /// subtree's previous sibling (or the parent if no previous sibling),
    /// per the § 6.3 algorithm. Returns the new reference (which may be
    /// unchanged if the reference was outside the removed subtree).
    pub fn adjust_for_removal<T: Tree>(&mut self, tree: &T, removed: usize) -> usize {
        // Is the reference inside the removed subtree? Walk up from the
        // reference; if `removed` is an ancestor (or the reference itself),
        // adjust.
        let mut n = self.reference;
        loop {
            if n == removed {
                // Adjust: the removed subtree's previous sibling's last
                // descendant, else the parent.
                let parent = tree.parent(removed);
                let new_ref = match tree.prev_sibling(removed) {
                    Some(ps) => last_descendant(tree, ps),
                    None => parent.unwrap_or(self.root),
                };
                self.reference = new_ref;
                return new_ref;
            }
            match tree.parent(n) {
                Some(p) => n = p,
                None => break,
            }
        }
        self.reference
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A tiny in-memory tree for the tests. Nodes are keyed by `usize`.
    /// `root` is the document; the tree shape:
    /// ```text
    /// 0 (Document)
    /// ├─ 1 (Element)
    /// │  ├─ 2 (Text "a")
    /// │  └─ 3 (Element)
    /// │     ├─ 4 (Text "b")
    /// │     └─ 5 (Comment "c")
    /// └─ 6 (Element)
    /// ```
    struct TestTree {
        parents: HashMap<usize, usize>,
        first_child: HashMap<usize, usize>,
        last_child: HashMap<usize, usize>,
        prev_sibling: HashMap<usize, usize>,
        next_sibling: HashMap<usize, usize>,
        types: HashMap<usize, NodeType>,
    }

    impl TestTree {
        fn build() -> Self {
            let mut t = Self {
                parents: HashMap::new(),
                first_child: HashMap::new(),
                last_child: HashMap::new(),
                prev_sibling: HashMap::new(),
                next_sibling: HashMap::new(),
                types: HashMap::new(),
            };
            // Helper to register a child under a parent (appended in order).
            let mut order: HashMap<usize, Vec<usize>> = HashMap::new();
            let mut add = |t: &mut Self, parent: usize, child: usize, ty: NodeType| {
                t.parents.insert(child, parent);
                t.types.insert(child, ty);
                order.entry(parent).or_default().push(child);
            };
            add(&mut t, 0, 1, NodeType::Element);
            add(&mut t, 0, 6, NodeType::Element);
            add(&mut t, 1, 2, NodeType::Text);
            add(&mut t, 1, 3, NodeType::Element);
            add(&mut t, 3, 4, NodeType::Text);
            add(&mut t, 3, 5, NodeType::Comment);
            // Link siblings + first/last child from the order map.
            for (parent, children) in &order {
                t.first_child.insert(*parent, children[0]);
                t.last_child.insert(*parent, *children.last().unwrap());
                for w in children.windows(2) {
                    t.next_sibling.insert(w[0], w[1]);
                    t.prev_sibling.insert(w[1], w[0]);
                }
            }
            t
        }
    }

    impl Tree for TestTree {
        fn parent(&self, n: usize) -> Option<usize> {
            self.parents.get(&n).copied()
        }
        fn first_child(&self, n: usize) -> Option<usize> {
            self.first_child.get(&n).copied()
        }
        fn last_child(&self, n: usize) -> Option<usize> {
            self.last_child.get(&n).copied()
        }
        fn prev_sibling(&self, n: usize) -> Option<usize> {
            self.prev_sibling.get(&n).copied()
        }
        fn next_sibling(&self, n: usize) -> Option<usize> {
            self.next_sibling.get(&n).copied()
        }
        fn node_type(&self, n: usize) -> NodeType {
            *self.types.get(&n).unwrap_or(&NodeType::Element)
        }
    }

    /// A filter that accepts only the given node ids.
    struct AcceptOnly(Vec<usize>);
    impl NodeFilter for AcceptOnly {
        fn accept(&self, node: usize) -> FilterResult {
            if self.0.contains(&node) {
                FilterResult::Accept
            } else {
                FilterResult::Skip
            }
        }
    }

    /// A filter that rejects the given node ids (skip their subtree).
    struct RejectOnly(Vec<usize>);
    impl NodeFilter for RejectOnly {
        fn accept(&self, node: usize) -> FilterResult {
            if self.0.contains(&node) {
                FilterResult::Reject
            } else {
                FilterResult::Accept
            }
        }
    }

    // --- WhatToShow --------------------------------------------------

    #[test]
    fn show_bits_match_node_type() {
        assert_eq!(NodeType::Element.show_bit(), 0x1);
        assert_eq!(NodeType::Text.show_bit(), 0x4);
        assert_eq!(NodeType::Comment.show_bit(), 0x80);
        assert!(WhatToShow::ALL.shows(NodeType::Element));
        assert!(WhatToShow::TEXT.shows(NodeType::Text));
        assert!(!WhatToShow::TEXT.shows(NodeType::Element));
    }

    // --- TreeWalker, accept-all --------------------------------------

    #[test]
    fn walker_next_node_preorder() {
        let t = TestTree::build();
        let mut w = TreeWalker::new(0, WhatToShow::ALL);
        // Preorder from root: 0, 1, 2, 3, 4, 5, 6.
        assert_eq!(w.next_node(&t, &AcceptAll), Some(1));
        assert_eq!(w.next_node(&t, &AcceptAll), Some(2));
        assert_eq!(w.next_node(&t, &AcceptAll), Some(3));
        assert_eq!(w.next_node(&t, &AcceptAll), Some(4));
        assert_eq!(w.next_node(&t, &AcceptAll), Some(5));
        assert_eq!(w.next_node(&t, &AcceptAll), Some(6));
        assert_eq!(w.next_node(&t, &AcceptAll), None, "end of subtree");
    }

    #[test]
    fn walker_previous_node_reverse_preorder() {
        let t = TestTree::build();
        let mut w = TreeWalker::new(0, WhatToShow::ALL);
        w.current = 6; // walk back from 6.
        assert_eq!(w.previous_node(&t, &AcceptAll), Some(5));
        assert_eq!(w.previous_node(&t, &AcceptAll), Some(4));
        assert_eq!(w.previous_node(&t, &AcceptAll), Some(3));
        assert_eq!(w.previous_node(&t, &AcceptAll), Some(2));
        assert_eq!(w.previous_node(&t, &AcceptAll), Some(1));
        assert_eq!(
            w.previous_node(&t, &AcceptAll),
            Some(0),
            "root is a predecessor"
        );
        assert_eq!(w.previous_node(&t, &AcceptAll), None);
    }

    #[test]
    fn walker_first_and_last_child() {
        let t = TestTree::build();
        let mut w = TreeWalker::new(0, WhatToShow::ALL);
        assert_eq!(w.first_child(&t, &AcceptAll), Some(1));
        // current is now 1; last_child of 1 is node 3 (its last child).
        assert_eq!(w.last_child(&t, &AcceptAll), Some(3));
        // current is now 3; last_child of 3 is node 5.
        assert_eq!(w.last_child(&t, &AcceptAll), Some(5));
        // node 5 has no children.
        assert_eq!(w.last_child(&t, &AcceptAll), None);
    }

    #[test]
    fn walker_parent_node_walks_up() {
        let t = TestTree::build();
        let mut w = TreeWalker::new(0, WhatToShow::ALL);
        w.current = 4;
        assert_eq!(w.parent_node(&t, &AcceptAll), Some(3));
        assert_eq!(w.parent_node(&t, &AcceptAll), Some(1));
        assert_eq!(w.parent_node(&t, &AcceptAll), Some(0));
        assert_eq!(w.parent_node(&t, &AcceptAll), None, "root has no parent");
    }

    #[test]
    fn walker_next_and_previous_sibling() {
        let t = TestTree::build();
        let mut w = TreeWalker::new(0, WhatToShow::ALL);
        w.current = 1;
        assert_eq!(w.next_sibling(&t, &AcceptAll), Some(6));
        assert_eq!(w.previous_sibling(&t, &AcceptAll), Some(1));
    }

    // --- TreeWalker, whatToShow --------------------------------------

    #[test]
    fn walker_what_to_show_skips_non_matching() {
        let t = TestTree::build();
        let mut w = TreeWalker::new(0, WhatToShow::TEXT);
        // Only text nodes (2, 4) are accepted.
        assert_eq!(w.next_node(&t, &AcceptAll), Some(2));
        assert_eq!(w.next_node(&t, &AcceptAll), Some(4));
        assert_eq!(w.next_node(&t, &AcceptAll), None);
    }

    // --- TreeWalker, FILTER_REJECT skips subtree ---------------------

    #[test]
    fn walker_reject_skips_subtree() {
        let t = TestTree::build();
        let mut w = TreeWalker::new(0, WhatToShow::ALL);
        // Reject node 3 (whose subtree is 4, 5).
        let filter = RejectOnly(vec![3]);
        // next_node from 0: 1 (accept), then 2 (accept), then 3 is rejected →
        // skip its subtree → next is 6.
        assert_eq!(w.next_node(&t, &filter), Some(1));
        assert_eq!(w.next_node(&t, &filter), Some(2));
        assert_eq!(
            w.next_node(&t, &filter),
            Some(6),
            "node 3 rejected ⇒ 4,5 skipped"
        );
    }

    #[test]
    fn walker_skip_traverses_subtree() {
        let t = TestTree::build();
        let mut w = TreeWalker::new(0, WhatToShow::ALL);
        // Skip node 3 (but traverse its subtree).
        let filter = AcceptOnly(vec![0, 1, 2, 4, 5, 6]); // everyone except 3
        assert_eq!(w.next_node(&t, &filter), Some(1));
        assert_eq!(w.next_node(&t, &filter), Some(2));
        assert_eq!(
            w.next_node(&t, &filter),
            Some(4),
            "3 skipped but 4,5 visited"
        );
        assert_eq!(w.next_node(&t, &filter), Some(5));
        assert_eq!(w.next_node(&t, &filter), Some(6));
    }

    // --- NodeIterator ------------------------------------------------

    #[test]
    fn iterator_next_and_previous_node() {
        let t = TestTree::build();
        let mut it = NodeIterator::new(0, WhatToShow::ALL);
        assert_eq!(it.next_node(&t, &AcceptAll), Some(1));
        assert_eq!(it.next_node(&t, &AcceptAll), Some(2));
        assert_eq!(it.next_node(&t, &AcceptAll), Some(3));
        assert_eq!(it.previous_node(&t, &AcceptAll), Some(2));
        assert_eq!(it.previous_node(&t, &AcceptAll), Some(1));
    }

    #[test]
    fn iterator_reject_treated_as_skip() {
        let t = TestTree::build();
        let mut it = NodeIterator::new(0, WhatToShow::ALL);
        let filter = RejectOnly(vec![3]);
        // 1 (accept), 2 (accept), 3 rejected but iterator visits its children.
        assert_eq!(it.next_node(&t, &filter), Some(1));
        assert_eq!(it.next_node(&t, &filter), Some(2));
        assert_eq!(
            it.next_node(&t, &filter),
            Some(4),
            "REJECT == SKIP for NodeIterator"
        );
        assert_eq!(it.next_node(&t, &filter), Some(5));
        assert_eq!(it.next_node(&t, &filter), Some(6));
    }

    // --- NodeIterator.adjust_for_removal -----------------------------

    #[test]
    fn iterator_adjust_for_removal_moves_reference_out() {
        let t = TestTree::build();
        let mut it = NodeIterator::new(0, WhatToShow::ALL);
        it.reference = 4; // inside node 3's subtree
        // Remove node 3 (the subtree root). The reference (4) is inside it.
        // Adjusted reference = previous sibling of 3 = 2 (its last descendant;
        // 2 has no children → 2).
        let new_ref = it.adjust_for_removal(&t, 3);
        assert_eq!(new_ref, 2);
        assert_eq!(it.reference, 2);
    }

    #[test]
    fn iterator_adjust_for_removal_no_previous_sibling_uses_parent() {
        let t = TestTree::build();
        let mut it = NodeIterator::new(0, WhatToShow::ALL);
        it.reference = 2; // first child of 1, no previous sibling.
        // Remove node 2 itself. No previous sibling ⇒ parent (1).
        let new_ref = it.adjust_for_removal(&t, 2);
        assert_eq!(new_ref, 1);
    }

    #[test]
    fn iterator_adjust_for_removal_outside_subtree_is_noop() {
        let t = TestTree::build();
        let mut it = NodeIterator::new(0, WhatToShow::ALL);
        it.reference = 6; // outside node 3's subtree.
        let new_ref = it.adjust_for_removal(&t, 3);
        assert_eq!(
            new_ref, 6,
            "reference outside the removed subtree is unchanged"
        );
    }
}
