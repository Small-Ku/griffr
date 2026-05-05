use winio::prelude::Result;
use winio::primitive::{Rect, Size, Transform};
use winio::ui::DrawingContext;

pub fn draw_local<F>(ctx: &mut DrawingContext<'_>, bounds: Rect, mut draw: F) -> Result<()>
where
    F: FnMut(&mut DrawingContext<'_>, Size) -> Result<()>,
{
    let before = ctx.transform()?;
    ctx.set_transform(Transform::translation(bounds.origin.x, bounds.origin.y))?;
    let result = draw(ctx, bounds.size);
    ctx.set_transform(before)?;
    result
}

pub fn rect_at_origin(size: Size) -> winio::prelude::Rect {
    winio::prelude::Rect::from_size(size)
}
