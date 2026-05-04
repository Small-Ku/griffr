use winio::prelude::{BrushPen, DrawingFontBuilder, Point, Size, SolidColorBrush};

use crate::ui::container::{draw_local, rect_at_origin};
use crate::ui::dispatch::{map_canvas_event, route_event, RoutedEvent};
use crate::ui::tile_plan::compile::compile;
use crate::ui::{CanvasEvent, CompiledPlan, WidgetDecl, WidgetId};

pub struct UiRuntime {
    decls: &'static [WidgetDecl],
    pub plan: CompiledPlan,
    pub hovered: Option<WidgetId>,
}

impl UiRuntime {
    pub fn new(decls: &'static [WidgetDecl], size: Size) -> Self {
        Self {
            decls,
            plan: compile(decls, size),
            hovered: None,
        }
    }

    pub fn relayout(&mut self, size: Size) {
        self.plan = compile(self.decls, size);
    }

    pub fn dispatch_with_pointer(&mut self, event: CanvasEvent, x: f64, y: f64) -> Option<WidgetId> {
        let routed: Option<RoutedEvent> = map_canvas_event(event, x, y);
        self.hovered = routed.and_then(|e| route_event(&self.plan, e));
        self.hovered
    }

    pub fn draw_tile(&self, tile_idx: usize, ctx: &mut winio::prelude::DrawingContext<'_>) -> winio::prelude::Result<()> {
        let tile = &self.plan.tile_plan.tiles[tile_idx];
        let wid = tile.widgets.last().copied();
        let node = wid.and_then(|id| self.plan.widgets.iter().find(|w| w.id == id));
        let hovered = wid.is_some() && wid == self.hovered;
        let bg = if tile.clipped {
            winio::prelude::Color::new(0x22, 0x2A, 0x44, 0xFF)
        } else if hovered {
            winio::prelude::Color::new(0x6A, 0x3C, 0x1E, 0xFF)
        } else {
            winio::prelude::Color::new(0x24, 0x3E, 0x2A, 0xFF)
        };
        let brush = SolidColorBrush::new(bg);
        let fg = SolidColorBrush::new(winio::prelude::Color::new(0xF2, 0xF2, 0xF2, 0xFF));
        draw_local(ctx, crate::ui::Rect::new(0.0, 0.0, tile.bounds.w, tile.bounds.h), |c, s| {
            c.fill_rect(&brush, rect_at_origin(s))?;
            let pen = BrushPen::new(&fg, if hovered { 2.0 } else { 1.0 });
            c.draw_rect(&pen, rect_at_origin(s))?;
            let label = format!(
                "{} | z={} | clipped={} | hovered={}",
                node.map_or("Widget", |n| n.widget_type),
                node.map_or(0, |n| n.z_order),
                tile.clipped,
                hovered
            );
            let font = DrawingFontBuilder::new()
                .family("Consolas")
                .size(14.0)
                .build();
            c.draw_str(&fg, font, Point::new(10.0, 8.0), label)
        })?;
        Ok(())
    }
}
