use winio::prelude::Size;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct WidgetId(pub u16);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct TileId(pub u16);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub struct WidgetCapabilities {
    pub hoverable: bool,
    pub clickable: bool,
    pub scrollable: bool,
}

impl WidgetCapabilities {
    pub const fn new(hoverable: bool, clickable: bool, scrollable: bool) -> Self {
        Self {
            hoverable,
            clickable,
            scrollable,
        }
    }
}

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
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }

    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
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
    pub capabilities: WidgetCapabilities,
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

#[derive(Clone, Debug, PartialEq)]
pub struct TilePlan {
    pub tiles: Vec<TileSpec>,
}

#[derive(Clone, Debug)]
pub struct CompiledPlan {
    pub widgets: Vec<WidgetNode>,
    pub bounds: Vec<(WidgetId, Rect)>,
    pub tile_plan: TilePlan,
    pub size: Size,
}

#[derive(Clone, Debug)]
pub struct StaticPlan {
    pub widgets: Vec<WidgetNode>,
    pub merged_tile_count: usize,
}
