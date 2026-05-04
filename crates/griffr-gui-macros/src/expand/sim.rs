use crate::model::FlatNode;

pub(crate) fn merged_tile_count_for_flat(flat: &[FlatNode]) -> usize {
    fn count_tiles(node_id: u16, flat: &[FlatNode]) -> usize {
        let children: Vec<_> = flat.iter().filter(|n| n.parent == node_id as i16).collect();
        if children.is_empty() {
            1
        } else {
            let c = children.len();
            let mut sum = 3 * c + 1;
            for child in children {
                sum += count_tiles(child.id, flat);
            }
            sum
        }
    }

    flat.iter()
        .filter(|n| n.parent == -1)
        .map(|n| count_tiles(n.id, flat))
        .sum::<usize>()
        .max(1)
}

#[cfg(test)]
mod tests {
    use crate::expand::sim::merged_tile_count_for_flat;
    use crate::model::FlatNode;

    #[derive(Clone, Copy)]
    struct SimNode {
        id: u16,
        parent: Option<u16>,
        direction: i8,
        flex_grow: f64,
        flex_shrink: f64,
        flex_basis: f64,
        margin: f64,
        padding: f64,
        z: i32,
        scrollable: bool,
        clip: i8,
    }

    #[derive(Clone, Copy)]
    struct SimRect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }

    impl SimRect {
        fn right(&self) -> f64 {
            self.x + self.w
        }
        fn bottom(&self) -> f64 {
            self.y + self.h
        }
        fn contains(&self, x: f64, y: f64) -> bool {
            x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
        }
    }

    #[derive(Clone)]
    struct SimTile {
        bounds: SimRect,
        clipped: bool,
        top: u16,
    }

    fn simulate_layout(nodes: &[SimNode], width: f64, height: f64) -> Vec<(u16, SimRect)> {
        let mut out = Vec::<(u16, SimRect)>::new();
        for root in nodes.iter().copied().filter(|n| n.parent.is_none()) {
            let root_bounds = SimRect {
                x: 0.0,
                y: 0.0,
                w: width,
                h: height,
            };
            out.push((root.id, root_bounds));
            layout_children(root.id, root_bounds, nodes, &mut out);
        }
        out
    }

    fn layout_children(
        parent_id: u16,
        parent_bounds: SimRect,
        nodes: &[SimNode],
        out: &mut Vec<(u16, SimRect)>,
    ) {
        let mut children: Vec<SimNode> = nodes
            .iter()
            .copied()
            .filter(|n| n.parent == Some(parent_id))
            .collect();
        if children.is_empty() {
            return;
        }
        children.sort_by_key(|n| (n.z, n.id));
        let parent = nodes.iter().find(|n| n.id == parent_id).copied();
        let parent_dir = parent.map(|n| n.direction).unwrap_or(1);
        let parent_padding = parent.map(|n| n.padding.max(0.0)).unwrap_or(0.0);
        let content = SimRect {
            x: parent_bounds.x + parent_padding,
            y: parent_bounds.y + parent_padding,
            w: (parent_bounds.w - parent_padding * 2.0).max(1.0),
            h: (parent_bounds.h - parent_padding * 2.0).max(1.0),
        };
        let total_basis: f64 = children
            .iter()
            .map(|n| n.flex_basis.max(1.0) + n.margin.max(0.0) * 2.0)
            .sum();
        let total_grow: f64 = children.iter().map(|n| n.flex_grow.max(0.0)).sum();
        let total_shrink: f64 = children.iter().map(|n| n.flex_shrink.max(0.0)).sum();
        let axis = if parent_dir == 0 {
            content.w
        } else {
            content.h
        };
        let positive_remainder = (axis - total_basis).max(0.0);
        let overflow = (total_basis - axis).max(0.0);
        let (mut cursor_x, mut cursor_y) = (content.x, content.y);
        for child in children {
            let margin = child.margin.max(0.0);
            let grow_share = if total_grow > 0.0 {
                positive_remainder * (child.flex_grow.max(0.0) / total_grow)
            } else {
                0.0
            };
            let shrink_share = if overflow > 0.0 && total_shrink > 0.0 {
                overflow * (child.flex_shrink.max(0.0) / total_shrink)
            } else {
                0.0
            };
            let primary = (child.flex_basis.max(1.0) + grow_share - shrink_share).max(1.0);
            let rect = if parent_dir == 0 {
                let r = SimRect {
                    x: cursor_x + margin,
                    y: content.y + margin,
                    w: (primary - margin * 2.0).max(1.0),
                    h: (content.h - margin * 2.0).max(1.0),
                };
                cursor_x += primary + margin * 2.0;
                r
            } else {
                let r = SimRect {
                    x: content.x + margin,
                    y: cursor_y + margin,
                    w: (content.w - margin * 2.0).max(1.0),
                    h: (primary - margin * 2.0).max(1.0),
                };
                cursor_y += primary + margin * 2.0;
                r
            };
            out.push((child.id, rect));
            layout_children(child.id, rect, nodes, out);
        }
    }

    fn partition_tiles(nodes: &[SimNode], bounds: &[(u16, SimRect)]) -> Vec<SimTile> {
        let mut xs = Vec::<f64>::new();
        let mut ys = Vec::<f64>::new();
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
        let mut out = Vec::<SimTile>::new();
        for yi in 0..ys.len().saturating_sub(1) {
            for xi in 0..xs.len().saturating_sub(1) {
                let (x0, x1, y0, y1) = (xs[xi], xs[xi + 1], ys[yi], ys[yi + 1]);
                if x1 <= x0 || y1 <= y0 {
                    continue;
                }
                let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
                let mut covering = Vec::<(i32, u16, bool)>::new();
                for (wid, rect) in bounds {
                    if rect.contains(cx, cy) {
                        if let Some(node) = nodes.iter().find(|n| n.id == *wid) {
                            covering.push((
                                node.z,
                                node.id,
                                match node.clip {
                                    1 => true,
                                    -1 => false,
                                    _ => node.scrollable,
                                },
                            ));
                        }
                    }
                }
                if covering.is_empty() {
                    continue;
                }
                covering.sort_by_key(|(z, id, _)| (*z, *id));
                let (_, top_id, clipped) = *covering.last().expect("non-empty");
                out.push(SimTile {
                    bounds: SimRect {
                        x: x0,
                        y: y0,
                        w: x1 - x0,
                        h: y1 - y0,
                    },
                    clipped,
                    top: top_id,
                });
            }
        }
        out
    }

    fn merge_adjacent(
        mut tiles: Vec<SimTile>,
        bounds: &[(u16, SimRect)],
        nodes: &[SimNode],
    ) -> Vec<SimTile> {
        loop {
            let mut changed = false;
            'outer: for i in 0..tiles.len() {
                for j in (i + 1)..tiles.len() {
                    if let Some(candidate) = merged_tile(&tiles[i], &tiles[j]) {
                        let mut others =
                            Vec::<SimTile>::with_capacity(tiles.len().saturating_sub(2));
                        for (idx, t) in tiles.iter().enumerate() {
                            if idx != i && idx != j {
                                others.push(t.clone());
                            }
                        }
                        let safe = widgets_fit(&candidate, bounds)
                            && no_scroll_or_clip_violation(&candidate, nodes)
                            && !overlaps_others(candidate.bounds, &others);
                        if safe {
                            others.push(candidate);
                            tiles = others;
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

    fn merged_tile(a: &SimTile, b: &SimTile) -> Option<SimTile> {
        if a.clipped || b.clipped || a.top != b.top {
            return None;
        }
        let horizontal =
            a.bounds.right() == b.bounds.x && a.bounds.y == b.bounds.y && a.bounds.h == b.bounds.h;
        let vertical =
            a.bounds.bottom() == b.bounds.y && a.bounds.x == b.bounds.x && a.bounds.w == b.bounds.w;
        if !(horizontal || vertical) {
            return None;
        }
        Some(SimTile {
            bounds: SimRect {
                x: a.bounds.x.min(b.bounds.x),
                y: a.bounds.y.min(b.bounds.y),
                w: a.bounds.right().max(b.bounds.right()) - a.bounds.x.min(b.bounds.x),
                h: a.bounds.bottom().max(b.bounds.bottom()) - a.bounds.y.min(b.bounds.y),
            },
            clipped: false,
            top: a.top,
        })
    }

    fn widgets_fit(tile: &SimTile, bounds: &[(u16, SimRect)]) -> bool {
        bounds
            .iter()
            .find(|(id, _)| *id == tile.top)
            .is_some_and(|(_, b)| {
                tile.bounds.x >= b.x
                    && tile.bounds.y >= b.y
                    && tile.bounds.right() <= b.right()
                    && tile.bounds.bottom() <= b.bottom()
            })
    }

    fn no_scroll_or_clip_violation(tile: &SimTile, nodes: &[SimNode]) -> bool {
        nodes
            .iter()
            .find(|n| n.id == tile.top)
            .is_some_and(|n| !n.scrollable && n.clip != 1)
    }

    fn overlaps_others(candidate: SimRect, others: &[SimTile]) -> bool {
        others.iter().any(|t| {
            candidate.x < t.bounds.right()
                && candidate.right() > t.bounds.x
                && candidate.y < t.bounds.bottom()
                && candidate.bottom() > t.bounds.y
        })
    }

    fn sample_tree() -> Vec<FlatNode> {
        vec![
            FlatNode {
                id: 0,
                parent: -1,
                kind: "Container".to_string(),
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
            FlatNode {
                id: 1,
                parent: 0,
                kind: "Button".to_string(),
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
            FlatNode {
                id: 2,
                parent: 0,
                kind: "Banner".to_string(),
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
        ]
    }

    fn to_sim_nodes(flat: &[FlatNode]) -> Vec<SimNode> {
        let mut nodes: Vec<SimNode> = flat
            .iter()
            .map(|n| SimNode {
                id: n.id,
                parent: (n.parent >= 0).then_some(n.parent as u16),
                direction: n.direction,
                flex_grow: n.flex_grow,
                flex_shrink: n.flex_shrink,
                flex_basis: n.flex_basis,
                margin: n.margin,
                padding: n.padding,
                z: n.z,
                scrollable: n.scrollable,
                clip: n.clip,
            })
            .collect();
        nodes.sort_by_key(|n| (n.z, n.id));
        nodes
    }

    #[test]
    fn topological_upper_bound_covers_problem_sizes() {
        let flat = sample_tree();
        let nodes = to_sim_nodes(&flat);
        let upper = merged_tile_count_for_flat(&flat);
        for (w, h) in [(1328.0, 1456.0), (1328.0, 757.0), (900.0, 640.0)] {
            let bounds = simulate_layout(&nodes, w, h);
            let merged = merge_adjacent(partition_tiles(&nodes, &bounds), &bounds, &nodes).len();
            println!("w={}, h={}, merged={}, upper={}", w, h, merged, upper);
            assert!(
                merged <= upper,
                "merged tiles {merged} exceed upper bound {upper} at {w}x{h}"
            );
        }
    }
}
