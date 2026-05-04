use std::collections::HashMap;

use crate::ui::{Rect, TileSpec, WidgetId, WidgetNode};

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
                    let mut others: Vec<TileSpec> = Vec::with_capacity(tiles.len().saturating_sub(2));
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
            .y
            .total_cmp(&b.bounds.y)
            .then(a.bounds.x.total_cmp(&b.bounds.x))
    });
    tiles
}

fn merged_tile(a: &TileSpec, b: &TileSpec) -> Option<TileSpec> {
    if a.clipped || b.clipped || a.widgets != b.widgets {
        return None;
    }
    let horizontal = a.bounds.right() == b.bounds.x && a.bounds.y == b.bounds.y && a.bounds.h == b.bounds.h;
    let vertical = a.bounds.bottom() == b.bounds.y && a.bounds.x == b.bounds.x && a.bounds.w == b.bounds.w;
    if !(horizontal || vertical) {
        return None;
    }
    let x = a.bounds.x.min(b.bounds.x);
    let y = a.bounds.y.min(b.bounds.y);
    let right = a.bounds.right().max(b.bounds.right());
    let bottom = a.bounds.bottom().max(b.bounds.bottom());
    Some(TileSpec {
        id: a.id,
        bounds: Rect::new(x, y, right - x, bottom - y),
        clipped: false,
        widgets: a.widgets.clone(),
    })
}

fn widgets_fit(tile: &TileSpec, bounds_by_widget: &HashMap<WidgetId, Rect>) -> bool {
    let Some(wid) = tile.widgets.last() else {
        return false;
    };
    bounds_by_widget.get(wid).is_some_and(|b| {
        tile.bounds.x >= b.x
            && tile.bounds.y >= b.y
            && tile.bounds.right() <= b.right()
            && tile.bounds.bottom() <= b.bottom()
    })
}

fn no_scroll_or_clip_violation(tile: &TileSpec, widget_by_id: &HashMap<WidgetId, WidgetNode>) -> bool {
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
    a.x < b.right() && a.right() > b.x && a.y < b.bottom() && a.bottom() > b.y
}

#[cfg(test)]
mod tests {
    use crate::ui::tile_plan::merge::merge_adjacent_non_clipped;
    use crate::ui::{ClipPolicy, LayoutDirection, LayoutSpec, Rect, TileId, TileSpec, WidgetCapabilities, WidgetId, WidgetNode};

    #[test]
    fn merges_adjacent_unclipped() {
        let tiles = vec![
            TileSpec { id: TileId(0), bounds: Rect::new(0.0, 0.0, 50.0, 50.0), clipped: false, widgets: vec![WidgetId(1)] },
            TileSpec { id: TileId(1), bounds: Rect::new(50.0, 0.0, 50.0, 50.0), clipped: false, widgets: vec![WidgetId(1)] },
        ];
        let wb = vec![(WidgetId(1), Rect::new(0.0, 0.0, 100.0, 50.0))];
        let wn = vec![WidgetNode {
            id: WidgetId(1),
            parent: None,
            capabilities: WidgetCapabilities::new(false, false, false),
            clip: ClipPolicy::InferFromCapabilities,
            layout: LayoutSpec { direction: LayoutDirection::Row, flex_grow: 1.0, flex_shrink: 1.0, flex_basis: 100.0, margin: 0.0, padding: 0.0 },
            z_order: 0,
            widget_type: "Container",
        }];
        let merged = merge_adjacent_non_clipped(tiles, &wb, &wn);
        assert_eq!(merged.len(), 1);
    }
}
