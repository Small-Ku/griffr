use winio::prelude::Size;

use crate::ui::layout::compute_layout;
use crate::ui::tile_plan::merge::merge_adjacent_non_clipped;
use crate::ui::{
    ClipPolicy, CompiledPlan, LayoutDirection, LayoutSpec, Rect, StaticPlan, TileId, TilePlan,
    TileSpec, WidgetCapabilities, WidgetDecl, WidgetId, WidgetNode,
};

pub fn compile(decls: &'static [WidgetDecl], size: Size) -> CompiledPlan {
    let static_plan = StaticPlan {
        widgets: build_widgets(decls),
        merged_tile_count: 0,
    };
    compile_dynamic(&static_plan, size)
}

pub fn compile_dynamic(static_plan: &StaticPlan, size: Size) -> CompiledPlan {
    let widgets = static_plan.widgets.clone();
    let bounds = compute_layout(&widgets, size);
    let mut tiles = partition_non_overlapping_tiles(&widgets, &bounds);
    tiles = merge_adjacent_non_clipped(tiles, &bounds, &widgets);
    for (idx, t) in tiles.iter_mut().enumerate() {
        t.id = TileId(idx as u16);
    }

    CompiledPlan {
        widgets,
        bounds,
        tile_plan: TilePlan { tiles },
        size,
    }
}

fn build_widgets(decls: &'static [WidgetDecl]) -> Vec<WidgetNode> {
    let mut widgets: Vec<WidgetNode> = decls
        .iter()
        .map(|d| WidgetNode {
            id: WidgetId(d.id),
            parent: (d.parent >= 0).then_some(WidgetId(d.parent as u16)),
            capabilities: WidgetCapabilities::new(d.hoverable, d.clickable, d.scrollable),
            clip: match d.clip {
                1 => ClipPolicy::ForceClip,
                -1 => ClipPolicy::ForceNoClip,
                _ => ClipPolicy::InferFromCapabilities,
            },
            layout: LayoutSpec {
                direction: if d.direction == 0 {
                    LayoutDirection::Row
                } else {
                    LayoutDirection::Column
                },
                flex_grow: d.flex_grow,
                flex_shrink: d.flex_shrink,
                flex_basis: d.flex_basis,
                margin: d.margin,
                padding: d.padding,
            },
            z_order: d.z,
            widget_type: d.widget_type,
        })
        .collect();
    widgets.sort_by_key(|w| (w.z_order, w.id));
    widgets
}

pub fn partition_non_overlapping_tiles(
    widgets: &[WidgetNode],
    bounds: &[(WidgetId, Rect)],
) -> Vec<TileSpec> {
    let mut xs: Vec<f64> = Vec::new();
    let mut ys: Vec<f64> = Vec::new();
    for (_, r) in bounds {
        xs.push(r.x);
        xs.push(r.right());
        ys.push(r.y);
        ys.push(r.bottom());
    }
    xs.sort_by(|a, b| a.total_cmp(b));
    ys.sort_by(|a, b| a.total_cmp(b));
    xs.dedup();
    ys.dedup();

    let mut out = Vec::<TileSpec>::new();
    for yi in 0..ys.len().saturating_sub(1) {
        for xi in 0..xs.len().saturating_sub(1) {
            let x0 = xs[xi];
            let x1 = xs[xi + 1];
            let y0 = ys[yi];
            let y1 = ys[yi + 1];
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            let cx = (x0 + x1) * 0.5;
            let cy = (y0 + y1) * 0.5;
            let mut covering: Vec<(i32, WidgetId, bool)> = Vec::new();
            for (wid, rect) in bounds {
                if rect.contains(cx, cy) {
                    if let Some(node) = widgets.iter().find(|w| w.id == *wid) {
                        let clipped = match node.clip {
                            ClipPolicy::InferFromCapabilities => node.capabilities.scrollable,
                            ClipPolicy::ForceClip => true,
                            ClipPolicy::ForceNoClip => false,
                        };
                        covering.push((node.z_order, node.id, clipped));
                    }
                }
            }
            if covering.is_empty() {
                continue;
            }
            covering.sort_by_key(|(z, id, _)| (*z, *id));
            let (_, top_id, top_clipped) = *covering.last().expect("non-empty covering");
            out.push(TileSpec {
                id: TileId(out.len() as u16),
                bounds: Rect::new(x0, y0, x1 - x0, y1 - y0),
                clipped: top_clipped,
                widgets: vec![top_id],
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use winio::prelude::Size;

    use crate::ui::tile_plan::compile::{compile, partition_non_overlapping_tiles};
    use crate::ui::{Rect, WidgetDecl, WidgetId};

    #[test]
    fn clip_inference_scrollable() {
        let decls = &[WidgetDecl {
            id: 0,
            parent: -1,
            widget_type: "Banner",
            hoverable: true,
            clickable: true,
            scrollable: true,
            clip: 0,
            z: 0,
            direction: 1,
            flex_grow: 1.0,
            flex_shrink: 1.0,
            flex_basis: 100.0,
            margin: 0.0,
            padding: 0.0,
        }];
        let plan = compile(decls, Size::new(100.0, 100.0));
        assert!(plan.tile_plan.tiles[0].clipped);
    }

    #[test]
    fn three_by_three_center_clipped_before_merge() {
        let decls = &[
            WidgetDecl {
                id: 0,
                parent: -1,
                widget_type: "Container",
                hoverable: false,
                clickable: false,
                scrollable: false,
                clip: 0,
                z: 0,
                direction: 0,
                flex_grow: 0.0,
                flex_shrink: 1.0,
                flex_basis: 300.0,
                margin: 0.0,
                padding: 0.0,
            },
            WidgetDecl {
                id: 1,
                parent: 0,
                widget_type: "Center",
                hoverable: true,
                clickable: true,
                scrollable: true,
                clip: 1,
                z: 10,
                direction: 1,
                flex_grow: 0.0,
                flex_shrink: 1.0,
                flex_basis: 100.0,
                margin: 100.0,
                padding: 0.0,
            },
        ];
        let mut plan = compile(decls, Size::new(300.0, 300.0));
        if let Some((_, r)) = plan.bounds.iter_mut().find(|(id, _)| id.0 == 0) {
            *r = Rect::new(0.0, 0.0, 300.0, 300.0);
        }
        if let Some((_, r)) = plan.bounds.iter_mut().find(|(id, _)| id.0 == 1) {
            *r = Rect::new(100.0, 100.0, 100.0, 100.0);
        }
        let pre = partition_non_overlapping_tiles(&plan.widgets, &plan.bounds);
        assert_eq!(pre.len(), 9);
        let center = pre
            .iter()
            .find(|t| t.bounds.x == 100.0 && t.bounds.y == 100.0)
            .expect("center tile required");
        assert!(center.clipped);
        assert!(center.widgets.contains(&WidgetId(1)));
    }

    #[test]
    fn banner_region_survives_after_merge() {
        let decls = &[
            WidgetDecl {
                id: 0,
                parent: -1,
                widget_type: "Container",
                hoverable: false,
                clickable: false,
                scrollable: false,
                clip: 0,
                z: 0,
                direction: 1,
                flex_grow: 1.0,
                flex_shrink: 1.0,
                flex_basis: 600.0,
                margin: 0.0,
                padding: 10.0,
            },
            WidgetDecl {
                id: 1,
                parent: 0,
                widget_type: "Button",
                hoverable: true,
                clickable: true,
                scrollable: false,
                clip: 0,
                z: 1,
                direction: 0,
                flex_grow: 1.0,
                flex_shrink: 1.0,
                flex_basis: 280.0,
                margin: 6.0,
                padding: 0.0,
            },
            WidgetDecl {
                id: 2,
                parent: 0,
                widget_type: "Banner",
                hoverable: true,
                clickable: false,
                scrollable: true,
                clip: 1,
                z: 2,
                direction: 0,
                flex_grow: 2.0,
                flex_shrink: 1.0,
                flex_basis: 320.0,
                margin: 6.0,
                padding: 0.0,
            },
        ];

        let plan = compile(decls, Size::new(900.0, 640.0));
        let banner_tiles = plan
            .tile_plan
            .tiles
            .iter()
            .filter(|t| t.widgets.last() == Some(&WidgetId(2)))
            .count();
        assert!(banner_tiles > 0, "banner must own at least one tile");
    }

    #[test]
    fn merged_tiles_do_not_overlap() {
        let decls = &[
            WidgetDecl {
                id: 0,
                parent: -1,
                widget_type: "Container",
                hoverable: false,
                clickable: false,
                scrollable: false,
                clip: 0,
                z: 0,
                direction: 1,
                flex_grow: 1.0,
                flex_shrink: 1.0,
                flex_basis: 600.0,
                margin: 0.0,
                padding: 10.0,
            },
            WidgetDecl {
                id: 1,
                parent: 0,
                widget_type: "Button",
                hoverable: true,
                clickable: true,
                scrollable: false,
                clip: 0,
                z: 1,
                direction: 0,
                flex_grow: 1.0,
                flex_shrink: 1.0,
                flex_basis: 280.0,
                margin: 6.0,
                padding: 0.0,
            },
            WidgetDecl {
                id: 2,
                parent: 0,
                widget_type: "Banner",
                hoverable: true,
                clickable: false,
                scrollable: true,
                clip: 1,
                z: 2,
                direction: 0,
                flex_grow: 2.0,
                flex_shrink: 1.0,
                flex_basis: 320.0,
                margin: 6.0,
                padding: 0.0,
            },
        ];
        let plan = compile(decls, Size::new(900.0, 640.0));
        let tiles = &plan.tile_plan.tiles;
        for i in 0..tiles.len() {
            for j in (i + 1)..tiles.len() {
                let a = tiles[i].bounds;
                let b = tiles[j].bounds;
                let overlap = a.x < b.right() && a.right() > b.x && a.y < b.bottom() && a.bottom() > b.y;
                assert!(!overlap, "tiles {i} and {j} overlap");
            }
        }
    }
}
