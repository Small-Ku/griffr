use winio::primitive::{Point, Rect, Size};

use crate::ui::{ClipPolicy, TileId, TileSpec, WidgetId, WidgetNode};

pub fn partition_non_overlapping_tiles(
    widgets: &[WidgetNode],
    bounds: &[(WidgetId, Rect)],
) -> Vec<TileSpec> {
    let mut xs: Vec<f64> = Vec::new();
    let mut ys: Vec<f64> = Vec::new();
    for (_, r) in bounds {
        xs.push(r.origin.x);
        xs.push(r.max_x());
        ys.push(r.origin.y);
        ys.push(r.max_y());
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
                if rect.contains(Point::new(cx, cy)) {
                    if let Some(node) = widgets.iter().find(|w| w.id == *wid) {
                        let clipped = match node.clip {
                            ClipPolicy::InferFromCapabilities => node.scrollable,
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
            let (_, _, top_clipped) = *covering.last().expect("non-empty covering");

            let mut widget_ids: Vec<WidgetId> = Vec::new();
            for (_, id, _) in covering.iter().rev() {
                widget_ids.push(*id);
                if let Some(node) = widgets.iter().find(|w| w.id == *id) {
                    if node.opaque {
                        break;
                    }
                }
            }
            widget_ids.reverse();

            out.push(TileSpec {
                id: TileId(out.len() as u16),
                bounds: Rect::new(Point::new(x0, y0), Size::new(x1 - x0, y1 - y0)),
                clipped: top_clipped,
                widgets: widget_ids,
            });
        }
    }
    out
}
