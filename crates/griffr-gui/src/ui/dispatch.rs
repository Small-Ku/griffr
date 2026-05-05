use winio::prelude::CanvasEvent;
use crate::ui::{CompiledPlan, WidgetId};

#[derive(Clone, Copy, Debug)]
pub enum RoutedEvent {
    MouseMove { x: f64, y: f64 },
    MouseDown { x: f64, y: f64 },
    MouseUp { x: f64, y: f64 },
    MouseWheel { x: f64, y: f64 },
}

pub fn route_event(plan: &CompiledPlan, event: RoutedEvent) -> Option<WidgetId> {
    let (x, y, predicate): (f64, f64, fn(bool, bool, bool) -> bool) = match event {
        RoutedEvent::MouseMove { x, y } => (x, y, |h, _, _| h),
        RoutedEvent::MouseDown { x, y } | RoutedEvent::MouseUp { x, y } => (x, y, |_, c, _| c),
        RoutedEvent::MouseWheel { x, y } => (x, y, |_, _, s| s),
    };
    let mut best: Option<(i32, WidgetId)> = None;
    for (id, bounds) in &plan.bounds {
        if !bounds.contains(x, y) {
            continue;
        }
        if let Some(node) = plan.widgets.iter().find(|n| n.id == *id) {
            let caps = node.capabilities;
            if predicate(caps.hoverable, caps.clickable, caps.scrollable) {
                match best {
                    Some((z, _)) if z >= node.z_order => {}
                    _ => best = Some((node.z_order, node.id)),
                }
            }
        }
    }
    best.map(|(_, id)| id)
}

pub fn map_canvas_event(event: &CanvasEvent, x: f64, y: f64) -> Option<RoutedEvent> {
    match event {
        CanvasEvent::MouseMove(_) => Some(RoutedEvent::MouseMove { x, y }),
        CanvasEvent::MouseDown(_) => Some(RoutedEvent::MouseDown { x, y }),
        CanvasEvent::MouseUp(_) => Some(RoutedEvent::MouseUp { x, y }),
        CanvasEvent::MouseWheel(_) => Some(RoutedEvent::MouseWheel { x, y }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use winio::prelude::Size;

    use crate::ui::tile_plan::compile::compile;
    use crate::ui::{route_event, RoutedEvent, WidgetDecl, WidgetId};

    #[test]
    fn topmost_clickable_wins() {
        let decls = &[
            WidgetDecl { id: 0, parent: -1, widget_type: "Button", hoverable: false, clickable: true, scrollable: false, clip: 0, z: 1, direction: 1, flex_grow: 0.0, flex_shrink: 1.0, flex_basis: 50.0, margin: 0.0, padding: 0.0 },
            WidgetDecl { id: 1, parent: -1, widget_type: "Button", hoverable: false, clickable: true, scrollable: false, clip: 0, z: 2, direction: 1, flex_grow: 0.0, flex_shrink: 1.0, flex_basis: 50.0, margin: 0.0, padding: 0.0 },
        ];
        let mut plan = compile(decls, Size::new(100.0, 100.0));
        let b0 = plan.bounds[0].1;
        if let Some((_, b)) = plan.bounds.get_mut(1) {
            *b = b0;
        }
        let hit = route_event(&plan, RoutedEvent::MouseDown { x: 10.0, y: 10.0 });
        assert_eq!(hit, Some(WidgetId(1)));
    }
}
