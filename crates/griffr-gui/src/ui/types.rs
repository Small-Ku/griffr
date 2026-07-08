use bitflags::bitflags;
use std::time::Instant;

use crate::ui::DrawResources;
use winio::prelude::Result;
use winio::primitive::{Rect, Size};
use winio::ui::DrawingContext;
use winio::widgets::CanvasEvent;

bitflags! {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct DirtyFlags: u8 {
        const PAINT = 1 << 0;
        const LAYOUT = 1 << 1;
        const TILE_PLAN = 1 << 2;
        const RESOURCES = 1 << 3;
    }
}

#[derive(Clone, Debug)]
pub struct TileSlot {
    pub bounds: Rect,
    pub clipped: bool,
    pub sizing: SizingPolicy,
}

pub trait Widget {
    fn init(tile: TileSlot) -> Result<Self>
    where
        Self: Sized;
    fn bounds(&self) -> Rect;

    // Routing capabilities
    fn hoverable(&self) -> bool {
        false
    }
    fn clickable(&self) -> bool {
        false
    }
    fn scrollable(&self) -> bool {
        false
    }

    // Rendering properties
    fn opaque(&self) -> bool {
        false
    }

    fn draw(
        &mut self,
        _ctx: &mut DrawingContext<'_>,
        _resources: &mut DrawResources,
        _size: Size,
        _clipped: bool,
    ) -> Result<()> {
        Ok(())
    }
    fn handle_event(&mut self, _event: &CanvasEvent, _is_target: bool) -> Result<DirtyFlags> {
        Ok(DirtyFlags::empty())
    }
    fn next_redraw_at(&self) -> Option<Instant> {
        None
    }
    fn on_animation_frame(&mut self, _now: Instant) -> DirtyFlags {
        DirtyFlags::empty()
    }
    fn sizing_policy(&self) -> SizingPolicy {
        SizingPolicy::default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SizingPolicy {
    Flex { grow: f64, shrink: f64, basis: f64 },
    AspectRatio(f64),
    Fixed(Size),
}

impl Default for SizingPolicy {
    fn default() -> Self {
        Self::Flex {
            grow: 0.0,
            shrink: 1.0,
            basis: 100.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct WidgetId(pub u16);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct TileId(pub u16);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipPolicy {
    InferFromCapabilities,
    ForceClip,
    ForceNoClip,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutDirection {
    Row,
    Column,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutSpec {
    pub direction: LayoutDirection,
    pub margin: f64,
    pub padding: f64,
    pub sizing: SizingPolicy,
}

impl Default for LayoutSpec {
    fn default() -> Self {
        Self {
            direction: LayoutDirection::Column,
            margin: 0.0,
            padding: 0.0,
            sizing: SizingPolicy::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WidgetNode {
    pub id: WidgetId,
    pub parent: Option<WidgetId>,
    pub hoverable: bool,
    pub clickable: bool,
    pub scrollable: bool,
    pub opaque: bool,
    pub clip: ClipPolicy,
    pub layout: LayoutSpec,
    pub z_order: i32,
    pub widget_type: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TileSpec {
    pub id: TileId,
    pub bounds: Rect,
    pub clipped: bool,
    pub widgets: Vec<WidgetId>,
    pub signature: TileSignature,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileSignature {
    pub draw_stack: Vec<WidgetId>,
    pub clip_id: Option<WidgetId>,
    pub opacity_barrier: Option<WidgetId>,
    pub scroll_space: Option<WidgetId>,
    pub clipped: bool,
}

impl TileSpec {
    pub fn signature(&self) -> &TileSignature {
        &self.signature
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TilePlan {
    pub tiles: Vec<TileSpec>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TilePlanWidgetKey {
    pub id: WidgetId,
    pub scrollable: bool,
    pub opaque: bool,
    pub clip: ClipPolicy,
    pub z_order: i32,
}

pub struct CompiledPlan {
    pub widgets: Vec<WidgetNode>,
    pub bounds: Box<[Rect]>,
    pub dirty: Box<[DirtyFlags]>,
    pub tile_plan: TilePlan,
    pub size: Size,
}

impl WidgetNode {
    pub fn tile_plan_key(&self) -> TilePlanWidgetKey {
        TilePlanWidgetKey {
            id: self.id,
            scrollable: self.scrollable,
            opaque: self.opaque,
            clip: self.clip,
            z_order: self.z_order,
        }
    }
}

impl CompiledPlan {
    pub fn dirty_summary(&self) -> DirtyFlags {
        self.dirty
            .iter()
            .copied()
            .fold(DirtyFlags::empty(), |acc, flags| acc | flags)
    }

    pub fn mark_widget_dirty(&mut self, id: WidgetId, flags: DirtyFlags) {
        if flags.is_empty() {
            return;
        }
        if let Some(slot) = self.dirty.get_mut(id.0 as usize) {
            *slot |= flags;
        }
    }

    pub fn clear_dirty(&mut self) {
        for flags in &mut self.dirty {
            *flags = DirtyFlags::empty();
        }
    }

    pub fn can_reuse_tile_plan(&self, widgets: &[WidgetNode], bounds: &[Rect]) -> bool {
        self.bounds.as_ref() == bounds
            && self.widgets.len() == widgets.len()
            && self
                .widgets
                .iter()
                .zip(widgets.iter())
                .all(|(old, new)| old.tile_plan_key() == new.tile_plan_key())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ClipPolicy, CompiledPlan, DirtyFlags, LayoutDirection, LayoutSpec, SizingPolicy, TilePlan,
        WidgetId, WidgetNode,
    };
    use winio::primitive::{Rect, Size};

    fn widget_node(id: u16) -> WidgetNode {
        WidgetNode {
            id: WidgetId(id),
            parent: None,
            hoverable: false,
            clickable: false,
            scrollable: false,
            opaque: false,
            clip: ClipPolicy::InferFromCapabilities,
            layout: LayoutSpec {
                direction: LayoutDirection::Column,
                margin: 0.0,
                padding: 0.0,
                sizing: SizingPolicy::Flex {
                    grow: 0.0,
                    shrink: 1.0,
                    basis: 100.0,
                },
            },
            z_order: id as i32,
            widget_type: "TestWidget",
        }
    }

    fn compiled_plan(widgets: Vec<WidgetNode>, bounds: Vec<Rect>) -> CompiledPlan {
        CompiledPlan {
            widgets,
            bounds: bounds.into_boxed_slice(),
            dirty: vec![DirtyFlags::empty(); 2].into_boxed_slice(),
            tile_plan: TilePlan { tiles: Vec::new() },
            size: Size::new(100.0, 100.0),
        }
    }

    #[test]
    fn dirty_summary_accumulates_widget_flags() {
        let mut plan = compiled_plan(
            vec![widget_node(0), widget_node(1)],
            vec![
                Rect::from_size(Size::new(100.0, 100.0)),
                Rect::from_size(Size::new(50.0, 50.0)),
            ],
        );

        plan.mark_widget_dirty(WidgetId(0), DirtyFlags::PAINT);
        plan.mark_widget_dirty(WidgetId(1), DirtyFlags::TILE_PLAN | DirtyFlags::RESOURCES);

        assert_eq!(
            plan.dirty_summary(),
            DirtyFlags::PAINT | DirtyFlags::TILE_PLAN | DirtyFlags::RESOURCES
        );

        plan.clear_dirty();
        assert!(plan.dirty_summary().is_empty());
    }

    #[test]
    fn tile_plan_cache_ignores_routing_only_widget_changes() {
        let old_widgets = vec![widget_node(0), widget_node(1)];
        let bounds = vec![
            Rect::from_size(Size::new(100.0, 100.0)),
            Rect::from_size(Size::new(50.0, 50.0)),
        ];
        let plan = compiled_plan(old_widgets.clone(), bounds.clone());

        let mut new_widgets = old_widgets;
        new_widgets[1].hoverable = true;
        new_widgets[1].clickable = true;

        assert!(plan.can_reuse_tile_plan(&new_widgets, &bounds));
    }

    #[test]
    fn tile_plan_cache_rejects_signature_affecting_widget_changes() {
        let old_widgets = vec![widget_node(0), widget_node(1)];
        let bounds = vec![
            Rect::from_size(Size::new(100.0, 100.0)),
            Rect::from_size(Size::new(50.0, 50.0)),
        ];
        let plan = compiled_plan(old_widgets.clone(), bounds.clone());

        let mut new_widgets = old_widgets;
        new_widgets[1].scrollable = true;

        assert!(!plan.can_reuse_tile_plan(&new_widgets, &bounds));
    }

    #[test]
    fn tile_plan_cache_rejects_bounds_changes() {
        let widgets = vec![widget_node(0), widget_node(1)];
        let old_bounds = vec![
            Rect::from_size(Size::new(100.0, 100.0)),
            Rect::from_size(Size::new(50.0, 50.0)),
        ];
        let new_bounds = vec![
            Rect::from_size(Size::new(100.0, 100.0)),
            Rect::from_size(Size::new(60.0, 50.0)),
        ];
        let plan = compiled_plan(widgets.clone(), old_bounds);

        assert!(!plan.can_reuse_tile_plan(&widgets, &new_bounds));
    }
}
