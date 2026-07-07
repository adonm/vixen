use super::{LayoutNode, LayoutNodeId, LayoutNodeKind, LayoutTree};

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
