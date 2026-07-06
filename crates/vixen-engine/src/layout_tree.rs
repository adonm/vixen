//! Vixen-owned layout tree spine (ADR-013).
//!
//! This is the first Rust layout stage behind [`crate::page::Page`]. It mirrors
//! Ladybird's `TreeBuilder` seam at `0de15a5dd2a9` in shape, not in C++
//! ownership: styled DOM nodes become stable arena entries (`LayoutNodeId`),
//! hidden/non-rendered subtrees are dropped before layout, and deterministic
//! debug output gives Phase 4 a vertical fixture surface. Formatting contexts
//! will grow from this arena instead of landing as isolated pure modules.

#![forbid(unsafe_code)]

use markup5ever_rcdom::{Handle, NodeData};

use crate::box_model::{
    AutoEdges, BoxModel, BoxModelInput, BoxSizing, Edges, LengthOrAuto, resolve_box_model,
};
use crate::display_list::Rect;
use crate::doc::Document;
use crate::line_layout::{LineBox, LineLayoutConfig, layout_text_lines};

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
    pub margin: AutoEdges,
    pub border: Edges,
    pub padding: Edges,
    pub width: LengthOrAuto,
    pub height: LengthOrAuto,
}

impl Default for LayoutStyle {
    fn default() -> Self {
        Self {
            box_sizing: BoxSizing::ContentBox,
            margin: AutoEdges::px_all(0.0),
            border: Edges::ZERO,
            padding: Edges::ZERO,
            width: LengthOrAuto::Auto,
            height: LengthOrAuto::Auto,
        }
    }
}

impl LayoutStyle {
    fn from_computed(styles: &[(String, String)]) -> Self {
        let mut out = Self::default();

        if style_value(styles, "box-sizing")
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("border-box"))
        {
            out.box_sizing = BoxSizing::BorderBox;
        }
        if let Some(value) = style_value(styles, "width").and_then(parse_auto_length) {
            out.width = value;
        }
        if let Some(value) = style_value(styles, "height").and_then(parse_auto_length) {
            out.height = value;
        }
        if let Some(edges) = style_value(styles, "margin").and_then(parse_auto_edges) {
            out.margin = edges;
        }
        apply_auto_edge(styles, "margin-top", |value| out.margin.top = value);
        apply_auto_edge(styles, "margin-right", |value| out.margin.right = value);
        apply_auto_edge(styles, "margin-bottom", |value| out.margin.bottom = value);
        apply_auto_edge(styles, "margin-left", |value| out.margin.left = value);

        if let Some(edges) = style_value(styles, "padding").and_then(parse_edges) {
            out.padding = edges;
        }
        apply_edge(styles, "padding-top", |value| out.padding.top = value);
        apply_edge(styles, "padding-right", |value| out.padding.right = value);
        apply_edge(styles, "padding-bottom", |value| out.padding.bottom = value);
        apply_edge(styles, "padding-left", |value| out.padding.left = value);

        if let Some(edges) = style_value(styles, "border-width").and_then(parse_border_edges) {
            out.border = edges;
        }
        if let Some(width) = style_value(styles, "border").and_then(parse_border_width) {
            out.border = Edges {
                top: width,
                right: width,
                bottom: width,
                left: width,
            };
        }
        apply_border_edge(styles, "border-top-width", |value| out.border.top = value);
        apply_border_edge(styles, "border-right-width", |value| {
            out.border.right = value
        });
        apply_border_edge(styles, "border-bottom-width", |value| {
            out.border.bottom = value
        });
        apply_border_edge(styles, "border-left-width", |value| out.border.left = value);

        out
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

/// Build the current Vixen layout tree from a parsed document.
pub fn build_layout_tree<F>(
    document: &Document,
    viewport: (u32, u32),
    mut computed_style: F,
) -> LayoutTree
where
    F: FnMut(usize) -> Vec<(String, String)>,
{
    let mut tree = LayoutTree {
        viewport,
        root: LayoutNodeId(0),
        nodes: Vec::new(),
    };
    let root = tree.push(LayoutNode {
        id: LayoutNodeId(0),
        parent: None,
        children: Vec::new(),
        dom_node_id: None,
        tag: None,
        html_id: None,
        kind: LayoutNodeKind::Viewport,
        display: None,
        style: LayoutStyle::default(),
        rect: Rect::new(0.0, 0.0, viewport.0 as f32, viewport.1 as f32),
        boxes: LayoutBoxes::from_rect(Rect::new(0.0, 0.0, viewport.0 as f32, viewport.1 as f32)),
        text: None,
    });
    tree.root = root;

    let mut builder = TreeBuilder {
        tree,
        next_element_node_id: 1,
        computed_style: &mut computed_style,
    };
    builder.collect_children(&document.dom.document, root, false);
    let mut tree = builder.tree;
    layout_tree(&mut tree, LineLayoutConfig::for_viewport(viewport));
    tree
}

/// Derive the stable line-box projection from the layout tree, not directly
/// from raw DOM text. This keeps `Page::dump_lines` on the same Phase 4 spine
/// that later formatting-context slices extend.
pub fn line_boxes_from_tree(tree: &LayoutTree) -> Vec<LineBox> {
    layout_text_lines(
        &tree.visible_text(),
        LineLayoutConfig::for_viewport(tree.viewport),
    )
}

/// Stable human-readable tree dump for `vixen-headless --dump-layout-tree`.
pub fn dump_layout_tree(tree: &LayoutTree) -> String {
    let mut out = format!(
        "# layout-tree viewport={}x{} nodes={}\n",
        tree.viewport.0,
        tree.viewport.1,
        tree.nodes.len()
    );
    dump_node(tree, tree.root, 0, &mut out);
    out
}

struct TreeBuilder<'a, F>
where
    F: FnMut(usize) -> Vec<(String, String)>,
{
    tree: LayoutTree,
    next_element_node_id: usize,
    computed_style: &'a mut F,
}

impl<F> TreeBuilder<'_, F>
where
    F: FnMut(usize) -> Vec<(String, String)>,
{
    fn collect_children(&mut self, node: &Handle, parent: LayoutNodeId, in_body: bool) {
        let children: Vec<Handle> = node.children.borrow().clone();
        for child in children {
            self.collect_node(&child, parent, in_body);
        }
    }

    fn collect_node(&mut self, node: &Handle, parent: LayoutNodeId, in_body: bool) {
        match &node.data {
            NodeData::Element { name, attrs, .. } => {
                let dom_node_id = self.next_element_node_id;
                self.next_element_node_id += 1;

                let tag = name.local.as_ref();
                let now_in_body = in_body || tag == "body";
                if !now_in_body {
                    self.collect_children(node, parent, now_in_body);
                    return;
                }
                if non_rendered_tag(tag) {
                    return;
                }

                let attrs = attrs.borrow();
                if attrs
                    .iter()
                    .any(|attr| attr.name.local.as_ref() == "hidden")
                {
                    return;
                }
                let html_id = attrs
                    .iter()
                    .find(|attr| attr.name.local.as_ref() == "id")
                    .map(|attr| attr.value.to_string());
                drop(attrs);

                let styles = (self.computed_style)(dom_node_id);
                let display = display_for(tag, &styles);
                let Some(display) = display else {
                    return;
                };
                let style = LayoutStyle::from_computed(&styles);
                let kind = match display {
                    LayoutDisplay::Block => LayoutNodeKind::Block,
                    LayoutDisplay::Inline => LayoutNodeKind::Inline,
                };
                let id = self.append_child(
                    parent,
                    LayoutNode {
                        id: LayoutNodeId(usize::MAX),
                        parent: Some(parent),
                        children: Vec::new(),
                        dom_node_id: Some(dom_node_id),
                        tag: Some(tag.to_owned()),
                        html_id,
                        kind,
                        display: Some(display),
                        style,
                        rect: Rect::new(0.0, 0.0, 0.0, 0.0),
                        boxes: LayoutBoxes::from_rect(Rect::new(0.0, 0.0, 0.0, 0.0)),
                        text: None,
                    },
                );
                self.collect_children(node, id, now_in_body);
            }
            NodeData::Text { contents } if in_body => {
                let text = collapse_whitespace(&contents.borrow());
                if !text.is_empty() {
                    self.append_child(
                        parent,
                        LayoutNode {
                            id: LayoutNodeId(usize::MAX),
                            parent: Some(parent),
                            children: Vec::new(),
                            dom_node_id: None,
                            tag: None,
                            html_id: None,
                            kind: LayoutNodeKind::Text,
                            display: Some(LayoutDisplay::Inline),
                            style: LayoutStyle::default(),
                            rect: Rect::new(0.0, 0.0, 0.0, 0.0),
                            boxes: LayoutBoxes::from_rect(Rect::new(0.0, 0.0, 0.0, 0.0)),
                            text: Some(text),
                        },
                    );
                }
            }
            _ => self.collect_children(node, parent, in_body),
        }
    }

    fn append_child(&mut self, parent: LayoutNodeId, node: LayoutNode) -> LayoutNodeId {
        let id = self.tree.push(node);
        self.tree.node_mut(parent).children.push(id);
        id
    }
}

fn layout_tree(tree: &mut LayoutTree, config: LineLayoutConfig) {
    let root = tree.root;
    let viewport = tree.viewport;
    set_node_rect(
        tree,
        root,
        Rect::new(0.0, 0.0, viewport.0 as f32, viewport.1 as f32),
    );
    let content_width = (viewport.0 as f32 - config.margin_px * 2.0).max(1.0);
    let children = tree.node(root).children.clone();
    let mut y = config.margin_px;
    for child in children {
        let h = layout_node(tree, child, config.margin_px, y, content_width, config);
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
    let content_x = border_x + sizing_model.border.left + sizing_model.padding.left;
    let content_y = border_y + sizing_model.border.top + sizing_model.padding.top;

    let children = tree.node(id).children.clone();
    let mut child_y = content_y;
    let mut children_h = 0.0;
    for child in children {
        let child_h = layout_node(
            tree,
            child,
            content_x,
            child_y,
            sizing_model.content_w,
            config,
        );
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
    model.margin_block_size()
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

fn display_for(tag: &str, styles: &[(String, String)]) -> Option<LayoutDisplay> {
    if let Some((_, value)) = styles.iter().find(|(property, _)| property == "display") {
        let value = value.trim().to_ascii_lowercase();
        if value == "none" {
            return None;
        }
        if value.starts_with("inline") {
            return Some(LayoutDisplay::Inline);
        }
        return Some(LayoutDisplay::Block);
    }
    Some(default_display_for(tag))
}

fn default_display_for(tag: &str) -> LayoutDisplay {
    match tag {
        "a" | "abbr" | "b" | "bdi" | "bdo" | "br" | "button" | "cite" | "code" | "em" | "i"
        | "img" | "input" | "label" | "mark" | "small" | "span" | "strong" | "sub" | "sup"
        | "textarea" | "time" => LayoutDisplay::Inline,
        _ => LayoutDisplay::Block,
    }
}

fn non_rendered_tag(tag: &str) -> bool {
    matches!(
        tag,
        "head" | "title" | "meta" | "link" | "style" | "script" | "noscript" | "template"
    )
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn translate_rect(rect: Rect, dx: f32, dy: f32) -> Rect {
    Rect::new(rect.x + dx, rect.y + dy, rect.w, rect.h)
}

fn style_value<'a>(styles: &'a [(String, String)], property: &str) -> Option<&'a str> {
    styles
        .iter()
        .find(|(name, _)| name == property)
        .map(|(_, value)| value.as_str())
}

fn apply_auto_edge<F>(styles: &[(String, String)], property: &str, apply: F)
where
    F: FnOnce(LengthOrAuto),
{
    if let Some(value) = style_value(styles, property).and_then(parse_auto_length) {
        apply(value);
    }
}

fn apply_edge<F>(styles: &[(String, String)], property: &str, apply: F)
where
    F: FnOnce(f32),
{
    if let Some(value) = style_value(styles, property).and_then(parse_non_negative_length) {
        apply(value);
    }
}

fn apply_border_edge<F>(styles: &[(String, String)], property: &str, apply: F)
where
    F: FnOnce(f32),
{
    if let Some(value) = style_value(styles, property).and_then(parse_border_width) {
        apply(value);
    }
}

fn parse_auto_edges(value: &str) -> Option<AutoEdges> {
    let values = parse_box_shorthand(value, parse_auto_length)?;
    Some(AutoEdges {
        top: values[0],
        right: values[1],
        bottom: values[2],
        left: values[3],
    })
}

fn parse_edges(value: &str) -> Option<Edges> {
    let values = parse_box_shorthand(value, parse_non_negative_length)?;
    Some(Edges {
        top: values[0],
        right: values[1],
        bottom: values[2],
        left: values[3],
    })
}

fn parse_border_edges(value: &str) -> Option<Edges> {
    let values = parse_box_shorthand(value, parse_border_width)?;
    Some(Edges {
        top: values[0],
        right: values[1],
        bottom: values[2],
        left: values[3],
    })
}

fn parse_box_shorthand<T, F>(value: &str, parse_one: F) -> Option<[T; 4]>
where
    T: Copy,
    F: FnMut(&str) -> Option<T>,
{
    let parsed: Vec<T> = value
        .split_whitespace()
        .map(parse_one)
        .collect::<Option<_>>()?;
    match parsed.as_slice() {
        [one] => Some([*one, *one, *one, *one]),
        [block, inline] => Some([*block, *inline, *block, *inline]),
        [top, inline, bottom] => Some([*top, *inline, *bottom, *inline]),
        [top, right, bottom, left] => Some([*top, *right, *bottom, *left]),
        _ => None,
    }
}

fn parse_auto_length(value: &str) -> Option<LengthOrAuto> {
    if value.trim().eq_ignore_ascii_case("auto") {
        return Some(LengthOrAuto::Auto);
    }
    parse_length(value).map(LengthOrAuto::Px)
}

fn parse_non_negative_length(value: &str) -> Option<f32> {
    parse_length(value).map(|value| value.max(0.0))
}

fn parse_border_width(value: &str) -> Option<f32> {
    let value = value.trim();
    if let Some(width) = parse_non_negative_length(value) {
        return Some(width);
    }
    for token in value.split_whitespace() {
        if let Some(width) =
            parse_border_width_keyword(token).or_else(|| parse_non_negative_length(token))
        {
            return Some(width);
        }
    }
    parse_border_width_keyword(value)
}

fn parse_border_width_keyword(value: &str) -> Option<f32> {
    match value.trim().to_ascii_lowercase().as_str() {
        "thin" => Some(1.0),
        "medium" => Some(3.0),
        "thick" => Some(5.0),
        _ => None,
    }
}

fn parse_length(value: &str) -> Option<f32> {
    let value = value.trim();
    if value == "0" || value == "+0" || value == "-0" {
        return Some(0.0);
    }
    value
        .strip_suffix("px")
        .or_else(|| value.strip_suffix("PX"))
        .and_then(|number| number.parse::<f32>().ok())
        .filter(|value| value.is_finite())
}

fn dump_node(tree: &LayoutTree, id: LayoutNodeId, depth: usize, out: &mut String) {
    let node = tree.node(id);
    let indent = "  ".repeat(depth);
    let kind = match node.kind {
        LayoutNodeKind::Viewport => "viewport",
        LayoutNodeKind::Block => "block",
        LayoutNodeKind::Inline => "inline",
        LayoutNodeKind::Text => "text",
    };
    out.push_str(&format!(
        "{indent}node {}: {kind}{}{}{} x={:.1} y={:.1} w={:.1} h={:.1} children={}{}{}\n",
        id.index(),
        fmt_dom_node_id(node.dom_node_id),
        fmt_tag(node.tag.as_deref()),
        fmt_html_id(node.html_id.as_deref()),
        node.rect.x,
        node.rect.y,
        node.rect.w,
        node.rect.h,
        node.children.len(),
        fmt_content_box(node),
        fmt_text(node.text.as_deref())
    ));
    for child in &node.children {
        dump_node(tree, *child, depth + 1, out);
    }
}

fn fmt_dom_node_id(node_id: Option<usize>) -> String {
    node_id
        .map(|id| format!(" node_id={id}"))
        .unwrap_or_default()
}

fn fmt_tag(tag: Option<&str>) -> String {
    tag.map(|tag| format!(" tag={tag}")).unwrap_or_default()
}

fn fmt_html_id(id: Option<&str>) -> String {
    id.map(|id| format!(" id={id}")).unwrap_or_default()
}

fn fmt_text(text: Option<&str>) -> String {
    text.map(|text| format!(" text=\"{}\"", escape_dump_text(text)))
        .unwrap_or_default()
}

fn fmt_content_box(node: &LayoutNode) -> String {
    if node.kind != LayoutNodeKind::Block || node.boxes.content == node.rect {
        return String::new();
    }
    format!(
        " content=({:.1},{:.1},{:.1},{:.1})",
        node.boxes.content.x, node.boxes.content.y, node.boxes.content.w, node.boxes.content.h
    )
}

fn escape_dump_text(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
