use crate::ui::{DirtyFlags, TileSlot, Widget};
use winio::prelude::{CanvasEvent, Color, DrawingContext, Rect, Result, Size, SolidColorBrush};

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

    fn hoverable(&self) -> bool {
        true
    }
    fn clickable(&self) -> bool {
        true
    }
    fn opaque(&self) -> bool {
        true
    }
    fn sizing_policy(&self) -> crate::ui::SizingPolicy {
        self.tile.sizing
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
        ctx.fill_rect(&brush, Rect::from_size(size))?;
        Ok(())
    }

    fn handle_event(&mut self, event: &CanvasEvent, is_target: bool) -> Result<DirtyFlags> {
        let before_hovered = self.hovered;
        let before_pressed = self.pressed;
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
        Ok(
            ((before_hovered != self.hovered) || (before_pressed != self.pressed))
                .then_some(DirtyFlags::PAINT)
                .unwrap_or_else(DirtyFlags::empty),
        )
    }
}
