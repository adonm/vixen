use markup5ever_rcdom::{Handle, NodeData};

use crate::display_list::Rect;
use crate::doc::Document;
use crate::line_layout::LineLayoutConfig;

use super::flow::layout_tree;
use super::style::{collapse_whitespace, display_for, non_rendered_tag, parse_grid_template};
use super::{
    LayoutBoxes, LayoutDisplay, LayoutNode, LayoutNodeId, LayoutNodeKind, LayoutStyle, LayoutTree,
};

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
        grid_template_columns: Vec::new(),
        grid_template_rows: Vec::new(),
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
                    self.skip_element_descendants(node);
                    return;
                }

                let attrs = attrs.borrow();
                if attrs
                    .iter()
                    .any(|attr| attr.name.local.as_ref() == "hidden")
                {
                    drop(attrs);
                    self.skip_element_descendants(node);
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
                    self.skip_element_descendants(node);
                    return;
                };
                let style =
                    LayoutStyle::from_computed(tag, &styles, self.tree.node(parent).style.color);
                let kind = match display {
                    LayoutDisplay::Block | LayoutDisplay::Flex | LayoutDisplay::Grid => {
                        LayoutNodeKind::Block
                    }
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
                        grid_template_columns: parse_grid_template(
                            &styles,
                            "grid-template-columns",
                        ),
                        grid_template_rows: parse_grid_template(&styles, "grid-template-rows"),
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
                            grid_template_columns: Vec::new(),
                            grid_template_rows: Vec::new(),
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

    fn skip_element_descendants(&mut self, node: &Handle) {
        let children: Vec<Handle> = node.children.borrow().clone();
        for child in children {
            if matches!(child.data, NodeData::Element { .. }) {
                self.next_element_node_id += 1;
            }
            self.skip_element_descendants(&child);
        }
    }
}
