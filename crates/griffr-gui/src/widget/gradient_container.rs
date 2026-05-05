use crate::ui::{TileSlot, Widget};
use winio::prelude::{Color, DrawingContext, Point, Rect, Result, Size};
use winio::primitive::{GradientStop, LinearGradientBrush, RelativePoint};

pub struct GradientContainer {
    tile: TileSlot,
}

impl Widget for GradientContainer {
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

    fn draw(&mut self, ctx: &mut DrawingContext<'_>, size: Size, _clipped: bool) -> Result<()> {
        let gradient = LinearGradientBrush {
            start: RelativePoint::new(0.0, 0.0), // Top-left
            end: RelativePoint::new(1.0, 1.0),   // Bottom-right
            stops: vec![
                GradientStop {
                    pos: 0.0,
                    color: Color::new(255, 0, 0, 255),
                }, // Red
                GradientStop {
                    pos: 0.5,
                    color: Color::new(0, 255, 0, 255),
                }, // Green
                GradientStop {
                    pos: 1.0,
                    color: Color::new(0, 0, 255, 255),
                }, // Blue
            ],
        };
        ctx.fill_rect(&gradient, Rect::new(Point::new(0.0, 0.0), size))?;
        Ok(())
    }
}
