use quote::ToTokens;

use crate::tree::{FlatNode, NodeInput, Sizing};

pub(crate) fn flatten_tree(root: &NodeInput) -> Vec<FlatNode> {
    let mut flat = Vec::new();
    let mut next_id: u16 = 0;
    flatten(root, -1, &mut next_id, &mut flat);
    flat
}

fn flatten(node: &NodeInput, parent: i16, next_id: &mut u16, out: &mut Vec<FlatNode>) {
    let id = *next_id;
    *next_id += 1;
    let kind = node.kind.to_token_stream().to_string().replace(' ', "");
    let z = node.props.z.unwrap_or(id as i32);
    let sizing = match node.props.aspect_ratio {
        Some(aspect_ratio) if aspect_ratio > 0.0 => Sizing::AspectRatio(aspect_ratio),
        _ => Sizing::Flex {
            grow: node.props.flex_grow,
            shrink: node.props.flex_shrink,
            basis: node.props.flex_basis,
        },
    };
    out.push(FlatNode {
        id,
        parent,
        kind,
        // Widget behavior is read from the runtime Widget implementation after construction.
        hoverable: false,
        clickable: false,
        scrollable: false,
        opaque: false,
        clip: node.props.clip.unwrap_or_default(),
        z,
        direction: node.props.direction.unwrap_or_default(),
        sizing,
        margin: node.props.margin,
        padding: node.props.padding,
    });
    for child in &node.children {
        flatten(child, id as i16, next_id, out);
    }
}
