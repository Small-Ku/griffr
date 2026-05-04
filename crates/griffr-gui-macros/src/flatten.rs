use crate::model::{FlatNode, NodeInput};

pub(crate) fn flatten_tree(root: &NodeInput) -> Vec<FlatNode> {
    let mut flat = Vec::new();
    let mut next_id: u16 = 0;
    flatten(root, -1, &mut next_id, &mut flat);
    flat
}

fn flatten(node: &NodeInput, parent: i16, next_id: &mut u16, out: &mut Vec<FlatNode>) {
    let id = *next_id;
    *next_id += 1;
    let kind = node.kind.to_string();
    let defaults = defaults_for_kind(&kind);
    let z = node.props.z.unwrap_or(id as i32);
    out.push(FlatNode {
        id,
        parent,
        kind,
        hoverable: node.props.hoverable.unwrap_or(defaults.0),
        clickable: node.props.clickable.unwrap_or(defaults.1),
        scrollable: node.props.scrollable.unwrap_or(defaults.2),
        clip: node.props.clip.unwrap_or(if defaults.2 { 1 } else { 0 }),
        z,
        direction: node.props.direction.unwrap_or(1),
        flex_grow: node.props.flex_grow.unwrap_or(0.0),
        flex_shrink: node.props.flex_shrink.unwrap_or(1.0),
        flex_basis: node.props.flex_basis.unwrap_or(100.0),
        margin: node.props.margin.unwrap_or(0.0),
        padding: node.props.padding.unwrap_or(0.0),
    });
    for child in &node.children {
        flatten(child, id as i16, next_id, out);
    }
}

fn defaults_for_kind(kind: &str) -> (bool, bool, bool) {
    match kind {
        "Button" => (true, true, false),
        "Banner" => (true, false, true),
        _ => (false, false, false),
    }
}
