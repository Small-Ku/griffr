use crate::ui::{DirtyFlags, DrawResources, TileSlot, Widget};
use winio::prelude::{CanvasEvent, Color, DrawingContext, Point, Rect, Result, Size};
use winio::primitive::DrawingFontBuilder;

pub struct CounterWidget {
    tile: TileSlot,
    hovered: bool,
    pressed: bool,
    click_count: u32,
}

impl Widget for CounterWidget {
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

    fn sizing_policy(&self) -> crate::ui::SizingPolicy {
        self.tile.sizing
    }

    fn hoverable(&self) -> bool {
        true
    }
    fn clickable(&self) -> bool {
        true
    }
    fn opaque(&self) -> bool {
        self.click_count % 2 == 1
    }

    fn draw(
        &mut self,
        ctx: &mut DrawingContext<'_>,
        resources: &mut DrawResources,
        size: Size,
        _clipped: bool,
    ) -> Result<()> {
        let mut alpha = 0xFF;
        if !self.click_count.is_multiple_of(2) {
            alpha = 0xAA; // Semi-transparent when "transparent"
        }

        let color = if self.pressed {
            Color::new(0x1F, 0x4B, 0x91, alpha)
        } else if self.hovered {
            Color::new(0x4B, 0x78, 0xC4, alpha)
        } else {
            Color::new(0x3A, 0x67, 0xB3, alpha)
        };
        let brush = resources.solid_brush(color);
        ctx.fill_round_rect(&brush, Rect::from_size(size), Size::new(12.0, 12.0))?;

        // Counter bars at the bottom
        for i in 0..(self.click_count % 10) {
            let bar_brush = resources.solid_brush(Color::new(255, 255, 255, 200));
            let bar_rect = Rect::new(
                Point::new(6.0 + i as f64 * 8.0, size.height - 10.0),
                Size::new(4.0, 4.0),
            );
            ctx.fill_rect(&bar_brush, bar_rect)?;
        }

        let pos = Point::new(6.0, 6.0);
        let font = resources.font(
            DrawingFontBuilder::new()
                .family("Segoe UI")
                .size(12.0)
                .build(),
        );

        let text_brush = resources.solid_brush(Color::new(255, 255, 255, 255));
        ctx.draw_str(
            &text_brush,
            font,
            pos,
            format!("Count: {}", self.click_count),
        )?;
        Ok(())
    }

    fn handle_event(&mut self, event: &CanvasEvent, is_target: bool) -> Result<DirtyFlags> {
        let before_hovered = self.hovered;
        let before_pressed = self.pressed;
        let before_click_count = self.click_count;
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
        let mut dirty = DirtyFlags::empty();
        if before_hovered != self.hovered || before_pressed != self.pressed {
            dirty |= DirtyFlags::PAINT;
        }
        if before_click_count != self.click_count {
            dirty |= DirtyFlags::PAINT | DirtyFlags::TILE_PLAN;
        }
        Ok(dirty)
    }
}
