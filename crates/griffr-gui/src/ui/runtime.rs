use winio::prelude::Size;

use crate::ui::dispatch::{map_canvas_event, route_event, RoutedEvent};
use crate::ui::tile_plan::compile::compile_dynamic;
use crate::ui::{CanvasEvent, CompiledPlan, StaticPlan, WidgetId};

pub struct UiRuntime {
    static_plan: StaticPlan,
    pub plan: CompiledPlan,
    pub hovered: Option<WidgetId>,
}

impl UiRuntime {
    pub fn from_static(static_plan: StaticPlan, size: Size) -> Self {
        Self {
            plan: compile_dynamic(&static_plan, size),
            static_plan,
            hovered: None,
        }
    }

    pub fn relayout(&mut self, size: Size) {
        self.plan = compile_dynamic(&self.static_plan, size);
    }

    pub fn canvas_count(&self) -> usize {
        self.static_plan.merged_tile_count.max(1)
    }

    pub fn dispatch_with_pointer(&mut self, event: &CanvasEvent, x: f64, y: f64) -> Option<WidgetId> {
        let routed: Option<RoutedEvent> = map_canvas_event(event, x, y);
        self.hovered = routed.and_then(|e| route_event(&self.plan, e));
        self.hovered
    }
}
