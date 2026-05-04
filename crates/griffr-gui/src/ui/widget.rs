use winio::prelude::{DrawingContext, Result, Size};

use crate::ui::{CanvasEvent, Rect, WidgetCapabilities};

pub trait Widget {
    fn layout(&mut self, parent_size: Size) -> Rect;
    fn capabilities(&self) -> WidgetCapabilities;
    fn context(&mut self) -> Result<DrawingContext<'_>>;
    fn draw(&mut self, _ctx: &mut DrawingContext<'_>) -> Result<()> {
        Ok(())
    }
    fn handle_event(&mut self, _event: &CanvasEvent) -> Result<()> {
        Ok(())
    }
}
