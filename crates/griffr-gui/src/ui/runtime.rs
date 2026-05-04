use winio::prelude::Size;

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

    pub fn dispatch_with_pointer(&mut self, event: &CanvasEvent, x: f64, y: f64) -> Option<WidgetId> {
        let routed: Option<RoutedEvent> = map_canvas_event(event, x, y);
        self.hovered = routed.and_then(|e| route_event(&self.plan, e));
        self.hovered
    }
}
