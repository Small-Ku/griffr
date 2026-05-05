use std::collections::HashMap;
use winio::primitive::{Point, Rect, Size};

use crate::ui::{TileSpec, WidgetId, WidgetNode};

pub fn merge_adjacent_non_clipped(
    mut tiles: Vec<TileSpec>,
    widget_bounds: &[(WidgetId, Rect)],
    widgets: &[WidgetNode],
) -> Vec<TileSpec> {
    let bounds_by_widget: HashMap<WidgetId, Rect> = widget_bounds.iter().copied().collect();
    let widget_by_id: HashMap<WidgetId, WidgetNode> = widgets.iter().map(|w| (w.id, *w)).collect();
    loop {
        let mut changed = false;
        'outer: for i in 0..tiles.len() {
            for j in (i + 1)..tiles.len() {
                if let Some(candidate) = merged_tile(&tiles[i], &tiles[j]) {
                    let mut others: Vec<TileSpec> =
                        Vec::with_capacity(tiles.len().saturating_sub(2));
                    for (idx, t) in tiles.iter().enumerate() {
                        if idx != i && idx != j {
                            others.push(t.clone());
                        }
                    }
                    let safe = widgets_fit(&candidate, &bounds_by_widget)
                        && no_scroll_or_clip_violation(&candidate, &widget_by_id)
                        && !overlaps_others(&candidate.bounds, &others);
                    if safe {
                        let mut next = others;
                        next.push(candidate);
                        tiles = next;
                        changed = true;
                        break 'outer;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    tiles.sort_by(|a, b| {
        a.bounds
            .origin
            .y
            .total_cmp(&b.bounds.origin.y)
            .then(a.bounds.origin.x.total_cmp(&b.bounds.origin.x))
    });
    tiles
}

fn merged_tile(a: &TileSpec, b: &TileSpec) -> Option<TileSpec> {
    if a.clipped || b.clipped || a.widgets != b.widgets {
        return None;
    }
    let horizontal = a.bounds.max_x() == b.bounds.origin.x
        && a.bounds.origin.y == b.bounds.origin.y
        && a.bounds.size.height == b.bounds.size.height;
    let vertical = a.bounds.max_y() == b.bounds.origin.y
        && a.bounds.origin.x == b.bounds.origin.x
        && a.bounds.size.width == b.bounds.size.width;
    if !(horizontal || vertical) {
        return None;
    }
    let x = a.bounds.origin.x.min(b.bounds.origin.x);
    let y = a.bounds.origin.y.min(b.bounds.origin.y);
    let right = a.bounds.max_x().max(b.bounds.max_x());
    let bottom = a.bounds.max_y().max(b.bounds.max_y());
    Some(TileSpec {
        id: a.id,
        bounds: Rect::new(Point::new(x, y), Size::new(right - x, bottom - y)),
        clipped: false,
        widgets: a.widgets.clone(),
    })
}

fn widgets_fit(tile: &TileSpec, bounds_by_widget: &HashMap<WidgetId, Rect>) -> bool {
    let Some(wid) = tile.widgets.last() else {
        return false;
    };
    bounds_by_widget.get(wid).is_some_and(|b| {
        tile.bounds.origin.x >= b.origin.x
            && tile.bounds.origin.y >= b.origin.y
            && tile.bounds.max_x() <= b.max_x()
            && tile.bounds.max_y() <= b.max_y()
    })
}

fn no_scroll_or_clip_violation(
    tile: &TileSpec,
    widget_by_id: &HashMap<WidgetId, WidgetNode>,
) -> bool {
    let Some(wid) = tile.widgets.last() else {
        return false;
    };
    widget_by_id.get(wid).is_some_and(|w| {
        !w.capabilities.scrollable && !matches!(w.clip, crate::ui::ClipPolicy::ForceClip)
    })
}

fn overlaps_others(candidate: &Rect, others: &[TileSpec]) -> bool {
    others.iter().any(|t| rects_overlap(candidate, &t.bounds))
}

fn rects_overlap(a: &Rect, b: &Rect) -> bool {
    a.origin.x < b.max_x()
        && a.max_x() > b.origin.x
        && a.origin.y < b.max_y()
        && a.max_y() > b.origin.y
}

#[cfg(test)]
mod tests {
    use winio::primitive::{Point, Rect, Size};

    use crate::ui::tile_plan::merge::merge_adjacent_non_clipped;
    use crate::ui::{
        ClipPolicy, LayoutDirection, LayoutSpec, TileId, TileSpec, WidgetCapabilities, WidgetId,
        WidgetNode,
    };

    #[test]
    fn merges_adjacent_unclipped() {
        let tiles = vec![
            TileSpec {
                id: TileId(0),
                bounds: Rect::from_size(Size::new(50.0, 50.0)),
                clipped: false,
                widgets: vec![WidgetId(1)],
            },
            TileSpec {
                id: TileId(1),
                bounds: Rect::new(Point::new(50.0, 0.0), Size::new(50.0, 50.0)),
                clipped: false,
                widgets: vec![WidgetId(1)],
            },
        ];
        let wb = vec![(WidgetId(1), Rect::from_size(Size::new(100.0, 50.0)))];
        let wn = vec![WidgetNode {
            id: WidgetId(1),
            parent: None,
            capabilities: WidgetCapabilities::new(false, false, false),
            clip: ClipPolicy::InferFromCapabilities,
            layout: LayoutSpec {
                direction: LayoutDirection::Row,
                flex_grow: 1.0,
                flex_shrink: 1.0,
                flex_basis: 100.0,
                margin: 0.0,
                padding: 0.0,
            },
            z_order: 0,
            widget_type: "Container",
        }];
        let merged = merge_adjacent_non_clipped(tiles, &wb, &wn);
        assert_eq!(merged.len(), 1);
    }
}
