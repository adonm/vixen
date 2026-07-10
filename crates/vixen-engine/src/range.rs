//! DOM § 5.2 `Range` + § 5.4 `Selection` — the boundary-point model the
//! `Range` / `Selection` host hooks + the editing command surface reduce to
//! (Phase 6 DOM prep). The boundary comparison + the collapsed/collapse/
//! direction logic is pure; the live DOM tree mutation (the § 5.3 "insert"
//! / "remove" content-stepping algorithms) is the host hook.
//!
//! What lives here:
//! - [`NodeRef`] — an opaque DOM-node handle (the host hook resolves to a
//!   real `Node`). Carries a [`DocumentOrder`] index (the pre-order DFS
//!   position the caller assigns) so two boundaries compare in document
//!   order by pure arithmetic.
//! - [`Boundary`] — a `(node, offset)` pair (the § 5.2 boundary-point). The
//!   offset is a child index for element/document nodes, a UTF-16 code-unit
//!   index for text/character-data nodes (the DOM's `Node.normalize`-safe
//!   convention).
//! - [`Boundary::compare`] — the § 5.2 "relative position" (`Before` /
//!   `Equal` / `After`) in document order, pure given the
//!   [`DocumentOrder`] indices.
//! - [`Range`] — the `(start, end)` pair with [`Range::is_collapsed`] +
//!   [`Range::collapse`] + the § 5.2 "valid range" invariants
//!   (`start ≤ end` in document order; same-root constraint).
//! - [`Selection`] — the `Range` list + the anchor/focus (the
//!   direction-aware extents) + `add_range` / `remove_all_ranges` /
//!   `collapse_to_node` + the `is_collapsed` + `direction` predicates.
//!
//! What does *not* live here:
//! - The live tree mutation (`surroundContents`, `insertNode`, `extractContents`,
//!   `cloneContents`) — the § 5.3 algorithms walk the real DOM; the host hook
//!   owns them.
//! - The `getClientRects` / `getBoundingClientRect` geometry (Phase 4
//!   layout layer).
//! - The `TreeWalker` / `NodeIterator` traversal the editing commands use
//!   (the host hook; this module only needs the pre-order index).
//! - Shadow-DOM re-targeting + `composedPath` (the [`crate::event_path`]
//!   neighbour handles the event surface; Range re-targeting is deferred).
//!
//! ## Document order
//!
//! A [`Boundary::compare`] needs document order. Without the real tree,
//! the caller assigns each node a [`DocumentOrder`] index = the pre-order
//! DFS position (the root is 0, first child 1, &c.). Two boundaries on the
//! *same* node compare by offset; two boundaries on *different* nodes
//! compare by document-order index (the ancestor/descendant offset case is
//! the caller's to refine — the common case of two distinct subtrees is
//! handled by the index). This keeps the comparison pure and unit-testable.
//!
//! Reference: <https://dom.spec.whatwg.org/#ranges>,
//! Selection <https://dom.spec.whatwg.org/#selection>.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// NodeRef + DocumentOrder
// ---------------------------------------------------------------------------

/// An opaque DOM-node handle (the host hook resolves to a real `Node`).
/// Carries a [`DocumentOrder`] index so [`Boundary::compare`] is pure
/// arithmetic — the caller assigns the pre-order DFS position when
/// constructing the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeRef {
    /// The opaque node id (the host hook's table key).
    pub id: usize,
    /// The pre-order DFS position (root = 0, first child = 1, …). Two
    /// nodes compare in document order by this index.
    pub order: DocumentOrder,
}

/// A pre-order DFS document-order index. The caller assigns one per node;
/// [`Boundary::compare`] uses it for cross-node ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct DocumentOrder(pub usize);

impl NodeRef {
    /// Construct a node handle with a given id + document-order index.
    pub const fn new(id: usize, order: DocumentOrder) -> Self {
        Self { id, order }
    }
}

// ---------------------------------------------------------------------------
// Boundary
// ---------------------------------------------------------------------------

/// A `Range` boundary point (DOM § 5.2): a `(node, offset)` pair. The
/// offset is a child index for element/document nodes, a UTF-16 code-unit
/// index for text/character-data nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Boundary {
    /// The node containing the boundary.
    pub node: NodeRef,
    /// The offset within `node` (child index for elements, UTF-16 index for
    /// text nodes).
    pub offset: usize,
}

impl Boundary {
    /// Construct a boundary.
    pub const fn at(node: NodeRef, offset: usize) -> Self {
        Self { node, offset }
    }

    /// The § 5.2 "relative position" of `self` vs `other` in document order:
    /// [`Ordering::Before`] / [`Ordering::Equal`] / [`Ordering::After`].
    ///
    /// Same node → compare by offset. Different nodes → compare by
    /// document-order index (the caller's pre-order assignment). The
    /// ancestor/descendant offset case (a boundary on a parent at a child
    /// index that points into a descendant's subtree) is not refined here;
    /// the common case of two distinct subtrees is correct, and the host
    /// hook with the real tree can refine the ancestor case if needed.
    pub fn compare(self, other: Boundary) -> Ordering {
        if self.node.id == other.node.id {
            match self.offset.cmp(&other.offset) {
                std::cmp::Ordering::Less => Ordering::Before,
                std::cmp::Ordering::Equal => Ordering::Equal,
                std::cmp::Ordering::Greater => Ordering::After,
            }
        } else {
            match self.node.order.cmp(&other.node.order) {
                std::cmp::Ordering::Less => Ordering::Before,
                std::cmp::Ordering::Equal => Ordering::Equal,
                std::cmp::Ordering::Greater => Ordering::After,
            }
        }
    }
}

/// The document-order position of one [`Boundary`] relative to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ordering {
    /// `self` is before `other` in document order.
    Before,
    /// `self` and `other` are the same boundary point.
    Equal,
    /// `self` is after `other` in document order.
    After,
}

// ---------------------------------------------------------------------------
// Range
// ---------------------------------------------------------------------------

/// DOM § 5.2 `Range` — a `(start, end)` boundary pair. The § 5.2 "valid
/// range" invariant (`start ≤ end` in document order, both boundaries in
/// the same root) is enforced by [`Range::new`] (which re-orders the
/// boundaries if needed); [`Range::new_unchecked`] is the escape hatch for
/// the host hook that already guarantees the order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Range {
    /// The start boundary (the § 5.2 "start" — always ≤ `end` in document
    /// order after construction).
    pub start: Boundary,
    /// The end boundary (the § 5.2 "end" — always ≥ `start`).
    pub end: Boundary,
}

impl Range {
    /// Construct a `Range`, re-ordering the boundaries so `start ≤ end` per
    /// the § 5.2 invariant. The same-root constraint (both boundaries in
    /// the same document) is the caller's to guarantee; the host hook
    /// rejects cross-root ranges at the `Range` constructor.
    pub fn new(start: Boundary, end: Boundary) -> Self {
        if start.compare(end) == Ordering::After {
            // Swap so start ≤ end.
            Self {
                start: end,
                end: start,
            }
        } else {
            Self { start, end }
        }
    }

    /// Construct without re-ordering (the caller guarantees `start ≤ end`).
    /// Used by the host hook when the order is already known.
    pub const fn new_unchecked(start: Boundary, end: Boundary) -> Self {
        Self { start, end }
    }

    /// `true` iff the range is collapsed (start == end) — the § 5.2
    /// "collapsed" predicate. A collapsed range selects no content.
    pub fn is_collapsed(self) -> bool {
        self.start == self.end
    }

    /// Collapse the range to one of its boundary points (§ 5.2
    /// `collapse(toStart)`). `to_start = true` collapses to `start`; `false`
    /// to `end`. The resulting range is collapsed.
    pub fn collapse(self, to_start: bool) -> Range {
        let point = if to_start { self.start } else { self.end };
        Range {
            start: point,
            end: point,
        }
    }

    /// Apply DOM `Range.setStart()`. If the new start is after the current end,
    /// the range collapses to the new boundary instead of swapping endpoints.
    pub fn set_start(&mut self, boundary: Boundary) {
        if boundary.compare(self.end) == Ordering::After {
            self.start = boundary;
            self.end = boundary;
        } else {
            self.start = boundary;
        }
    }

    /// Apply DOM `Range.setEnd()`. If the new end is before the current start,
    /// the range collapses to the new boundary instead of swapping endpoints.
    pub fn set_end(&mut self, boundary: Boundary) {
        if boundary.compare(self.start) == Ordering::Before {
            self.start = boundary;
            self.end = boundary;
        } else {
            self.end = boundary;
        }
    }

    /// `true` iff `boundary` lies within `[start, end]` (inclusive) — the
    /// § 5.2 "contained" predicate for a single boundary point. The § 5.2
    /// "contained node" predicate (a whole node is contained iff its parent
    /// boundaries are) is the host hook's, operating on the real tree.
    pub fn contains_boundary(self, boundary: Boundary) -> bool {
        let start_cmp = boundary.compare(self.start);
        let end_cmp = boundary.compare(self.end);
        !matches!(start_cmp, Ordering::Before) && !matches!(end_cmp, Ordering::After)
    }

    /// The intersection of two ranges, or `None` if they don't overlap.
    /// `max(start) .. min(end)`; `None` when that's empty.
    pub fn intersect(self, other: Range) -> Option<Range> {
        let start = if self.start.compare(other.start) == Ordering::Before {
            other.start
        } else {
            self.start
        };
        let end = if self.end.compare(other.end) == Ordering::After {
            other.end
        } else {
            self.end
        };
        if start.compare(end) == Ordering::After {
            None
        } else {
            Some(Range { start, end })
        }
    }
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

/// DOM § 5.4 `Selection` — the user's current selection. Carries a list of
/// `Range`s + the anchor/focus (the direction-aware extents; the anchor is
/// where the selection started, the focus is where it ended — the focus
/// may be before the anchor in document order, which is the "backward"
/// selection state).
///
/// v1.0 models the single-range case (most browsers enforce
/// `rangeCount ≤ 1` for the user selection); the multi-range list is kept
/// for the host hook that supports it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Selection {
    /// The selected ranges (typically one; the spec allows more).
    pub ranges: Vec<Range>,
    /// The anchor boundary (where the selection started).
    pub anchor: Option<Boundary>,
    /// The focus boundary (where the selection ended; may be before the
    /// anchor in document order — the "backward" selection).
    pub focus: Option<Boundary>,
}

/// The direction of a [`Selection`] — forward if the anchor is before the
/// focus in document order, backward if after.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SelectionDirection {
    /// Anchor before focus (the natural left-to-right selection).
    #[default]
    Forward,
    /// Focus before anchor (the right-to-left / "backward" selection).
    Backward,
    /// The selection is collapsed (anchor == focus) — no direction.
    None,
}

impl Selection {
    /// An empty selection (no ranges, no anchor/focus).
    pub fn empty() -> Self {
        Self::default()
    }

    /// `true` iff the selection is collapsed (every range is collapsed, or
    /// the selection is empty).
    pub fn is_collapsed(&self) -> bool {
        self.ranges.iter().all(|r| r.is_collapsed())
    }

    /// The number of ranges in the selection (`Selection.rangeCount`).
    pub fn range_count(&self) -> usize {
        self.ranges.len()
    }

    /// Add a range to the selection, setting the anchor/focus to the
    /// range's start/end (forward direction). The § 5.4 "add a range"
    /// algorithm is simplified here: v1.0 supports a single range, so this
    /// replaces the existing selection. The host hook that supports
    /// multi-range selection extends this.
    pub fn add_range(&mut self, range: Range) {
        self.anchor = Some(range.start);
        self.focus = Some(range.end);
        self.ranges = vec![range];
    }

    /// Collapse the selection to a boundary point (§ 5.4
    /// `collapse(node, offset)`). The anchor and focus both move to the
    /// point; the single range becomes collapsed.
    pub fn collapse_to(&mut self, boundary: Boundary) {
        let collapsed = Range {
            start: boundary,
            end: boundary,
        };
        self.ranges = vec![collapsed];
        self.anchor = Some(boundary);
        self.focus = Some(boundary);
    }

    /// Remove every range + clear the anchor/focus (§ 5.4
    /// `removeAllRanges`).
    pub fn remove_all_ranges(&mut self) {
        self.ranges.clear();
        self.anchor = None;
        self.focus = None;
    }

    /// The selection direction: forward if the anchor is before the focus,
    /// backward if after, none if collapsed or empty.
    pub fn direction(&self) -> SelectionDirection {
        match (self.anchor, self.focus) {
            (Some(a), Some(f)) => match a.compare(f) {
                Ordering::Before => SelectionDirection::Forward,
                Ordering::After => SelectionDirection::Backward,
                Ordering::Equal => SelectionDirection::None,
            },
            _ => SelectionDirection::None,
        }
    }

    /// Extend the selection to a new focus boundary (§ 5.4 `extend`): the
    /// anchor stays, the focus moves, and the single range is rebuilt as
    /// `(min(anchor, focus), max(anchor, focus))` (the range itself is
    /// always start ≤ end; the direction tracks the anchor-vs-focus order).
    pub fn extend_to(&mut self, new_focus: Boundary) {
        if let Some(anchor) = self.anchor {
            self.focus = Some(new_focus);
            let range = Range::new(anchor, new_focus);
            self.ranges = vec![range];
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Convenience: a node with id `i` and document-order `i`.
    fn n(i: usize) -> NodeRef {
        NodeRef::new(i, DocumentOrder(i))
    }

    // --- Boundary::compare --------------------------------------------

    #[test]
    fn boundary_compare_same_node_by_offset() {
        let node = n(1);
        let a = Boundary::at(node, 0);
        let b = Boundary::at(node, 5);
        assert_eq!(a.compare(b), Ordering::Before);
        assert_eq!(b.compare(a), Ordering::After);
        assert_eq!(a.compare(a), Ordering::Equal);
    }

    #[test]
    fn boundary_compare_different_nodes_by_order() {
        let a = Boundary::at(n(1), 99);
        let b = Boundary::at(n(2), 0);
        assert_eq!(a.compare(b), Ordering::Before);
        assert_eq!(b.compare(a), Ordering::After);
    }

    #[test]
    fn boundary_equal_when_same_node_and_offset() {
        let a = Boundary::at(n(5), 3);
        let b = Boundary::at(n(5), 3);
        assert_eq!(a.compare(b), Ordering::Equal);
        assert_eq!(a, b);
    }

    // --- Range ---------------------------------------------------------

    #[test]
    fn range_new_reorders_boundaries() {
        // Constructed end-before-start; should re-order.
        let start = Boundary::at(n(1), 0);
        let end = Boundary::at(n(2), 5);
        let r = Range::new(end, start);
        assert_eq!(r.start, start);
        assert_eq!(r.end, end);
    }

    #[test]
    fn range_new_preserves_ordered_boundaries() {
        let start = Boundary::at(n(1), 0);
        let end = Boundary::at(n(2), 5);
        let r = Range::new(start, end);
        assert_eq!(r.start, start);
        assert_eq!(r.end, end);
    }

    #[test]
    fn range_is_collapsed_when_start_equals_end() {
        let p = Boundary::at(n(1), 3);
        let r = Range::new(p, p);
        assert!(r.is_collapsed());
    }

    #[test]
    fn range_is_not_collapsed_when_distinct() {
        let r = Range::new(Boundary::at(n(1), 0), Boundary::at(n(1), 5));
        assert!(!r.is_collapsed());
    }

    #[test]
    fn range_collapse_to_start() {
        let r = Range::new(Boundary::at(n(1), 0), Boundary::at(n(2), 5));
        let c = r.collapse(true);
        assert!(c.is_collapsed());
        assert_eq!(c.start, Boundary::at(n(1), 0));
    }

    #[test]
    fn range_collapse_to_end() {
        let r = Range::new(Boundary::at(n(1), 0), Boundary::at(n(2), 5));
        let c = r.collapse(false);
        assert!(c.is_collapsed());
        assert_eq!(c.start, Boundary::at(n(2), 5));
    }

    #[test]
    fn range_set_start_crossing_end_collapses_at_new_start() {
        let mut range = Range::new(Boundary::at(n(1), 0), Boundary::at(n(2), 0));
        let new_start = Boundary::at(n(3), 1);

        range.set_start(new_start);

        assert_eq!(range.start, new_start);
        assert_eq!(range.end, new_start);
        assert!(range.is_collapsed());
    }

    #[test]
    fn range_set_end_crossing_start_collapses_at_new_end() {
        let mut range = Range::new(Boundary::at(n(2), 0), Boundary::at(n(3), 0));
        let new_end = Boundary::at(n(1), 1);

        range.set_end(new_end);

        assert_eq!(range.start, new_end);
        assert_eq!(range.end, new_end);
        assert!(range.is_collapsed());
    }

    #[test]
    fn range_contains_boundary_inclusive() {
        let r = Range::new(Boundary::at(n(1), 2), Boundary::at(n(1), 8));
        // Inside.
        assert!(r.contains_boundary(Boundary::at(n(1), 5)));
        // At start (inclusive).
        assert!(r.contains_boundary(Boundary::at(n(1), 2)));
        // At end (inclusive).
        assert!(r.contains_boundary(Boundary::at(n(1), 8)));
        // Before start.
        assert!(!r.contains_boundary(Boundary::at(n(1), 1)));
        // After end.
        assert!(!r.contains_boundary(Boundary::at(n(1), 9)));
    }

    #[test]
    fn range_intersect_overlapping() {
        let a = Range::new(Boundary::at(n(1), 0), Boundary::at(n(1), 10));
        let b = Range::new(Boundary::at(n(1), 5), Boundary::at(n(1), 15));
        let i = a.intersect(b).unwrap();
        assert_eq!(i.start, Boundary::at(n(1), 5));
        assert_eq!(i.end, Boundary::at(n(1), 10));
    }

    #[test]
    fn range_intersect_disjoint_is_none() {
        let a = Range::new(Boundary::at(n(1), 0), Boundary::at(n(1), 5));
        let b = Range::new(Boundary::at(n(1), 10), Boundary::at(n(1), 15));
        assert!(a.intersect(b).is_none());
    }

    #[test]
    fn range_intersect_contained() {
        let outer = Range::new(Boundary::at(n(1), 0), Boundary::at(n(1), 20));
        let inner = Range::new(Boundary::at(n(1), 5), Boundary::at(n(1), 10));
        let i = outer.intersect(inner).unwrap();
        assert_eq!(i, inner);
    }

    // --- Selection -----------------------------------------------------

    #[test]
    fn selection_default_is_empty() {
        let s = Selection::empty();
        assert_eq!(s.range_count(), 0);
        assert!(s.is_collapsed());
        assert_eq!(s.direction(), SelectionDirection::None);
        assert!(s.anchor.is_none());
        assert!(s.focus.is_none());
    }

    #[test]
    fn selection_add_range_sets_anchor_and_focus() {
        let mut s = Selection::empty();
        let r = Range::new(Boundary::at(n(1), 0), Boundary::at(n(2), 5));
        s.add_range(r);
        assert_eq!(s.range_count(), 1);
        assert_eq!(s.anchor, Some(Boundary::at(n(1), 0)));
        assert_eq!(s.focus, Some(Boundary::at(n(2), 5)));
        assert_eq!(s.direction(), SelectionDirection::Forward);
        assert!(!s.is_collapsed());
    }

    #[test]
    fn selection_collapse_to_boundary() {
        let mut s = Selection::empty();
        let r = Range::new(Boundary::at(n(1), 0), Boundary::at(n(2), 5));
        s.add_range(r);
        s.collapse_to(Boundary::at(n(3), 7));
        assert!(s.is_collapsed());
        assert_eq!(s.anchor, Some(Boundary::at(n(3), 7)));
        assert_eq!(s.focus, Some(Boundary::at(n(3), 7)));
        assert_eq!(s.direction(), SelectionDirection::None);
    }

    #[test]
    fn selection_remove_all_ranges_clears() {
        let mut s = Selection::empty();
        s.add_range(Range::new(Boundary::at(n(1), 0), Boundary::at(n(2), 5)));
        s.remove_all_ranges();
        assert_eq!(s.range_count(), 0);
        assert!(s.anchor.is_none());
        assert!(s.focus.is_none());
    }

    #[test]
    fn selection_extend_backward() {
        let mut s = Selection::empty();
        s.collapse_to(Boundary::at(n(5), 5));
        // Extend to a boundary earlier in document order → backward.
        s.extend_to(Boundary::at(n(2), 0));
        assert_eq!(s.anchor, Some(Boundary::at(n(5), 5)));
        assert_eq!(s.focus, Some(Boundary::at(n(2), 0)));
        assert_eq!(s.direction(), SelectionDirection::Backward);
        // The range itself is still start ≤ end.
        assert_eq!(s.ranges[0].start, Boundary::at(n(2), 0));
        assert_eq!(s.ranges[0].end, Boundary::at(n(5), 5));
    }

    #[test]
    fn selection_extend_forward() {
        let mut s = Selection::empty();
        s.collapse_to(Boundary::at(n(2), 0));
        s.extend_to(Boundary::at(n(5), 5));
        assert_eq!(s.direction(), SelectionDirection::Forward);
        assert_eq!(s.ranges[0].start, Boundary::at(n(2), 0));
        assert_eq!(s.ranges[0].end, Boundary::at(n(5), 5));
    }

    #[test]
    fn selection_extend_when_empty_is_noop() {
        let mut s = Selection::empty();
        s.extend_to(Boundary::at(n(1), 0));
        assert!(s.ranges.is_empty());
        assert!(s.anchor.is_none());
    }

    #[test]
    fn selection_direction_none_when_collapsed() {
        let mut s = Selection::empty();
        s.collapse_to(Boundary::at(n(1), 1));
        assert_eq!(s.direction(), SelectionDirection::None);
        assert!(s.is_collapsed());
    }
}
