use crate::box_model::{
    AutoEdges, BoxModel, BoxModelInput, Edges, LengthOrAuto, resolve_box_model,
};
use crate::display_list::Rect;
use crate::flex_resolve::{FlexDirection, FlexItem, resolve_main_axis};
use crate::grid_resolve::{GridTrack, resolve_tracks};
use crate::line_layout::{LineBox, LineLayoutConfig, layout_text_lines};

use super::{
    LayoutBoxes, LayoutDisplay, LayoutNode, LayoutNodeId, LayoutNodeKind, LayoutPosition,
    LayoutStyle, LayoutTree, translate_rect,
};

/// Derive the stable line-box projection from the layout tree, not directly
/// from raw DOM text. This keeps `Page::dump_lines` on the same Phase 4 spine
/// that later formatting-context slices extend.
pub fn line_boxes_from_tree(tree: &LayoutTree) -> Vec<LineBox> {
    layout_text_lines(
        &tree.visible_text(),
        LineLayoutConfig::for_viewport(tree.viewport),
    )
}

pub(super) fn layout_tree(tree: &mut LayoutTree, config: LineLayoutConfig) {
    let root = tree.root;
    let viewport = tree.viewport;
    set_node_rect(
        tree,
        root,
        Rect::new(0.0, 0.0, viewport.0 as f32, viewport.1 as f32),
    );
    let content_width = viewport.0 as f32;
    let children = tree.node(root).children.clone();
    let mut y = 0.0;
    for child in children {
        let h = layout_node(tree, child, 0.0, y, content_width, config);
        y += h;
    }
}

fn layout_node(
    tree: &mut LayoutTree,
    id: LayoutNodeId,
    x: f32,
    y: f32,
    available_width: f32,
    config: LineLayoutConfig,
) -> f32 {
    match tree.node(id).kind {
        LayoutNodeKind::Viewport => 0.0,
        LayoutNodeKind::Text => {
            let text = tree.node(id).text.clone().unwrap_or_default();
            let (w, h) = text_extent(&text, available_width, config);
            set_node_rect(tree, id, Rect::new(x, y, w, h));
            h
        }
        LayoutNodeKind::Inline => {
            let text = flatten_text(tree, id);
            let (w, text_h) = text_extent(&text, available_width, config);
            let children = tree.node(id).children.clone();
            let mut child_y = y;
            let mut children_h = 0.0;
            for child in children {
                let h = layout_node(tree, child, x, child_y, available_width, config);
                child_y += h;
                children_h += h;
            }
            let h = text_h.max(children_h);
            set_node_rect(tree, id, Rect::new(x, y, w.max(0.0), h));
            h
        }
        LayoutNodeKind::Block if tree.node(id).display == Some(LayoutDisplay::Flex) => {
            layout_flex_node(tree, id, x, y, available_width, config)
        }
        LayoutNodeKind::Block if tree.node(id).display == Some(LayoutDisplay::Grid) => {
            layout_grid_node(tree, id, x, y, available_width, config)
        }
        LayoutNodeKind::Block => layout_block_node(tree, id, x, y, available_width, config),
    }
}

fn layout_block_node(
    tree: &mut LayoutTree,
    id: LayoutNodeId,
    x: f32,
    y: f32,
    available_width: f32,
    config: LineLayoutConfig,
) -> f32 {
    let style = tree.node(id).style;
    let sizing_model = resolve_layout_box(style, available_width, LengthOrAuto::Px(0.0));
    let border_x = x + sizing_model.margin.left;
    let border_y = y + sizing_model.margin.top;
    let padding_containing_block = Rect::new(
        border_x + sizing_model.border.left,
        border_y + sizing_model.border.top,
        (sizing_model.padding.inline_sum() + sizing_model.content_w).max(0.0),
        0.0,
    );
    let content_x = border_x + sizing_model.border.left + sizing_model.padding.left;
    let content_y = border_y + sizing_model.border.top + sizing_model.padding.top;

    let children = tree.node(id).children.clone();
    let mut child_y = content_y;
    let mut children_h = 0.0;
    let mut idx = 0;
    while idx < children.len() {
        let child = children[idx];
        let child_h = if tree.node(child).style.position.is_out_of_flow() {
            idx += 1;
            layout_out_of_flow_node(tree, child, padding_containing_block, config);
            0.0
        } else if is_inline_flow_child(tree, child) {
            let start = idx;
            idx += 1;
            while idx < children.len()
                && !tree.node(children[idx]).style.position.is_out_of_flow()
                && is_inline_flow_child(tree, children[idx])
            {
                idx += 1;
            }
            layout_inline_sequence(
                tree,
                &children[start..idx],
                content_x,
                child_y,
                sizing_model.content_w,
                config,
            )
        } else {
            idx += 1;
            layout_node(
                tree,
                child,
                content_x,
                child_y,
                sizing_model.content_w,
                config,
            )
        };
        child_y += child_h;
        children_h += child_h;
    }
    let fallback_h = empty_block_height(tree.node(id).tag.as_deref(), config);
    let auto_height = children_h.max(fallback_h);
    let resolved_height = if style.height.is_auto() {
        LengthOrAuto::Px(auto_height)
    } else {
        style.height
    };
    let model = resolve_layout_box(style, available_width, resolved_height);
    let border_x = x + model.margin.left;
    let border_y = y + model.margin.top;
    let boxes = LayoutBoxes::from_box_model(model, border_x, border_y);
    let node = tree.node_mut(id);
    node.rect = boxes.border;
    node.boxes = boxes;
    let flow_h = model.margin_block_size();
    let (dx, dy) = relative_offset(style);
    if dx != 0.0 || dy != 0.0 {
        translate_subtree(tree, id, dx, dy);
    }
    flow_h
}

fn layout_flex_node(
    tree: &mut LayoutTree,
    id: LayoutNodeId,
    x: f32,
    y: f32,
    available_width: f32,
    config: LineLayoutConfig,
) -> f32 {
    let style = tree.node(id).style;
    let sizing_height = if style.flex_direction.is_row() {
        LengthOrAuto::Px(0.0)
    } else {
        style.height
    };
    let sizing_model = resolve_layout_box(style, available_width, sizing_height);
    let border_x = x + sizing_model.margin.left;
    let border_y = y + sizing_model.margin.top;
    let content_x = border_x + sizing_model.border.left + sizing_model.padding.left;
    let content_y = border_y + sizing_model.border.top + sizing_model.padding.top;
    let padding_containing_block = Rect::new(
        border_x + sizing_model.border.left,
        border_y + sizing_model.border.top,
        (sizing_model.padding.inline_sum() + sizing_model.content_w).max(0.0),
        0.0,
    );

    let children = tree.node(id).children.clone();
    let mut flex_children = Vec::new();
    for child in &children {
        if tree.node(*child).style.position.is_out_of_flow() {
            layout_out_of_flow_node(tree, *child, padding_containing_block, config);
        } else {
            flex_children.push(*child);
        }
    }

    let is_row = style.flex_direction.is_row();
    let gap = if is_row {
        style.column_gap.max(0.0)
    } else {
        style.row_gap.max(0.0)
    };
    let total_gap = gap * flex_children.len().saturating_sub(1) as f32;
    let auto_column_main: f32 = flex_children
        .iter()
        .map(|child| flex_outer_basis(tree, *child, config, false))
        .sum::<f32>()
        + total_gap;
    let container_main = if is_row {
        sizing_model.content_w
    } else if sizing_model.content_h > 0.0 {
        sizing_model.content_h
    } else {
        auto_column_main
    };
    let inner_main = (container_main - total_gap).max(0.0);
    let flex_items: Vec<FlexItem> = flex_children
        .iter()
        .map(|child| flex_item_for_node(tree, *child, config, is_row))
        .collect();
    let resolution = resolve_main_axis(inner_main, &flex_items);

    let auto_height = if is_row {
        let mut cursor_x = if style.flex_direction == FlexDirection::RowReverse {
            content_x + sizing_model.content_w
        } else {
            content_x
        };
        let mut cross_h: f32 = 0.0;

        for (idx, child) in flex_children.iter().enumerate() {
            let used_outer_w = resolution.sizes.get(idx).copied().unwrap_or_default();
            let item_x = if style.flex_direction == FlexDirection::RowReverse {
                cursor_x -= used_outer_w;
                let item_x = cursor_x;
                cursor_x -= gap;
                item_x
            } else {
                let item_x = cursor_x;
                cursor_x += used_outer_w + gap;
                item_x
            };
            let child_h = layout_flex_item(tree, *child, item_x, content_y, used_outer_w, config);
            cross_h = cross_h.max(child_h);
        }
        cross_h
    } else {
        let mut cursor_y = if style.flex_direction == FlexDirection::ColumnReverse {
            content_y + container_main
        } else {
            content_y
        };
        for (idx, child) in flex_children.iter().enumerate() {
            let used_outer_h = resolution.sizes.get(idx).copied().unwrap_or_default();
            let item_y = if style.flex_direction == FlexDirection::ColumnReverse {
                cursor_y -= used_outer_h;
                let item_y = cursor_y;
                cursor_y -= gap;
                item_y
            } else {
                let item_y = cursor_y;
                cursor_y += used_outer_h + gap;
                item_y
            };
            layout_sized_item(
                tree,
                *child,
                content_x,
                item_y,
                sizing_model.content_w,
                used_outer_h,
                config,
            );
        }
        container_main
    };
    let resolved_height = if style.height.is_auto() {
        LengthOrAuto::Px(auto_height)
    } else {
        style.height
    };
    let model = resolve_layout_box(style, available_width, resolved_height);
    let border_x = x + model.margin.left;
    let border_y = y + model.margin.top;
    let boxes = LayoutBoxes::from_box_model(model, border_x, border_y);
    let node = tree.node_mut(id);
    node.rect = boxes.border;
    node.boxes = boxes;
    let flow_h = model.margin_block_size();
    let (dx, dy) = relative_offset(style);
    if dx != 0.0 || dy != 0.0 {
        translate_subtree(tree, id, dx, dy);
    }
    flow_h
}

fn flex_item_for_node(
    tree: &LayoutTree,
    id: LayoutNodeId,
    config: LineLayoutConfig,
    is_row: bool,
) -> FlexItem {
    let style = tree.node(id).style;
    FlexItem {
        flex_basis: flex_outer_basis(tree, id, config, is_row),
        grow: style.flex_grow.max(0.0),
        shrink: style.flex_shrink.max(0.0),
        min: None,
        max: None,
    }
}

fn flex_outer_basis(
    tree: &LayoutTree,
    id: LayoutNodeId,
    config: LineLayoutConfig,
    is_row: bool,
) -> f32 {
    let node = tree.node(id);
    let style = node.style;
    let margin = inline_margin_edges(style.margin);
    if is_row {
        let content_w = style
            .flex_basis
            .px()
            .or_else(|| style.width.px())
            .unwrap_or_else(|| match node.kind {
                LayoutNodeKind::Text => {
                    inline_text_extent(node.text.as_deref().unwrap_or_default(), config).0
                }
                LayoutNodeKind::Inline => {
                    inline_children_width(tree, node.children.as_slice(), config)
                }
                LayoutNodeKind::Viewport | LayoutNodeKind::Block => 0.0,
            });
        margin.inline_sum()
            + style.border.inline_sum()
            + style.padding.inline_sum()
            + content_w.max(0.0)
    } else {
        let content_h = style
            .flex_basis
            .px()
            .or_else(|| style.height.px())
            .unwrap_or_else(|| match node.kind {
                LayoutNodeKind::Text => {
                    inline_text_extent(node.text.as_deref().unwrap_or_default(), config).1
                }
                LayoutNodeKind::Inline => config.line_height_px,
                LayoutNodeKind::Viewport | LayoutNodeKind::Block => 0.0,
            });
        margin.block_sum()
            + style.border.block_sum()
            + style.padding.block_sum()
            + content_h.max(0.0)
    }
}

fn layout_flex_item(
    tree: &mut LayoutTree,
    id: LayoutNodeId,
    x: f32,
    y: f32,
    used_outer_w: f32,
    config: LineLayoutConfig,
) -> f32 {
    let original_style = tree.node(id).style;
    let used_content_w = flex_used_content_width(original_style, used_outer_w);
    let mut used_style = original_style;
    used_style.width = LengthOrAuto::Px(used_content_w);
    tree.node_mut(id).style = used_style;
    let height = layout_node(tree, id, x, y, used_outer_w.max(0.0), config);
    tree.node_mut(id).style = original_style;
    height
}

fn flex_used_content_width(style: LayoutStyle, used_outer_w: f32) -> f32 {
    let margin = inline_margin_edges(style.margin);
    (used_outer_w - margin.inline_sum() - style.border.inline_sum() - style.padding.inline_sum())
        .max(0.0)
}

fn layout_grid_node(
    tree: &mut LayoutTree,
    id: LayoutNodeId,
    x: f32,
    y: f32,
    available_width: f32,
    config: LineLayoutConfig,
) -> f32 {
    let style = tree.node(id).style;
    let sizing_model = resolve_layout_box(style, available_width, style.height);
    let border_x = x + sizing_model.margin.left;
    let border_y = y + sizing_model.margin.top;
    let content_x = border_x + sizing_model.border.left + sizing_model.padding.left;
    let content_y = border_y + sizing_model.border.top + sizing_model.padding.top;
    let padding_containing_block = Rect::new(
        border_x + sizing_model.border.left,
        border_y + sizing_model.border.top,
        (sizing_model.padding.inline_sum() + sizing_model.content_w).max(0.0),
        (sizing_model.padding.block_sum() + sizing_model.content_h).max(0.0),
    );

    let children = tree.node(id).children.clone();
    let mut grid_children = Vec::new();
    for child in &children {
        if tree.node(*child).style.position.is_out_of_flow() {
            layout_out_of_flow_node(tree, *child, padding_containing_block, config);
        } else {
            grid_children.push(*child);
        }
    }

    let columns = grid_tracks_or_default(tree.node(id).grid_template_columns.as_slice());
    let rows = grid_tracks_or_default(tree.node(id).grid_template_rows.as_slice());
    let column_gap = style.column_gap.max(0.0);
    let row_gap = style.row_gap.max(0.0);
    let column_sizes = resolve_grid_axis(sizing_model.content_w, &columns, column_gap);
    let row_container = if sizing_model.content_h > 0.0 {
        sizing_model.content_h
    } else {
        grid_base_sum(&rows) + row_gap * rows.len().saturating_sub(1) as f32
    };
    let row_sizes = resolve_grid_axis(row_container, &rows, row_gap);

    for (idx, child) in grid_children.iter().enumerate() {
        let column = idx % column_sizes.len();
        let row = idx / column_sizes.len();
        if row >= row_sizes.len() {
            break;
        }
        let item_x = content_x + axis_offset(&column_sizes, column_gap, column);
        let item_y = content_y + axis_offset(&row_sizes, row_gap, row);
        layout_sized_item(
            tree,
            *child,
            item_x,
            item_y,
            column_sizes[column],
            row_sizes[row],
            config,
        );
    }

    let auto_height = grid_axis_outer_size(&row_sizes, row_gap);
    let resolved_height = if style.height.is_auto() {
        LengthOrAuto::Px(auto_height)
    } else {
        style.height
    };
    let model = resolve_layout_box(style, available_width, resolved_height);
    let border_x = x + model.margin.left;
    let border_y = y + model.margin.top;
    let boxes = LayoutBoxes::from_box_model(model, border_x, border_y);
    let node = tree.node_mut(id);
    node.rect = boxes.border;
    node.boxes = boxes;
    let flow_h = model.margin_block_size();
    let (dx, dy) = relative_offset(style);
    if dx != 0.0 || dy != 0.0 {
        translate_subtree(tree, id, dx, dy);
    }
    flow_h
}

fn grid_tracks_or_default(tracks: &[GridTrack]) -> Vec<GridTrack> {
    if tracks.is_empty() {
        vec![GridTrack::fr(1.0)]
    } else {
        tracks.to_vec()
    }
}

fn resolve_grid_axis(container_size: f32, tracks: &[GridTrack], gap: f32) -> Vec<f32> {
    let total_gap = gap * tracks.len().saturating_sub(1) as f32;
    resolve_tracks((container_size - total_gap).max(0.0), tracks).sizes
}

fn grid_base_sum(tracks: &[GridTrack]) -> f32 {
    tracks.iter().map(|track| track.base).sum()
}

fn grid_axis_outer_size(sizes: &[f32], gap: f32) -> f32 {
    sizes.iter().sum::<f32>() + gap * sizes.len().saturating_sub(1) as f32
}

fn axis_offset(sizes: &[f32], gap: f32, index: usize) -> f32 {
    sizes.iter().take(index).sum::<f32>() + gap * index as f32
}

fn layout_sized_item(
    tree: &mut LayoutTree,
    id: LayoutNodeId,
    x: f32,
    y: f32,
    used_outer_w: f32,
    used_outer_h: f32,
    config: LineLayoutConfig,
) -> f32 {
    let original_style = tree.node(id).style;
    let mut used_style = original_style;
    used_style.width = LengthOrAuto::Px(flex_used_content_width(original_style, used_outer_w));
    used_style.height = LengthOrAuto::Px(used_content_height(original_style, used_outer_h));
    tree.node_mut(id).style = used_style;
    let height = layout_node(tree, id, x, y, used_outer_w.max(0.0), config);
    tree.node_mut(id).style = original_style;
    height
}

fn used_content_height(style: LayoutStyle, used_outer_h: f32) -> f32 {
    let margin = inline_margin_edges(style.margin);
    (used_outer_h - margin.block_sum() - style.border.block_sum() - style.padding.block_sum())
        .max(0.0)
}

fn is_inline_flow_child(tree: &LayoutTree, id: LayoutNodeId) -> bool {
    matches!(
        tree.node(id).kind,
        LayoutNodeKind::Text | LayoutNodeKind::Inline
    )
}

fn layout_inline_sequence(
    tree: &mut LayoutTree,
    children: &[LayoutNodeId],
    x: f32,
    y: f32,
    available_width: f32,
    config: LineLayoutConfig,
) -> f32 {
    let line_right = x + available_width.max(1.0);
    let mut cursor_x = x;
    let mut line_y = y;
    let mut line_h: f32 = 0.0;
    let mut max_bottom = y;

    for child in children {
        if tree.node(*child).kind == LayoutNodeKind::Text {
            let text = tree.node(*child).text.clone().unwrap_or_default();
            let unwrapped_w = inline_text_extent(&text, config).0;
            if cursor_x > x && unwrapped_w > line_right - cursor_x {
                line_y = max_bottom;
                cursor_x = x;
                line_h = 0.0;
            }
            let placement = place_wrapped_text_item(
                tree,
                *child,
                cursor_x,
                line_y,
                line_right - cursor_x,
                config,
            );
            if placement.line_count > 1 {
                line_y += (placement.line_count - 1) as f32 * config.line_height_px;
                cursor_x = x + placement.last_line_width;
            } else {
                cursor_x += placement.last_line_width;
            }
            line_h = line_h.max(placement.last_line_height);
            max_bottom = max_bottom.max(line_y + line_h);
            continue;
        }

        let (item_w, item_h) = inline_outer_size(tree, *child, config);
        if cursor_x > x && item_w > 0.0 && cursor_x + item_w > line_right {
            let advance = line_h.max(config.line_height_px);
            line_y += advance;
            cursor_x = x;
            line_h = 0.0;
        }
        place_inline_item(tree, *child, cursor_x, line_y, config);
        cursor_x += item_w;
        line_h = line_h.max(item_h);
        max_bottom = max_bottom.max(line_y + line_h);
    }

    (max_bottom - y).max(config.line_height_px)
}

#[derive(Debug, Clone, Copy)]
struct TextPlacement {
    line_count: usize,
    last_line_width: f32,
    last_line_height: f32,
}

fn place_wrapped_text_item(
    tree: &mut LayoutTree,
    id: LayoutNodeId,
    x: f32,
    y: f32,
    available_width: f32,
    config: LineLayoutConfig,
) -> TextPlacement {
    let text = tree.node(id).text.clone().unwrap_or_default();
    let lines = layout_text_lines(
        &text,
        LineLayoutConfig {
            viewport_width: available_width.round().max(1.0) as u32,
            margin_px: 0.0,
            line_height_px: config.line_height_px,
            average_char_width_px: config.average_char_width_px,
        },
    );

    let line_count = lines.len().max(1);
    let last_line_width = lines.last().map(|line| line.w).unwrap_or(0.0);
    let width = lines.iter().map(|line| line.w).fold(0.0, f32::max);
    let height = line_count as f32 * config.line_height_px;
    set_node_rect(tree, id, Rect::new(x, y, width, height));

    TextPlacement {
        line_count,
        last_line_width,
        last_line_height: config.line_height_px,
    }
}

fn inline_outer_size(tree: &LayoutTree, id: LayoutNodeId, config: LineLayoutConfig) -> (f32, f32) {
    match tree.node(id).kind {
        LayoutNodeKind::Viewport | LayoutNodeKind::Block => (0.0, 0.0),
        LayoutNodeKind::Text => {
            let text = tree.node(id).text.as_deref().unwrap_or_default();
            inline_text_extent(text, config)
        }
        LayoutNodeKind::Inline => {
            let content_w = inline_children_width(tree, tree.node(id).children.as_slice(), config);
            let content_h = if content_w > 0.0 {
                config.line_height_px
            } else {
                0.0
            };
            let model = resolve_inline_box(tree.node(id).style, content_w, content_h);
            (model.margin_inline_size(), model.margin_block_size())
        }
    }
}

fn inline_children_width(
    tree: &LayoutTree,
    children: &[LayoutNodeId],
    config: LineLayoutConfig,
) -> f32 {
    children
        .iter()
        .map(|child| inline_outer_size(tree, *child, config).0)
        .sum()
}

fn place_inline_item(
    tree: &mut LayoutTree,
    id: LayoutNodeId,
    outer_x: f32,
    y: f32,
    config: LineLayoutConfig,
) {
    match tree.node(id).kind {
        LayoutNodeKind::Viewport | LayoutNodeKind::Block => {}
        LayoutNodeKind::Text => {
            let text = tree.node(id).text.clone().unwrap_or_default();
            let (w, h) = inline_text_extent(&text, config);
            set_node_rect(tree, id, Rect::new(outer_x, y, w, h));
        }
        LayoutNodeKind::Inline => {
            let content_w = inline_children_width(tree, tree.node(id).children.as_slice(), config);
            let content_h = if content_w > 0.0 {
                config.line_height_px
            } else {
                0.0
            };
            let model = resolve_inline_box(tree.node(id).style, content_w, content_h);
            let border_x = outer_x + model.margin.left;
            let boxes = LayoutBoxes::from_box_model(model, border_x, y + model.margin.top);
            let content_x = boxes.content.x;
            let content_y = boxes.content.y;
            let children = tree.node(id).children.clone();
            let node = tree.node_mut(id);
            node.rect = boxes.border;
            node.boxes = boxes;

            let mut child_x = content_x;
            for child in children {
                let (child_w, _) = inline_outer_size(tree, child, config);
                place_inline_item(tree, child, child_x, content_y, config);
                child_x += child_w;
            }

            let (dx, dy) = relative_offset(tree.node(id).style);
            if dx != 0.0 || dy != 0.0 {
                translate_subtree(tree, id, dx, dy);
            }
        }
    }
}

fn layout_out_of_flow_node(
    tree: &mut LayoutTree,
    id: LayoutNodeId,
    containing_block: Rect,
    config: LineLayoutConfig,
) {
    let style = tree.node(id).style;
    let x = containing_block.x + style.inset.left.unwrap_or(0.0);
    let y = containing_block.y + style.inset.top.unwrap_or(0.0);
    layout_node(tree, id, x, y, containing_block.w.max(1.0), config);
}

fn relative_offset(style: LayoutStyle) -> (f32, f32) {
    if style.position != LayoutPosition::Relative {
        return (0.0, 0.0);
    }
    let dx = style
        .inset
        .left
        .or_else(|| style.inset.right.map(|right| -right))
        .unwrap_or(0.0);
    let dy = style
        .inset
        .top
        .or_else(|| style.inset.bottom.map(|bottom| -bottom))
        .unwrap_or(0.0);
    (dx, dy)
}

fn translate_subtree(tree: &mut LayoutTree, id: LayoutNodeId, dx: f32, dy: f32) {
    translate_node_boxes(tree.node_mut(id), dx, dy);
    let children = tree.node(id).children.clone();
    for child in children {
        translate_subtree(tree, child, dx, dy);
    }
}

fn translate_node_boxes(node: &mut LayoutNode, dx: f32, dy: f32) {
    node.rect = translate_rect(node.rect, dx, dy);
    node.boxes = LayoutBoxes {
        margin: translate_rect(node.boxes.margin, dx, dy),
        border: translate_rect(node.boxes.border, dx, dy),
        padding: translate_rect(node.boxes.padding, dx, dy),
        content: translate_rect(node.boxes.content, dx, dy),
    };
}

fn inline_text_extent(text: &str, config: LineLayoutConfig) -> (f32, f32) {
    if text.is_empty() {
        return (0.0, 0.0);
    }
    (
        text.chars().count() as f32 * config.average_char_width_px,
        config.line_height_px,
    )
}

fn resolve_inline_box(style: LayoutStyle, content_w: f32, content_h: f32) -> BoxModel {
    BoxModel {
        margin: inline_margin_edges(style.margin),
        border: style.border,
        padding: style.padding,
        content_w: content_w.max(0.0),
        content_h: content_h.max(0.0),
    }
}

fn inline_margin_edges(margin: AutoEdges) -> Edges {
    Edges {
        top: definite_or_zero(margin.top),
        right: definite_or_zero(margin.right),
        bottom: definite_or_zero(margin.bottom),
        left: definite_or_zero(margin.left),
    }
}

fn definite_or_zero(value: LengthOrAuto) -> f32 {
    match value {
        LengthOrAuto::Px(value) => value,
        LengthOrAuto::Auto => 0.0,
    }
}

fn resolve_layout_box(style: LayoutStyle, available_width: f32, height: LengthOrAuto) -> BoxModel {
    resolve_box_model(&BoxModelInput {
        box_sizing: style.box_sizing,
        containing_inline: available_width.max(0.0),
        margin: style.margin,
        border: style.border,
        padding: style.padding,
        width: style.width,
        height,
    })
}

fn set_node_rect(tree: &mut LayoutTree, id: LayoutNodeId, rect: Rect) {
    let node = tree.node_mut(id);
    node.rect = rect;
    node.boxes = LayoutBoxes::from_rect(rect);
}

fn text_extent(text: &str, available_width: f32, config: LineLayoutConfig) -> (f32, f32) {
    if text.is_empty() {
        return (0.0, 0.0);
    }
    let width = available_width.max(1.0).round() as u32;
    let lines = layout_text_lines(
        text,
        LineLayoutConfig {
            viewport_width: width,
            margin_px: 0.0,
            line_height_px: config.line_height_px,
            average_char_width_px: config.average_char_width_px,
        },
    );
    let max_w = lines.iter().map(|line| line.w).fold(0.0, f32::max);
    (max_w, lines.len() as f32 * config.line_height_px)
}

fn flatten_text(tree: &LayoutTree, id: LayoutNodeId) -> String {
    let node = tree.node(id);
    if node.kind == LayoutNodeKind::Text {
        return node.text.clone().unwrap_or_default();
    }
    let mut parts = Vec::new();
    for child in &node.children {
        let text = flatten_text(tree, *child);
        if !text.is_empty() {
            parts.push(text);
        }
    }
    parts.join(" ")
}

fn empty_block_height(tag: Option<&str>, config: LineLayoutConfig) -> f32 {
    match tag {
        Some("br") => config.line_height_px,
        _ => 0.0,
    }
}
