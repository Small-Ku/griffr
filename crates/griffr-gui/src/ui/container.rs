use winio::prelude::{DrawingContext, Point, Result, Size, Transform};

use crate::ui::Rect;

pub fn draw_local<F>(ctx: &mut DrawingContext<'_>, bounds: Rect, mut draw: F) -> Result<()>
where
    F: FnMut(&mut DrawingContext<'_>, Size) -> Result<()>,
{
    let before = ctx.transform()?;
    ctx.set_transform(Transform::translation(bounds.x, bounds.y))?;
    let result = draw(ctx, Size::new(bounds.w, bounds.h));
    ctx.set_transform(before)?;
    result
}

pub fn rect_at_origin(size: Size) -> winio::prelude::Rect {
    winio::prelude::Rect::new(Point::new(0.0, 0.0), size)
}
