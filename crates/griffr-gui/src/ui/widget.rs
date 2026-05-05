use winio::prelude::Result;
use winio::primitive::{Color, Rect, Size, SolidColorBrush};
use winio::ui::DrawingContext;
use winio::widgets::CanvasEvent;

use crate::ui::WidgetCapabilities;

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
    fn draw(&mut self, _ctx: &mut DrawingContext<'_>, _size: Size, _clipped: bool) -> Result<()> {
        Ok(())
    }
    fn handle_event(&mut self, _event: &CanvasEvent, _is_target: bool) -> Result<()> {
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

    fn draw(&mut self, ctx: &mut DrawingContext<'_>, size: Size, _clipped: bool) -> Result<()> {
        let brush = SolidColorBrush::new(Color::new(0x1E, 0x22, 0x2B, 0xFF));
        ctx.fill_rect(&brush, winio::prelude::Rect::from_size(size))?;
        Ok(())
    }
}

pub struct Button {
    tile: TileSlot,
    hovered: bool,
    pressed: bool,
    click_count: u32,
}

impl Widget for Button {
    fn init(tile: TileSlot) -> Result<Self> {
        Ok(Self {
            tile,
            hovered: false,
            pressed: false,
            click_count: 0,
        })
    }

    fn bounds(&self) -> Rect {
        self.tile.bounds
    }

    fn capabilities(&self) -> WidgetCapabilities {
        WidgetCapabilities::new(true, true, false)
    }

    fn draw(&mut self, ctx: &mut DrawingContext<'_>, size: Size, _clipped: bool) -> Result<()> {
        let color = if self.pressed {
            Color::new(0x1F, 0x4B, 0x91, 0xFF)
        } else if self.hovered {
            Color::new(0x4B, 0x78, 0xC4, 0xFF)
        } else {
            Color::new(0x3A, 0x67, 0xB3, 0xFF)
        };
        let brush = SolidColorBrush::new(color);
        ctx.fill_rect(&brush, winio::prelude::Rect::from_size(size))?;
        Ok(())
    }

    fn handle_event(&mut self, event: &CanvasEvent, is_target: bool) -> Result<()> {
        match event {
            CanvasEvent::MouseMove(_) => {
                self.hovered = is_target;
                if !is_target {
                    self.pressed = false;
                }
            }
            CanvasEvent::MouseDown(_) => {
                self.pressed = is_target;
            }
            CanvasEvent::MouseUp(_) => {
                if is_target && self.pressed {
                    self.click_count = self.click_count.saturating_add(1);
                }
                self.pressed = false;
            }
            _ => {}
        }
        Ok(())
    }
}

pub struct Banner {
    tile: TileSlot,
    hovered: bool,
    h: f32,
    s: f32,
    v: f32,
}

impl Widget for Banner {
    fn init(tile: TileSlot) -> Result<Self> {
        Ok(Self {
            tile,
            hovered: false,
            h: 0.0,
            s: 44.0 / 90.0,
            v: 90.0 / 255.0,
        })
    }

    fn bounds(&self) -> Rect {
        self.tile.bounds
    }

    fn capabilities(&self) -> WidgetCapabilities {
        WidgetCapabilities::new(true, false, true)
    }

    fn draw(&mut self, ctx: &mut DrawingContext<'_>, size: Size, _clipped: bool) -> Result<()> {
        let mut current_v = self.v;
        if self.hovered {
            current_v = (current_v + 0.1).min(1.0);
        }

        let c = self.s * current_v;
        let x = c * (1.0 - ((self.h / 60.0) % 2.0 - 1.0).abs());
        let m = current_v - c;

        let (r1, g1, b1) = if self.h < 60.0 {
            (c, x, 0.0)
        } else if self.h < 120.0 {
            (x, c, 0.0)
        } else if self.h < 180.0 {
            (0.0, c, x)
        } else if self.h < 240.0 {
            (0.0, x, c)
        } else if self.h < 300.0 {
            (x, 0.0, c)
        } else {
            (c, 0.0, x)
        };

        let r = ((r1 + m) * 255.0).round() as u8;
        let g = ((g1 + m) * 255.0).round() as u8;
        let b = ((b1 + m) * 255.0).round() as u8;

        let brush = SolidColorBrush::new(Color::new(r, g, b, 0xFF));
        ctx.fill_rect(&brush, winio::prelude::Rect::from_size(size))?;
        Ok(())
    }

    fn handle_event(&mut self, event: &CanvasEvent, is_target: bool) -> Result<()> {
        match event {
            CanvasEvent::MouseMove(_) => {
                self.hovered = is_target;
            }
            CanvasEvent::MouseWheel(_) => {
                if is_target {
                    self.h = (self.h + 15.0) % 360.0;
                }
            }
            _ => {}
        }
        Ok(())
    }
}
