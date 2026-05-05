use crate::ui::{TileSlot, Widget};
use winio::prelude::{CanvasEvent, Color, DrawingContext, Rect, Result, Size, SolidColorBrush};

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

    fn hoverable(&self) -> bool {
        true
    }
    fn scrollable(&self) -> bool {
        true
    }
    fn opaque(&self) -> bool {
        true
    }
    fn sizing_policy(&self) -> crate::ui::SizingPolicy {
        self.tile.sizing
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
        ctx.fill_rect(&brush, Rect::from_size(size))?;
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
