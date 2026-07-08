use std::time::Instant;

use winio::prelude::Result;
use winio::primitive::{Rect, Size};
use winio::ui::DrawingContext;
use winio::widgets::CanvasEvent;

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

    fn draw(&mut self, _ctx: &mut DrawingContext<'_>, _size: Size, _clipped: bool) -> Result<()> {
        Ok(())
    }
    fn handle_event(&mut self, _event: &CanvasEvent, _is_target: bool) -> Result<()> {
        Ok(())
    }
    fn next_redraw_at(&self) -> Option<Instant> {
        None
    }
    fn on_animation_frame(&mut self, _now: Instant) -> bool {
        false
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

#[derive(Clone, Debug)]
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

pub struct TilePlan {
    pub tiles: Vec<TileSpec>,
}

pub struct CompiledPlan {
    pub widgets: Vec<WidgetNode>,
    pub bounds: Box<[Rect]>,
    pub dirty: Box<[bool]>,
    pub tile_plan: TilePlan,
    pub size: Size,
}
