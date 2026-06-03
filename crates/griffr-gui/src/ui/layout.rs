use std::collections::HashMap;

use winio::primitive::{Point, Rect, Size};

use crate::ui::{LayoutDirection, SizingPolicy, WidgetId, WidgetNode};

pub fn compute_layout(nodes: &[WidgetNode], size: Size) -> Vec<(WidgetId, Rect)> {
    let mut by_parent: HashMap<Option<WidgetId>, Vec<WidgetNode>> = HashMap::new();
    let mut by_id: HashMap<WidgetId, WidgetNode> = HashMap::new();
    for n in nodes {
        by_parent.entry(n.parent).or_default().push(n.clone());
        by_id.insert(n.id, n.clone());
    }
    for children in by_parent.values_mut() {
        children.sort_by_key(|n| (n.z_order, n.id));
    }

    let mut out = Vec::new();
    if let Some(roots) = by_parent.get(&None) {
        let root_bounds = Rect::from_size(size);
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
    let parent_node = by_id.get(&parent).cloned();
    let parent_dir = parent_node
        .as_ref()
        .map(|n| n.layout.direction)
        .unwrap_or(LayoutDirection::Column);
    let parent_padding = parent_node
        .as_ref()
        .map(|n| n.layout.padding)
        .unwrap_or(0.0)
        .max(0.0);

    let content = Rect::new(
        Point::new(
            parent_bounds.origin.x + parent_padding,
            parent_bounds.origin.y + parent_padding,
        ),
        Size::new(
            (parent_bounds.size.width - parent_padding * 2.0).max(1.0),
            (parent_bounds.size.height - parent_padding * 2.0).max(1.0),
        ),
    );

    let total_basis: f64 = children
        .iter()
        .map(|n| flex_components(&n.layout.sizing).2.max(1.0) + n.layout.margin.max(0.0) * 2.0)
        .sum();
    let total_grow: f64 = children
        .iter()
        .map(|n| flex_components(&n.layout.sizing).0.max(0.0))
        .sum();
    let total_shrink: f64 = children
        .iter()
        .map(|n| flex_components(&n.layout.sizing).1.max(0.0))
        .sum();
    let axis = match parent_dir {
        LayoutDirection::Row => content.size.width,
        LayoutDirection::Column => content.size.height,
    };
    let positive_remainder = (axis - total_basis).max(0.0);
    let overflow = (total_basis - axis).max(0.0);

    let mut cursor_x = content.origin.x;
    let mut cursor_y = content.origin.y;
    for child in children {
        let margin = child.layout.margin.max(0.0);
        let (flex_grow, flex_shrink, flex_basis) = flex_components(&child.layout.sizing);
        let grow_share = if total_grow > 0.0 {
            positive_remainder * (flex_grow.max(0.0) / total_grow)
        } else {
            0.0
        };
        let shrink_share = if overflow > 0.0 && total_shrink > 0.0 {
            overflow * (flex_shrink.max(0.0) / total_shrink)
        } else {
            0.0
        };
        let primary = (flex_basis.max(1.0) + grow_share - shrink_share).max(1.0);
        let mut child_bounds = match parent_dir {
            LayoutDirection::Row => Rect::new(
                Point::new(cursor_x + margin, content.origin.y + margin),
                Size::new(
                    primary.max(1.0),
                    (content.size.height - margin * 2.0).max(1.0),
                ),
            ),
            LayoutDirection::Column => Rect::new(
                Point::new(content.origin.x + margin, cursor_y + margin),
                Size::new(
                    (content.size.width - margin * 2.0).max(1.0),
                    primary.max(1.0),
                ),
            ),
        };

        match child.layout.sizing {
            SizingPolicy::Flex { .. } => {}
            SizingPolicy::AspectRatio(ratio) => {
                if ratio > 0.0 {
                    match parent_dir {
                        LayoutDirection::Row => {
                            child_bounds.size.height = (child_bounds.size.width / ratio)
                                .min(content.size.height - margin * 2.0);
                        }
                        LayoutDirection::Column => {
                            child_bounds.size.height = child_bounds.size.width / ratio;
                        }
                    }
                }
            }
            SizingPolicy::Fixed(size) => {
                child_bounds.size = size;
            }
        }

        match parent_dir {
            LayoutDirection::Row => {
                cursor_x += child_bounds.size.width + margin * 2.0;
            }
            LayoutDirection::Column => {
                cursor_y += child_bounds.size.height + margin * 2.0;
            }
        }

        out.push((child.id, child_bounds));
        layout_node_children(child.id, child_bounds, by_parent, by_id, out);
    }
}

fn flex_components(sizing: &SizingPolicy) -> (f64, f64, f64) {
    match sizing {
        SizingPolicy::Flex {
            grow,
            shrink,
            basis,
        } => (*grow, *shrink, *basis),
        _ => (0.0, 1.0, 100.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{LayoutSpec, WidgetId, WidgetNode};
    use winio::primitive::Size;

    #[test]
    fn test_margin_padding_gaps() {
        let nodes = vec![
            WidgetNode {
                id: WidgetId(0),
                parent: None,
                hoverable: false,
                clickable: false,
                scrollable: false,
                opaque: true,
                clip: crate::ui::ClipPolicy::InferFromCapabilities,
                layout: LayoutSpec {
                    direction: LayoutDirection::Column,
                    margin: 0.0,
                    padding: 10.0,
                    sizing: SizingPolicy::Flex {
                        grow: 1.0,
                        shrink: 1.0,
                        basis: 600.0,
                    },
                },
                z_order: 0,
                widget_type: "GradientContainer",
            },
            WidgetNode {
                id: WidgetId(1),
                parent: Some(WidgetId(0)),
                hoverable: true,
                clickable: true,
                scrollable: false,
                opaque: true,
                clip: crate::ui::ClipPolicy::InferFromCapabilities,
                layout: LayoutSpec {
                    direction: LayoutDirection::Row,
                    margin: 6.0,
                    padding: 0.0,
                    sizing: SizingPolicy::Flex {
                        grow: 1.0,
                        shrink: 1.0,
                        basis: 280.0,
                    },
                },
                z_order: 1,
                widget_type: "CounterWidget",
            },
            WidgetNode {
                id: WidgetId(2),
                parent: Some(WidgetId(0)),
                hoverable: true,
                clickable: true,
                scrollable: false,
                opaque: true,
                clip: crate::ui::ClipPolicy::InferFromCapabilities,
                layout: LayoutSpec {
                    direction: LayoutDirection::Row,
                    margin: 6.0,
                    padding: 0.0,
                    sizing: SizingPolicy::Flex {
                        grow: 2.0,
                        shrink: 1.0,
                        basis: 320.0,
                    },
                },
                z_order: 2,
                widget_type: "Banner",
            },
        ];

        let size = Size::new(900.0, 640.0);
        let layout = compute_layout(&nodes, size);

        let r1 = layout.iter().find(|(id, _)| id.0 == 1).unwrap().1;
        let r2 = layout.iter().find(|(id, _)| id.0 == 2).unwrap().1;

        // Container start is 0. Padding 10 + Margin 6 = 16.
        assert_eq!(r1.origin.y, 16.0, "Top gap should be 16");

        // Gap between widgets: r2.top - r1.bottom
        let gap_between = r2.origin.y - r1.max_y();
        assert_eq!(gap_between, 12.0, "Middle gap should be 12 (6+6)");

        // Bottom gap: Window height (640) - r2.bottom
        let gap_bottom = 640.0 - r2.max_y();
        assert_eq!(gap_bottom, 16.0, "Bottom gap should be 16 (6+10)");
    }
}
