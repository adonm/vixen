//! Transitional Vixen-owned Rust layout tree; ADR-022 replaces this ownership
//! with the Flutter-hosted formatter and deletes unused modules at cutover.
//!
//! This is the first Rust layout stage behind [`crate::page::Page`]. It mirrors
//! Ladybird's `TreeBuilder` seam at `0de15a5dd2a9` in shape, not in C++
//! ownership: styled DOM nodes become stable arena entries (`LayoutNodeId`),
//! hidden/non-rendered subtrees are dropped before layout, and deterministic
//! debug output gives Phase 4 a vertical fixture surface. Formatting contexts
//! will grow from this arena instead of landing as isolated pure modules.

#![forbid(unsafe_code)]

use crate::box_model::{AutoEdges, BoxModel, BoxSizing, Edges, LengthOrAuto};
use crate::display_list::{Color, Rect};
use crate::flex_resolve::FlexDirection;
use crate::grid_resolve::GridTrack;

mod build;
mod dump;
mod flow;
mod fragments;
mod style;

pub use build::build_layout_tree;
pub use dump::dump_layout_tree;
pub use flow::line_boxes_from_tree;
pub use fragments::{LayoutFragment, LayoutFragmentKind, layout_fragments_from_tree};

/// Translate descendants of nested scroll containers while leaving each
/// scrollport's own boxes in place. Fixed-position subtrees remain viewport
/// anchored, matching [`apply_root_scroll`].
pub fn apply_element_scrolls<F>(tree: &mut LayoutTree, mut offset_for: F)
where
    F: FnMut(usize) -> Option<(f32, f32)>,
{
    let mut fixed_subtree = vec![false; tree.nodes.len()];
    for index in 0..tree.nodes.len() {
        let node = &tree.nodes[index];
        if node.id == tree.root {
            continue;
        }
        let parent_fixed = node
            .parent
            .is_some_and(|parent| fixed_subtree[parent.index()]);
        fixed_subtree[index] = parent_fixed || node.style.position == LayoutPosition::Fixed;
        if fixed_subtree[index] {
            continue;
        }

        let mut offset = (0.0, 0.0);
        let mut parent = node.parent;
        while let Some(parent_id) = parent {
            let parent_node = &tree.nodes[parent_id.index()];
            if let Some(dom_node_id) = parent_node.dom_node_id
                && let Some(parent_offset) = offset_for(dom_node_id)
            {
                offset.0 += parent_offset.0;
                offset.1 += parent_offset.1;
            }
            parent = parent_node.parent;
        }
        if offset == (0.0, 0.0) {
            continue;
        }
        let node = &mut tree.nodes[index];
        translate_rect_mut(&mut node.rect, -offset.0, -offset.1);
        translate_rect_mut(&mut node.boxes.margin, -offset.0, -offset.1);
        translate_rect_mut(&mut node.boxes.border, -offset.0, -offset.1);
        translate_rect_mut(&mut node.boxes.padding, -offset.0, -offset.1);
        translate_rect_mut(&mut node.boxes.content, -offset.0, -offset.1);
    }
}

/// Translate the document-scrolling subtree into viewport coordinates while
/// leaving the viewport and fixed-position subtrees anchored.
pub fn apply_root_scroll(tree: &mut LayoutTree, offset: (f32, f32)) {
    if offset == (0.0, 0.0) {
        return;
    }
    let mut fixed_subtree = vec![false; tree.nodes.len()];
    for index in 0..tree.nodes.len() {
        let node = &tree.nodes[index];
        if node.id == tree.root {
            continue;
        }
        let parent_fixed = node
            .parent
            .is_some_and(|parent| fixed_subtree[parent.index()]);
        fixed_subtree[index] = parent_fixed || node.style.position == LayoutPosition::Fixed;
        if fixed_subtree[index] {
            continue;
        }
        let node = &mut tree.nodes[index];
        translate_rect_mut(&mut node.rect, -offset.0, -offset.1);
        translate_rect_mut(&mut node.boxes.margin, -offset.0, -offset.1);
        translate_rect_mut(&mut node.boxes.border, -offset.0, -offset.1);
        translate_rect_mut(&mut node.boxes.padding, -offset.0, -offset.1);
        translate_rect_mut(&mut node.boxes.content, -offset.0, -offset.1);
    }
}

fn translate_rect_mut(rect: &mut Rect, x: f32, y: f32) {
    rect.x += x;
    rect.y += y;
}

/// Stable index into a [`LayoutTree`] arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayoutNodeId(pub usize);

impl LayoutNodeId {
    pub const fn index(self) -> usize {
        self.0
    }
}

/// Coarse layout role for the initial formatting-context spine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutNodeKind {
    Viewport,
    Block,
    Inline,
    Text,
}

/// CSS display category retained by the layout tree. `display:none` never
/// reaches the arena; the builder drops those subtrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutDisplay {
    Block,
    Inline,
    Flex,
    Grid,
}

/// CSS positioning mode consumed by the first block/inline positioning slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutPosition {
    Static,
    Relative,
    Absolute,
    Fixed,
}

impl LayoutPosition {
    fn is_out_of_flow(self) -> bool {
        matches!(self, LayoutPosition::Absolute | LayoutPosition::Fixed)
    }
}

/// CSS overflow value consumed by paint clipping. `auto` and `scroll` both
/// establish a scrollport in this first non-interactive slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LayoutOverflow {
    #[default]
    Visible,
    Hidden,
    Clip,
    Scroll,
    Auto,
}

impl LayoutOverflow {
    pub fn clips_contents(self) -> bool {
        !matches!(self, LayoutOverflow::Visible)
    }

    pub fn programmatically_scrollable(self) -> bool {
        matches!(
            self,
            LayoutOverflow::Hidden | LayoutOverflow::Scroll | LayoutOverflow::Auto
        )
    }

    pub fn user_scrollable(self) -> bool {
        matches!(self, LayoutOverflow::Scroll | LayoutOverflow::Auto)
    }
}

/// Physical inset offsets for positioned boxes. `None` is CSS `auto`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LayoutInsets {
    pub top: Option<f32>,
    pub right: Option<f32>,
    pub bottom: Option<f32>,
    pub left: Option<f32>,
}

/// Positioned CSS boxes for one layout node. `rect` on [`LayoutNode`] aliases
/// `boxes.border` because the display-list builder paints border boxes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutBoxes {
    pub margin: Rect,
    pub border: Rect,
    pub padding: Rect,
    pub content: Rect,
}

impl LayoutBoxes {
    fn from_rect(rect: Rect) -> Self {
        Self {
            margin: rect,
            border: rect,
            padding: rect,
            content: rect,
        }
    }

    fn from_box_model(model: BoxModel, border_x: f32, border_y: f32) -> Self {
        Self {
            margin: translate_rect(model.margin_box(), border_x, border_y),
            border: translate_rect(model.border_box(), border_x, border_y),
            padding: translate_rect(model.padding_box(), border_x, border_y),
            content: translate_rect(model.content_box(), border_x, border_y),
        }
    }
}

/// The style subset consumed by the first block formatting-context slice.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutStyle {
    pub box_sizing: BoxSizing,
    pub position: LayoutPosition,
    pub overflow: LayoutOverflow,
    pub inset: LayoutInsets,
    pub margin: AutoEdges,
    pub border: Edges,
    pub padding: Edges,
    pub width: LengthOrAuto,
    pub height: LengthOrAuto,
    pub flex_direction: FlexDirection,
    pub flex_grow: f32,
    pub flex_shrink: f32,
    pub flex_basis: LengthOrAuto,
    pub row_gap: f32,
    pub column_gap: f32,
    pub color: Color,
    pub background_color: Option<Color>,
}

impl Default for LayoutStyle {
    fn default() -> Self {
        Self {
            box_sizing: BoxSizing::ContentBox,
            position: LayoutPosition::Static,
            overflow: LayoutOverflow::Visible,
            inset: LayoutInsets::default(),
            margin: AutoEdges::px_all(0.0),
            border: Edges::ZERO,
            padding: Edges::ZERO,
            width: LengthOrAuto::Auto,
            height: LengthOrAuto::Auto,
            flex_direction: FlexDirection::Row,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: LengthOrAuto::Auto,
            row_gap: 0.0,
            column_gap: 0.0,
            color: Color::BLACK,
            background_color: None,
        }
    }
}

/// One arena entry produced by the TreeBuilder slice.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutNode {
    pub id: LayoutNodeId,
    pub parent: Option<LayoutNodeId>,
    pub children: Vec<LayoutNodeId>,
    /// Stable 1-based document-order element id, matching selector/WPT ids.
    pub dom_node_id: Option<usize>,
    pub tag: Option<String>,
    pub html_id: Option<String>,
    pub kind: LayoutNodeKind,
    pub display: Option<LayoutDisplay>,
    pub grid_template_columns: Vec<GridTrack>,
    pub grid_template_rows: Vec<GridTrack>,
    pub style: LayoutStyle,
    pub rect: Rect,
    pub boxes: LayoutBoxes,
    pub text: Option<String>,
}

/// Arena-backed layout tree for one viewport.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutTree {
    pub viewport: (u32, u32),
    pub root: LayoutNodeId,
    pub nodes: Vec<LayoutNode>,
}

impl LayoutTree {
    pub fn node(&self, id: LayoutNodeId) -> &LayoutNode {
        &self.nodes[id.index()]
    }

    fn node_mut(&mut self, id: LayoutNodeId) -> &mut LayoutNode {
        &mut self.nodes[id.index()]
    }

    fn push(&mut self, mut node: LayoutNode) -> LayoutNodeId {
        let id = LayoutNodeId(self.nodes.len());
        node.id = id;
        self.nodes.push(node);
        id
    }

    /// Visible text represented by text layout nodes, in document order.
    pub fn visible_text(&self) -> String {
        let mut parts = Vec::new();
        for node in &self.nodes {
            if node.kind == LayoutNodeKind::Text
                && let Some(text) = node.text.as_deref()
                && !text.is_empty()
            {
                parts.push(text);
            }
        }
        parts.join(" ")
    }
}

fn translate_rect(rect: Rect, dx: f32, dy: f32) -> Rect {
    Rect::new(rect.x + dx, rect.y + dy, rect.w, rect.h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::Document;

    fn doc(html: &str) -> Document {
        Document::parse(html).unwrap()
    }

    fn tree(html: &str) -> LayoutTree {
        build_layout_tree(&doc(html), (120, 200), |_| Vec::new())
    }

    fn node_with_id<'a>(tree: &'a LayoutTree, html_id: &str) -> &'a LayoutNode {
        tree.nodes
            .iter()
            .find(|node| node.html_id.as_deref() == Some(html_id))
            .unwrap()
    }

    #[test]
    fn tree_builder_skips_head_and_hidden_subtrees() {
        let tree = tree(
            "<html><head><title>Hidden title</title></head><body><p>Visible</p><p hidden>Gone</p></body></html>",
        );
        assert_eq!(tree.visible_text(), "Visible");
        let dump = dump_layout_tree(&tree);
        assert!(dump.contains("tag=body"));
        assert!(dump.contains("tag=p"));
        assert!(!dump.contains("Hidden title"));
        assert!(!dump.contains("Gone"));
    }

    #[test]
    fn tree_builder_skips_non_rendered_body_subtrees() {
        let tree = tree(
            "<html><body><p>Visible</p><script>Gone()</script><style>.gone { color: red }</style></body></html>",
        );
        assert_eq!(tree.visible_text(), "Visible");
        let dump = dump_layout_tree(&tree);
        assert!(!dump.contains("Gone"));
        assert!(!dump.contains(".gone"));
    }

    #[test]
    fn display_none_from_computed_style_drops_subtree() {
        let doc = doc("<html><body><p id='keep'>Keep</p><p id='drop'>Drop</p></body></html>");
        let tree = build_layout_tree(&doc, (120, 200), |node_id| {
            let element = doc.element_by_node_id(node_id).unwrap();
            if element.id.as_deref() == Some("drop") {
                vec![("display".to_owned(), "none".to_owned())]
            } else {
                Vec::new()
            }
        });
        assert_eq!(tree.visible_text(), "Keep");
        assert!(!dump_layout_tree(&tree).contains("id=drop"));
    }

    #[test]
    fn block_layout_applies_basic_box_model_styles() {
        let doc = doc("<html><body><div id='box'>x</div></body></html>");
        let tree = build_layout_tree(&doc, (120, 200), |node_id| {
            let element = doc.element_by_node_id(node_id).unwrap();
            if element.id.as_deref() == Some("box") {
                vec![
                    ("width".to_owned(), "20px".to_owned()),
                    ("height".to_owned(), "10px".to_owned()),
                    ("margin".to_owned(), "5px 0 7px 10px".to_owned()),
                    ("padding".to_owned(), "2px".to_owned()),
                    ("border".to_owned(), "1px solid black".to_owned()),
                ]
            } else {
                Vec::new()
            }
        });
        let node = node_with_id(&tree, "box");
        assert_eq!(node.rect, Rect::new(18.0, 13.0, 26.0, 16.0));
        assert_eq!(node.boxes.content, Rect::new(21.0, 16.0, 20.0, 10.0));
        assert!(dump_layout_tree(&tree).contains("content=(21.0,16.0,20.0,10.0)"));
    }

    #[test]
    fn line_boxes_are_derived_from_layout_tree_visible_text() {
        let tree = tree("<html><body><p>one two three four</p></body></html>");
        let lines = line_boxes_from_tree(&tree);
        assert!(!lines.is_empty());
        assert!(lines[0].text.contains("one"));
    }

    #[test]
    fn dump_format_is_stable() {
        let tree = tree(r#"<html><body><main id='root'><p>A "quote"</p></main></body></html>"#);
        let dump = dump_layout_tree(&tree);
        assert!(dump.contains("# layout-tree viewport=120x200"));
        assert!(dump.contains("node 0: viewport"));
        assert!(dump.contains("tag=main id=root"));
        assert!(dump.contains("text=\"A \\\"quote\\\"\""));
    }

    #[test]
    fn nested_scroll_moves_descendants_but_not_scrollport_or_fixed_content() {
        let mut tree = tree(
            "<html><body><div id='scroll'><p id='child'>Child</p><div id='fixed'>Fixed</div></div></body></html>",
        );
        let scroll = node_with_id(&tree, "scroll").clone();
        let child = node_with_id(&tree, "child").clone();
        let fixed_id = node_with_id(&tree, "fixed").id;
        tree.node_mut(fixed_id).style.position = LayoutPosition::Fixed;
        let fixed_rect = tree.node(fixed_id).rect;

        apply_element_scrolls(&mut tree, |node_id| {
            (Some(node_id) == scroll.dom_node_id).then_some((7.0, 11.0))
        });

        assert_eq!(node_with_id(&tree, "scroll").rect, scroll.rect);
        assert_eq!(
            node_with_id(&tree, "child").rect,
            Rect::new(
                child.rect.x - 7.0,
                child.rect.y - 11.0,
                child.rect.w,
                child.rect.h,
            )
        );
        assert_eq!(node_with_id(&tree, "fixed").rect, fixed_rect);
    }
}
