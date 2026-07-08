use winio::primitive::{Point, Rect, Size};

use crate::ui::{TileSpec, WidgetId, WidgetNode};

pub fn merge_adjacent_non_clipped(
    mut tiles: Vec<TileSpec>,
    _widget_bounds: &[Rect],
    _widgets: &[WidgetNode],
) -> Vec<TileSpec> {
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
                    let safe = !overlaps_others(&candidate.bounds, &others);
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
    tiles
}

fn merged_tile(
    a: &TileSpec,
    b: &TileSpec,
) -> Option<TileSpec> {
    if a.signature() != b.signature() {
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
        clipped: a.clipped,
        widgets: a.widgets.clone(),
        signature: a.signature.clone(),
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
    use rapidhash::RapidHashMap as HashMap;
    use winio::primitive::{Point, Rect, Size};

    use crate::ui::tile_plan::merge::merge_adjacent_non_clipped;
    use crate::ui::{
        ClipPolicy, LayoutDirection, LayoutSpec, SizingPolicy, TileId, TileSpec, WidgetId,
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
                signature: crate::ui::TileSignature {
                    draw_stack: vec![WidgetId(1)],
                    clip_id: None,
                    opacity_barrier: Some(WidgetId(1)),
                    scroll_space: None,
                    clipped: false,
                },
            },
            TileSpec {
                id: TileId(1),
                bounds: Rect::new(Point::new(50.0, 0.0), Size::new(50.0, 50.0)),
                clipped: false,
                widgets: vec![WidgetId(1)],
                signature: crate::ui::TileSignature {
                    draw_stack: vec![WidgetId(1)],
                    clip_id: None,
                    opacity_barrier: Some(WidgetId(1)),
                    scroll_space: None,
                    clipped: false,
                },
            },
        ];
        let wb = vec![Rect::default(), Rect::from_size(Size::new(100.0, 50.0))];
        let wn = vec![WidgetNode {
            id: WidgetId(1),
            parent: None,
            hoverable: false,
            clickable: false,
            scrollable: false,
            opaque: true,
            clip: ClipPolicy::InferFromCapabilities,
            layout: LayoutSpec {
                direction: LayoutDirection::Row,
                margin: 0.0,
                padding: 0.0,
                sizing: SizingPolicy::Flex {
                    grow: 1.0,
                    shrink: 1.0,
                    basis: 100.0,
                },
            },
            z_order: 0,
            widget_type: "Container",
        }];
        let wn: HashMap<WidgetId, WidgetNode> = wn.iter().map(|w| (w.id, w.clone())).collect();
        let merged =
            merge_adjacent_non_clipped(tiles, &wb, &wn.values().cloned().collect::<Vec<_>>());
        assert_eq!(merged.len(), 1);
    }
}
