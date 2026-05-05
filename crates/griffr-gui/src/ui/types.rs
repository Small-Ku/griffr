use winio::prelude::Result;
use winio::primitive::{Rect, Size};
use winio::ui::DrawingContext;
use winio::widgets::CanvasEvent;

#[derive(Clone, Debug)]
pub struct TileSlot {
    pub bounds: Rect,
    pub clipped: bool,
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
    pub flex_grow: f64,
    pub flex_shrink: f64,
    pub flex_basis: f64,
    pub margin: f64,
    pub padding: f64,
}

impl Default for LayoutSpec {
    fn default() -> Self {
        Self {
            direction: LayoutDirection::Column,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: 100.0,
            margin: 0.0,
            padding: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct WidgetDecl {
    pub id: u16,
    pub parent: i16,
    pub widget_type: &'static str,
    pub hoverable: bool,
    pub clickable: bool,
    pub scrollable: bool,
    pub opaque: bool,
    pub clip: i8,
    pub z: i32,
    pub direction: i8,
    pub flex_grow: f64,
    pub flex_shrink: f64,
    pub flex_basis: f64,
    pub margin: f64,
    pub padding: f64,
}

#[derive(Clone, Copy, Debug)]
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
}

#[derive(Clone, Debug)]
pub struct StaticPlan {
    pub widgets: Vec<WidgetNode>,
    pub merged_tile_count: usize,
}

pub struct TilePlan {
    pub tiles: Vec<TileSpec>,
}

pub struct CompiledPlan {
    pub widgets: Vec<WidgetNode>,
    pub bounds: Vec<(WidgetId, Rect)>,
    pub tile_plan: TilePlan,
    pub size: Size,
}
