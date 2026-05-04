use winio::prelude::{Color, DrawingContext, Point, Result, Size, SolidColorBrush};

use crate::ui::{CanvasEvent, Rect, WidgetCapabilities};

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
    fn capabilities(&self) -> WidgetCapabilities;
    fn draw(&mut self, _ctx: &mut DrawingContext<'_>, _local_bounds: Rect, _clipped: bool) -> Result<()> {
        Ok(())
    }
    fn handle_event(&mut self, _event: &CanvasEvent) -> Result<()> {
        Ok(())
    }
}

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

    fn capabilities(&self) -> WidgetCapabilities {
        WidgetCapabilities::new(false, false, false)
    }

    fn draw(&mut self, ctx: &mut DrawingContext<'_>, local_bounds: Rect, _clipped: bool) -> Result<()> {
        let size = Size::new(local_bounds.w, local_bounds.h);
        let brush = SolidColorBrush::new(Color::new(0x1E, 0x22, 0x2B, 0xFF));
        ctx.fill_rect(
            &brush,
            winio::prelude::Rect::new(Point::new(local_bounds.x, local_bounds.y), size),
        )?;
        Ok(())
    }
}

pub struct Button {
    tile: TileSlot,
}

impl Widget for Button {
    fn init(tile: TileSlot) -> Result<Self> {
        Ok(Self { tile })
    }

    fn bounds(&self) -> Rect {
        self.tile.bounds
    }

    fn capabilities(&self) -> WidgetCapabilities {
        WidgetCapabilities::new(true, true, false)
    }

    fn draw(&mut self, ctx: &mut DrawingContext<'_>, local_bounds: Rect, _clipped: bool) -> Result<()> {
        let size = Size::new(local_bounds.w, local_bounds.h);
        let brush = SolidColorBrush::new(Color::new(0x3A, 0x67, 0xB3, 0xFF));
        ctx.fill_rect(
            &brush,
            winio::prelude::Rect::new(Point::new(local_bounds.x, local_bounds.y), size),
        )?;
        Ok(())
    }
}

pub struct Banner {
    tile: TileSlot,
}

impl Widget for Banner {
    fn init(tile: TileSlot) -> Result<Self> {
        Ok(Self { tile })
    }

    fn bounds(&self) -> Rect {
        self.tile.bounds
    }

    fn capabilities(&self) -> WidgetCapabilities {
        WidgetCapabilities::new(true, false, true)
    }

    fn draw(&mut self, ctx: &mut DrawingContext<'_>, local_bounds: Rect, clipped: bool) -> Result<()> {
        let size = Size::new(local_bounds.w, local_bounds.h);
        let color = if clipped {
            Color::new(0x5A, 0x2E, 0x2E, 0xFF)
        } else {
            Color::new(0x2E, 0x5A, 0x43, 0xFF)
        };
        let brush = SolidColorBrush::new(color);
        ctx.fill_rect(
            &brush,
            winio::prelude::Rect::new(Point::new(local_bounds.x, local_bounds.y), size),
        )?;
        Ok(())
    }
}
