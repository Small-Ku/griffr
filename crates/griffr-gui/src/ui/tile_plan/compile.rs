use winio::primitive::{Point, Rect, Size};

use crate::ui::{ClipPolicy, TileId, TileSpec, WidgetId, WidgetNode};

pub fn partition_non_overlapping_tiles(widgets: &[WidgetNode], bounds: &[Rect]) -> Vec<TileSpec> {
    if bounds.is_empty() {
        return Vec::new();
    }

    // Collect all unique Y coordinates
    let mut ys: Vec<f64> = Vec::new();
    for r in bounds {
        ys.push(r.origin.y);
        ys.push(r.max_y());
    }
    ys.sort_by(|a, b| a.total_cmp(b));
    ys.dedup();

    if ys.len() < 2 {
        return Vec::new();
    }

    struct ActiveTile {
        x0: f64,
        x1: f64,
        y0: f64,
        y1: f64,
        signature: crate::ui::TileSignature,
        clipped: bool,
        widgets: Vec<WidgetId>,
    }

    let mut active_tiles: Vec<ActiveTile> = Vec::new();
    let mut final_tiles: Vec<TileSpec> = Vec::new();

    // Iterate through Y-bands
    for yi in 0..ys.len() - 1 {
        let y_start = ys[yi];
        let y_end = ys[yi + 1];

        // Find widgets overlapping this Y-band
        let mut overlapping_indices = Vec::new();
        for (idx, r) in bounds.iter().enumerate() {
            if r.origin.y < y_end && r.max_y() > y_start {
                overlapping_indices.push(idx);
            }
        }

        if overlapping_indices.is_empty() {
            // Close all active tiles since this band has no widgets
            for active in active_tiles.drain(..) {
                final_tiles.push(TileSpec {
                    id: TileId(0), // will be re-indexed later
                    bounds: Rect::new(
                        Point::new(active.x0, active.y0),
                        Size::new(active.x1 - active.x0, active.y1 - active.y0),
                    ),
                    clipped: active.clipped,
                    widgets: active.widgets,
                    signature: active.signature,
                });
            }
            continue;
        }

        // Collect unique X coordinates for overlapping widgets
        let mut xs: Vec<f64> = Vec::new();
        for &idx in &overlapping_indices {
            let r = &bounds[idx];
            xs.push(r.origin.x);
            xs.push(r.max_x());
        }
        xs.sort_by(|a, b| a.total_cmp(b));
        xs.dedup();

        if xs.len() < 2 {
            continue;
        }

        struct Run {
            x0: f64,
            x1: f64,
            signature: crate::ui::TileSignature,
            clipped: bool,
            widgets: Vec<WidgetId>,
        }

        let mut current_runs: Vec<Run> = Vec::new();

        // X-axis scan
        for xi in 0..xs.len() - 1 {
            let x0 = xs[xi];
            let x1 = xs[xi + 1];
            let cx = (x0 + x1) * 0.5;
            let cy = (y_start + y_end) * 0.5;

            // Find covering widgets in this interval
            let mut covering: Vec<(i32, WidgetId, bool)> = Vec::new();
            for &idx in &overlapping_indices {
                let rect = &bounds[idx];
                if rect.contains(Point::new(cx, cy)) {
                    let node = &widgets[idx];
                    let clipped = match node.clip {
                        ClipPolicy::InferFromCapabilities => node.scrollable,
                        ClipPolicy::ForceClip => true,
                        ClipPolicy::ForceNoClip => false,
                    };
                    covering.push((node.z_order, node.id, clipped));
                }
            }

            if covering.is_empty() {
                continue;
            }

            covering.sort_by_key(|(z, id, _)| (*z, *id));
            let (_, _, top_clipped) = *covering.last().expect("non-empty covering");

            let mut draw_stack: Vec<WidgetId> = Vec::new();
            let mut opacity_barrier = None;
            for &(_, id, _) in covering.iter().rev() {
                draw_stack.push(id);
                let node = &widgets[id.0 as usize];
                if node.opaque {
                    opacity_barrier = Some(id);
                    break;
                }
            }
            draw_stack.reverse();

            let mut clip_id = None;
            let mut scroll_space = None;
            for &id in &draw_stack {
                let node = &widgets[id.0 as usize];
                if node.scrollable {
                    scroll_space = Some(id);
                }
                let clipped = match node.clip {
                    ClipPolicy::InferFromCapabilities => node.scrollable,
                    ClipPolicy::ForceClip => true,
                    ClipPolicy::ForceNoClip => false,
                };
                if clipped {
                    clip_id = Some(id);
                }
            }

            let signature = crate::ui::TileSignature {
                draw_stack: draw_stack.clone(),
                clip_id,
                opacity_barrier,
                scroll_space,
                clipped: top_clipped,
            };

            // Horizontal merge with previous run in the same band
            if let Some(last_run) = current_runs.last_mut() {
                if last_run.signature == signature && last_run.clipped == top_clipped {
                    last_run.x1 = x1;
                    continue;
                }
            }

            current_runs.push(Run {
                x0,
                x1,
                signature,
                clipped: top_clipped,
                widgets: draw_stack,
            });
        }

        // Vertical merge with active_tiles
        let mut next_active_tiles = Vec::new();
        let mut matched_runs = vec![false; current_runs.len()];

        for mut active in active_tiles {
            let mut merged = false;
            if active.y1 == y_start {
                // Try to find a matching run in current_runs
                for (r_idx, run) in current_runs.iter().enumerate() {
                    if !matched_runs[r_idx]
                        && active.x0 == run.x0
                        && active.x1 == run.x1
                        && active.signature == run.signature
                        && active.clipped == run.clipped
                    {
                        active.y1 = y_end;
                        matched_runs[r_idx] = true;
                        merged = true;
                        break;
                    }
                }
            }

            if merged {
                next_active_tiles.push(active);
            } else {
                // Cannot extend further, output it
                final_tiles.push(TileSpec {
                    id: TileId(0),
                    bounds: Rect::new(
                        Point::new(active.x0, active.y0),
                        Size::new(active.x1 - active.x0, active.y1 - active.y0),
                    ),
                    clipped: active.clipped,
                    widgets: active.widgets,
                    signature: active.signature,
                });
            }
        }

        // Any unmatched runs become new active tiles
        for (r_idx, run) in current_runs.into_iter().enumerate() {
            if !matched_runs[r_idx] {
                next_active_tiles.push(ActiveTile {
                    x0: run.x0,
                    x1: run.x1,
                    y0: y_start,
                    y1: y_end,
                    signature: run.signature,
                    clipped: run.clipped,
                    widgets: run.widgets,
                });
            }
        }

        active_tiles = next_active_tiles;
    }

    // Flush remaining active tiles
    for active in active_tiles {
        final_tiles.push(TileSpec {
            id: TileId(0),
            bounds: Rect::new(
                Point::new(active.x0, active.y0),
                Size::new(active.x1 - active.x0, active.y1 - active.y0),
            ),
            clipped: active.clipped,
            widgets: active.widgets,
            signature: active.signature,
        });
    }

    // Assign final sequential tile IDs
    for (idx, tile) in final_tiles.iter_mut().enumerate() {
        tile.id = TileId(idx as u16);
    }

    final_tiles
}
