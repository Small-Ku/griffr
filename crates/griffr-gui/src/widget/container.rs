use crate::ui::{DrawResources, TileSlot, Widget};
use winio::prelude::{Color, DrawingContext, Rect, Result, Size};

pub struct Container {
    tile: TileSlot,
}

impl Widget for Container {
    fn init(tile: TileSlot) -> Result<Self> {
        Ok(Self { tile })
    }

    fn bounds(&self) -> Rect {
        self.tile.bounds
    }

    fn opaque(&self) -> bool {
        true
    }
    fn sizing_policy(&self) -> crate::ui::SizingPolicy {
        self.tile.sizing
    }

    fn draw(
        &mut self,
        ctx: &mut DrawingContext<'_>,
        resources: &mut DrawResources,
        size: Size,
        _clipped: bool,
    ) -> Result<()> {
        let brush = resources.solid_brush(Color::new(0x1E, 0x22, 0x2B, 0xFF));
        ctx.fill_rect(&brush, Rect::from_size(size))?;
        Ok(())
    }
}
