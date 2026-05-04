use std::collections::HashMap;

use winio::prelude::Size;

use crate::ui::{LayoutDirection, Rect, WidgetId, WidgetNode};

pub fn compute_layout(nodes: &[WidgetNode], size: Size) -> Vec<(WidgetId, Rect)> {
    let mut by_parent: HashMap<Option<WidgetId>, Vec<WidgetNode>> = HashMap::new();
    let mut by_id: HashMap<WidgetId, WidgetNode> = HashMap::new();
    for n in nodes {
        by_parent.entry(n.parent).or_default().push(*n);
        by_id.insert(n.id, *n);
    }
    for children in by_parent.values_mut() {
        children.sort_by_key(|n| (n.z_order, n.id));
    }

    let mut out = Vec::new();
    if let Some(roots) = by_parent.get(&None) {
        let root_bounds = Rect::new(0.0, 0.0, size.width, size.height);
        for root in roots {
            out.push((root.id, root_bounds));
            layout_node_children(root.id, root_bounds, &by_parent, &by_id, &mut out);
        }
    }
    out
}

fn layout_node_children(
    parent: WidgetId,
    parent_bounds: Rect,
    by_parent: &HashMap<Option<WidgetId>, Vec<WidgetNode>>,
    by_id: &HashMap<WidgetId, WidgetNode>,
    out: &mut Vec<(WidgetId, Rect)>,
) {
    let Some(children) = by_parent.get(&Some(parent)) else {
        return;
    };
    let parent_node = by_id.get(&parent).copied();
    let parent_dir = parent_node.map(|n| n.layout.direction).unwrap_or(LayoutDirection::Column);
    let parent_padding = parent_node.map(|n| n.layout.padding).unwrap_or(0.0).max(0.0);

    let content = Rect::new(
        parent_bounds.x + parent_padding,
        parent_bounds.y + parent_padding,
        (parent_bounds.w - parent_padding * 2.0).max(1.0),
        (parent_bounds.h - parent_padding * 2.0).max(1.0),
    );

    let total_basis: f64 = children
        .iter()
        .map(|n| n.layout.flex_basis.max(1.0) + n.layout.margin.max(0.0) * 2.0)
        .sum();
    let total_grow: f64 = children.iter().map(|n| n.layout.flex_grow.max(0.0)).sum();
    let total_shrink: f64 = children.iter().map(|n| n.layout.flex_shrink.max(0.0)).sum();
    let axis = match parent_dir {
        LayoutDirection::Row => content.w,
        LayoutDirection::Column => content.h,
    };
    let positive_remainder = (axis - total_basis).max(0.0);
    let overflow = (total_basis - axis).max(0.0);

    let mut cursor_x = content.x;
    let mut cursor_y = content.y;
    for child in children {
        let margin = child.layout.margin.max(0.0);
        let grow_share = if total_grow > 0.0 {
            positive_remainder * (child.layout.flex_grow.max(0.0) / total_grow)
        } else {
            0.0
        };
        let shrink_share = if overflow > 0.0 && total_shrink > 0.0 {
            overflow * (child.layout.flex_shrink.max(0.0) / total_shrink)
        } else {
            0.0
        };
        let primary = (child.layout.flex_basis.max(1.0) + grow_share - shrink_share).max(1.0);
        let child_bounds = match parent_dir {
            LayoutDirection::Row => {
                let r = Rect::new(
                    cursor_x + margin,
                    content.y + margin,
                    (primary - margin * 2.0).max(1.0),
                    (content.h - margin * 2.0).max(1.0),
                );
                cursor_x += primary + margin * 2.0;
                r
            }
            LayoutDirection::Column => {
                let r = Rect::new(
                    content.x + margin,
                    cursor_y + margin,
                    (content.w - margin * 2.0).max(1.0),
                    (primary - margin * 2.0).max(1.0),
                );
                cursor_y += primary + margin * 2.0;
                r
            }
        };
        out.push((child.id, child_bounds));
        layout_node_children(child.id, child_bounds, by_parent, by_id, out);
    }
}
