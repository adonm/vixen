use crate::display_list::{Color, Rect};
use crate::line_layout::{LineLayoutConfig, layout_text_lines};

use super::{LayoutBoxes, LayoutNode, LayoutNodeId, LayoutNodeKind, LayoutTree};

/// Positioned output of the layout tree. Paint consumes these fragments instead
/// of re-walking layout nodes directly, keeping the Milestone 2 layout/paint
/// seam explicit while the future formatter swaps in shaped glyph fragments.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutFragment {
    pub order: u32,
    pub node_id: LayoutNodeId,
    pub dom_node_id: Option<usize>,
    pub rect: Rect,
    pub clip: Option<Rect>,
    pub kind: LayoutFragmentKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LayoutFragmentKind {
    Background { color: Color, boxes: LayoutBoxes },
    Image,
    Text { color: Color, text: String },
}

/// Project the laid-out arena into positioned paint fragments.
pub fn layout_fragments_from_tree(tree: &LayoutTree) -> Vec<LayoutFragment> {
    let viewport = Rect::new(0.0, 0.0, tree.viewport.0 as f32, tree.viewport.1 as f32);
    let mut out = Vec::new();

    for node in &tree.nodes {
        if node.id == tree.root {
            continue;
        }
        let Some(clip) = clip_for_node(tree, node, viewport) else {
            continue;
        };
        if node.kind == LayoutNodeKind::Block
            && let Some(color) = node.style.background_color
        {
            out.push(LayoutFragment {
                order: fragment_order(node, 0),
                node_id: node.id,
                dom_node_id: node.dom_node_id,
                rect: node.boxes.border,
                clip: Some(clip),
                kind: LayoutFragmentKind::Background {
                    color,
                    boxes: node.boxes,
                },
            });
        }

        if node.tag.as_deref() == Some("img") && node.dom_node_id.is_some() {
            out.push(LayoutFragment {
                order: fragment_order(node, 1),
                node_id: node.id,
                dom_node_id: node.dom_node_id,
                rect: node.boxes.content,
                clip: Some(clip),
                kind: LayoutFragmentKind::Image,
            });
        }

        if node.kind == LayoutNodeKind::Text
            && let Some(text) = node.text.as_deref()
            && !text.is_empty()
        {
            let color = inherited_text_color(tree, node);
            out.extend(text_fragments(tree, node, text, color, clip));
        }
    }

    out
}

fn text_fragments(
    tree: &LayoutTree,
    node: &LayoutNode,
    text: &str,
    color: Color,
    clip: Rect,
) -> Vec<LayoutFragment> {
    let config = LineLayoutConfig::for_viewport(tree.viewport);
    let available_width = text_available_width(tree, node).max(config.average_char_width_px);
    let lines = layout_text_lines(
        text,
        LineLayoutConfig {
            viewport_width: available_width.round().max(1.0) as u32,
            margin_px: 0.0,
            line_height_px: config.line_height_px,
            average_char_width_px: config.average_char_width_px,
        },
    );

    lines
        .into_iter()
        .enumerate()
        .map(|(idx, line)| LayoutFragment {
            order: fragment_order(node, idx + 1),
            node_id: node.id,
            dom_node_id: node.dom_node_id,
            rect: Rect::new(node.rect.x + line.x, node.rect.y + line.y, line.w, line.h),
            clip: Some(clip),
            kind: LayoutFragmentKind::Text {
                color,
                text: line.text,
            },
        })
        .collect()
}

fn text_available_width(tree: &LayoutTree, node: &LayoutNode) -> f32 {
    containing_content_right(tree, node)
        .map(|right| right - node.rect.x)
        .unwrap_or(node.rect.w)
        .max(1.0)
}

fn containing_content_right(tree: &LayoutTree, node: &LayoutNode) -> Option<f32> {
    let mut parent = node.parent;
    while let Some(id) = parent {
        let parent_node = tree.node(id);
        if parent_node.boxes.content.w > 0.0 {
            return Some(parent_node.boxes.content.x + parent_node.boxes.content.w);
        }
        parent = parent_node.parent;
    }
    None
}

fn clip_for_node(tree: &LayoutTree, node: &LayoutNode, viewport: Rect) -> Option<Rect> {
    let mut clip = viewport;
    let mut parent = node.parent;
    while let Some(id) = parent {
        let ancestor = tree.node(id);
        if ancestor.style.overflow.clips_contents() {
            clip = clip.intersect(ancestor.boxes.padding)?;
        }
        parent = ancestor.parent;
    }
    Some(clip)
}

fn inherited_text_color(tree: &LayoutTree, node: &LayoutNode) -> Color {
    let mut parent = node.parent;
    while let Some(id) = parent {
        let parent_node = tree.node(id);
        if parent_node.dom_node_id.is_some() {
            return parent_node.style.color;
        }
        parent = parent_node.parent;
    }
    Color::BLACK
}

fn fragment_order(node: &LayoutNode, part: usize) -> u32 {
    (node.id.index() as u32)
        .saturating_mul(1024)
        .saturating_add(part as u32)
        .saturating_add(1)
}
