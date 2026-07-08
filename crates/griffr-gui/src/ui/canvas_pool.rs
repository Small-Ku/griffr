use winio::primitive::{Point, Rect, Size};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasPlacement {
    pub loc: Point,
    pub size: Size,
}

impl CanvasPlacement {
    pub fn from_tile_bounds(bounds: Rect, overlap_px: f64) -> Self {
        Self {
            loc: bounds.origin,
            size: Size::new(
                bounds.size.width + overlap_px,
                bounds.size.height + overlap_px,
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CanvasSlotUpdate {
    pub show: bool,
    pub hide: bool,
    pub move_or_resize: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct CanvasSlotState {
    visible: bool,
    placement: Option<CanvasPlacement>,
}

#[derive(Clone, Debug)]
pub struct CanvasPool {
    tile_to_slot: Vec<usize>,
    slot_to_tile: Box<[Option<usize>]>,
    slot_states: Box<[CanvasSlotState]>,
    free_slots: Vec<usize>,
    released_slots: Vec<usize>,
}

impl CanvasPool {
    pub fn new(capacity: usize) -> Self {
        Self {
            tile_to_slot: Vec::new(),
            slot_to_tile: vec![None; capacity].into_boxed_slice(),
            slot_states: vec![CanvasSlotState::default(); capacity].into_boxed_slice(),
            free_slots: (0..capacity).rev().collect(),
            released_slots: Vec::new(),
        }
    }

    pub fn capacity(&self) -> usize {
        self.slot_to_tile.len()
    }

    pub fn active_count(&self) -> usize {
        self.tile_to_slot.len()
    }

    pub fn prepare_frame(&mut self, tile_count: usize) {
        assert!(
            tile_count <= self.capacity(),
            "tile count exceeds canvas pool capacity"
        );
        self.released_slots.clear();

        while self.tile_to_slot.len() > tile_count {
            let slot_idx = self
                .tile_to_slot
                .pop()
                .expect("active slots should exist while shrinking");
            self.slot_to_tile[slot_idx] = None;
            self.free_slots.push(slot_idx);
            self.released_slots.push(slot_idx);
        }

        while self.tile_to_slot.len() < tile_count {
            let slot_idx = self
                .free_slots
                .pop()
                .expect("free canvas slots should exist while growing");
            let tile_idx = self.tile_to_slot.len();
            self.tile_to_slot.push(slot_idx);
            self.slot_to_tile[slot_idx] = Some(tile_idx);
        }
    }

    pub fn slot_for_tile(&self, tile_idx: usize) -> usize {
        self.tile_to_slot[tile_idx]
    }

    pub fn tile_for_slot(&self, slot_idx: usize) -> Option<usize> {
        self.slot_to_tile.get(slot_idx).copied().flatten()
    }

    pub fn drain_released_slots(&mut self) -> impl Iterator<Item = usize> + '_ {
        self.released_slots.drain(..)
    }

    pub fn apply_placement(
        &mut self,
        slot_idx: usize,
        placement: CanvasPlacement,
    ) -> CanvasSlotUpdate {
        let state = &mut self.slot_states[slot_idx];
        let update = CanvasSlotUpdate {
            show: !state.visible,
            hide: false,
            move_or_resize: state.placement != Some(placement),
        };
        state.visible = true;
        state.placement = Some(placement);
        update
    }

    pub fn release_slot(&mut self, slot_idx: usize) -> CanvasSlotUpdate {
        let state = &mut self.slot_states[slot_idx];
        let was_visible = state.visible;
        state.visible = false;
        state.placement = None;
        CanvasSlotUpdate {
            show: false,
            hide: was_visible,
            move_or_resize: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CanvasPlacement, CanvasPool};
    use winio::primitive::{Point, Rect, Size};

    #[test]
    fn prepare_frame_reuses_and_recycles_slots() {
        let mut pool = CanvasPool::new(3);

        pool.prepare_frame(2);
        assert_eq!(pool.active_count(), 2);
        assert_eq!(pool.slot_for_tile(0), 0);
        assert_eq!(pool.slot_for_tile(1), 1);

        pool.prepare_frame(1);
        assert_eq!(pool.active_count(), 1);
        assert_eq!(pool.drain_released_slots().collect::<Vec<_>>(), vec![1]);

        pool.prepare_frame(2);
        assert_eq!(pool.slot_for_tile(0), 0);
        assert_eq!(pool.slot_for_tile(1), 1);
        assert!(pool.drain_released_slots().next().is_none());
    }

    #[test]
    fn apply_placement_only_marks_real_changes() {
        let mut pool = CanvasPool::new(1);
        pool.prepare_frame(1);
        let placement = CanvasPlacement {
            loc: Point::new(10.0, 20.0),
            size: Size::new(30.0, 40.0),
        };

        let first = pool.apply_placement(0, placement);
        assert!(first.show);
        assert!(first.move_or_resize);

        let second = pool.apply_placement(0, placement);
        assert!(!second.show);
        assert!(!second.move_or_resize);

        let released = pool.release_slot(0);
        assert!(released.hide);
    }

    #[test]
    fn placement_from_tile_bounds_applies_overlap() {
        let placement = CanvasPlacement::from_tile_bounds(
            Rect::new(Point::new(4.0, 5.0), Size::new(10.0, 20.0)),
            0.5,
        );

        assert_eq!(placement.loc, Point::new(4.0, 5.0));
        assert_eq!(placement.size, Size::new(10.5, 20.5));
    }
}
