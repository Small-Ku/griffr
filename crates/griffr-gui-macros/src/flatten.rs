use quote::ToTokens;

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
    let kind = node.kind.to_token_stream().to_string().replace(' ', "");
    let defaults = defaults_for_kind(&kind);
    let z = node.props.z.unwrap_or(id as i32);
    let flex_grow = node.props.flex_grow.unwrap_or(0.0);
    let flex_shrink = node.props.flex_shrink.unwrap_or(1.0);
    let flex_basis = node.props.flex_basis.unwrap_or(100.0);
    let (sizing_mode, sizing_f1, sizing_f2, sizing_f3) = match node.props.aspect_ratio {
        Some(aspect_ratio) if aspect_ratio > 0.0 => (1, aspect_ratio, 0.0, 0.0),
        _ => (0, flex_grow, flex_shrink, flex_basis),
    };
    out.push(FlatNode {
        id,
        parent,
        kind,
        hoverable: node.props.hoverable.unwrap_or(defaults.0),
        clickable: node.props.clickable.unwrap_or(defaults.1),
        scrollable: node.props.scrollable.unwrap_or(defaults.2),
        opaque: node.props.opaque.unwrap_or(defaults.3),
        clip: node.props.clip.unwrap_or(if defaults.2 { 1 } else { 0 }),
        z,
        direction: node.props.direction.unwrap_or(1),
        sizing_mode,
        sizing_f1,
        sizing_f2,
        sizing_f3,
        margin: node.props.margin.unwrap_or(0.0),
        padding: node.props.padding.unwrap_or(0.0),
    });
    for child in &node.children {
        flatten(child, id as i16, next_id, out);
    }
}

fn defaults_for_kind(kind: &str) -> (bool, bool, bool, bool) {
    let leaf = kind.rsplit("::").next().unwrap_or(kind).trim();
    match leaf {
        "Button" => (true, true, false, true),
        "Banner" => (true, false, true, true),
        "Container" => (false, false, false, true),
        "GradientContainer" => (false, false, false, true),
        "CounterWidget" => (true, true, false, true),
        _ => (false, false, false, false),
    }
}
