use super::{codegen::ComponentTokens, component_runtime};
use proc_macro2::TokenStream;
use quote::quote;

pub(super) fn render(tokens: ComponentTokens<'_>) -> TokenStream {
    let definitions = render_definitions(&tokens);
    let runtime = component_runtime::render(&tokens);
    quote! { #definitions #runtime }
}

fn render_definitions(tokens: &ComponentTokens<'_>) -> TokenStream {
    let ComponentTokens {
        canvas_count,
        canvas_fields,
        canvas_inits,
        canvas_match_arms,
        canvas_struct_inits,
        comp_ident,
        event_ident,
        ident,
        msg_ident,
        parents,
        root,
        routed_ident,
        start_arms,
        static_widgets,
        update_children_items,
        widget_count,
        z_order_back_to_front,
        z_order_front_to_back,
        ..
    } = tokens;
    quote! {
        #root
        impl #ident {
            pub const WIDGET_COUNT: usize = #widget_count;
            pub const PARENTS: [Option<::griffr_gui::ui::WidgetId>; Self::WIDGET_COUNT] = [
                #(#parents),*
            ];
            pub const Z_ORDER_BACK_TO_FRONT: [::griffr_gui::ui::WidgetId; Self::WIDGET_COUNT] = [
                #(#z_order_back_to_front),*
            ];
            pub const Z_ORDER_FRONT_TO_BACK: [::griffr_gui::ui::WidgetId; Self::WIDGET_COUNT] = [
                #(#z_order_front_to_back),*
            ];
            pub const CANVAS_COUNT: usize = #canvas_count;
            pub fn initial_widget_nodes() -> Vec<::griffr_gui::ui::WidgetNode> {
                vec![#(#static_widgets),*]
            }
        }
        #[derive(Debug)]
        pub enum #event_ident { Redraw, Target(Option<::griffr_gui::ui::WidgetId>), }
        pub struct #comp_ident {
            root: ::winio::prelude::Child<::winio::widgets::View>,
            #(#canvas_fields)*
            widget_nodes: Vec<::griffr_gui::ui::WidgetNode>,
            plan: ::griffr_gui::ui::CompiledPlan,
            canvas_pool: ::griffr_gui::ui::CanvasPool,
            draw_resources: ::griffr_gui::ui::DrawResources,
            pending_dirty: ::griffr_gui::ui::DirtyFlags,
            hovered: Option<::griffr_gui::ui::WidgetId>,
            widgets: Vec<(::griffr_gui::ui::WidgetId, Box<dyn ::griffr_gui::ui::Widget>)>,
            pointers: [::winio::prelude::Point; #canvas_count],
            next_tick_seq: u64,
            scheduled_tick: Option<(u64, ::std::time::Instant)>,
            halign: ::winio::prelude::HAlign,
            valign: ::winio::prelude::VAlign,
            margin: ::winio::prelude::Margin,
        }
        #[derive(Debug)]
        pub enum #msg_ident {
            Noop,
            Layout(::winio::prelude::Rect),
            Canvas(usize, ::winio::prelude::CanvasEvent),
            AnimationTick(u64, ::std::time::Instant),
        }
        #[derive(Clone, Copy, Debug)]
        #[allow(clippy::enum_variant_names)]
        enum #routed_ident {
            MouseMove { x: f64, y: f64 },
            MouseDown { x: f64, y: f64 },
            MouseUp { x: f64, y: f64 },
            MouseWheel { x: f64, y: f64 },
        }
        impl ::winio::prelude::Component for #comp_ident {
            type Error = ::winio::prelude::Error;
            type Event = #event_ident;
            type Init<'a> = &'a ::winio::prelude::Child<::winio::widgets::Window>;
            type Message = #msg_ident;
            async fn init(init: Self::Init<'_>, _sender: &::winio::prelude::ComponentSender<Self>) -> ::winio::prelude::Result<Self> {
                let mut widget_nodes = #ident::initial_widget_nodes();
                ::winio::prelude::init! { root: ::winio::widgets::View = (init), #(#canvas_inits)* }
                let size = init.client_size()?;
                root.set_loc(::winio::prelude::Point::new(0.0, 0.0))?;
                root.set_size(Self::expand_size(size))?;
                let mut plan = Self::compile_plan(&widget_nodes, size, None, ::griffr_gui::ui::DirtyFlags::LAYOUT | ::griffr_gui::ui::DirtyFlags::TILE_PLAN | ::griffr_gui::ui::DirtyFlags::PAINT);
                let widgets = Self::build_widgets(&plan)?;
                for (id, w) in &widgets {
                    if let Some(node) = widget_nodes.iter_mut().find(|n| n.id == *id) {
                        node.hoverable = w.hoverable();
                        node.clickable = w.clickable();
                        node.scrollable = w.scrollable();
                        node.opaque = w.opaque();
                    }
                }
                plan = Self::compile_plan(&widget_nodes, size, Some(&plan), ::griffr_gui::ui::DirtyFlags::TILE_PLAN | ::griffr_gui::ui::DirtyFlags::PAINT);
                let mut this = Self {
                    root,
                    #(#canvas_struct_inits)*
                    widgets,
                    widget_nodes,
                    plan,
                    canvas_pool: ::griffr_gui::ui::CanvasPool::new(#canvas_count),
                    draw_resources: ::griffr_gui::ui::DrawResources::default(),
                    pending_dirty: ::griffr_gui::ui::DirtyFlags::empty(),
                    hovered: None,
                    pointers: [::winio::prelude::Point::new(0.0, 0.0); #canvas_count],
                    next_tick_seq: 0,
                    scheduled_tick: None,
                    halign: ::winio::prelude::HAlign::Stretch,
                    valign: ::winio::prelude::VAlign::Stretch,
                    margin: ::winio::prelude::Margin::default(),
                };
                this.canvas_pool.prepare_frame(this.plan.tile_plan.tiles.len());
                for idx in 0..#canvas_count {
                    Self::canvas_mut(&mut this, idx).set_visible(false)?;
                }
                this.reschedule_if_needed(_sender);
                Ok(this)
            }
            async fn start(&mut self, sender: &::winio::prelude::ComponentSender<Self>) -> ! {
                ::winio::prelude::start! { sender, default: #msg_ident::Noop, #(#start_arms)* }
            }
            async fn update_children(&mut self) -> ::winio::prelude::Result<bool> {
                ::winio::prelude::update_children!(self.root, #(#update_children_items)*)
            }
            async fn update(&mut self, message: Self::Message, sender: &::winio::prelude::ComponentSender<Self>) -> ::winio::prelude::Result<bool> {
                match message {
                    #msg_ident::Noop => Ok(false),
                    #msg_ident::Layout(rect) => {
                        self.root.set_loc(rect.origin)?;
                        self.root.set_size(Self::expand_size(rect.size))?;
                        self.pending_dirty |= ::griffr_gui::ui::DirtyFlags::LAYOUT
                            | ::griffr_gui::ui::DirtyFlags::TILE_PLAN
                            | ::griffr_gui::ui::DirtyFlags::PAINT;
                        self.mark_all_widgets_dirty(self.pending_dirty);
                        Ok(true)
                    }
                    #msg_ident::Canvas(idx, ev) => {
                        let p_local = match &ev {
                            ::winio::prelude::CanvasEvent::MouseMove(p) => Some(*p),
                            _ => None,
                        };
                        if let Some(p) = p_local {
                            if idx < self.pointers.len() {
                                self.pointers[idx] = self.local_to_global(idx, p);
                            }
                        }
                        let p = self.pointers.get(idx).copied().unwrap_or(::winio::prelude::Point::new(0.0, 0.0));
                        self.sync_widgets_routing();
                        let routed = Self::map_canvas_event(&ev, p.x, p.y);
                        self.hovered = routed.and_then(|e| Self::route_event(&self.plan, e));
                        let hit = self.hovered;
                        let mut dirty = ::griffr_gui::ui::DirtyFlags::empty();
                        for (id, widget) in &mut self.widgets {
                            let widget_dirty = widget.handle_event(&ev, hit.is_some_and(|hit_id| hit_id == *id))?;
                            self.plan.mark_widget_dirty(*id, widget_dirty);
                            dirty |= widget_dirty;
                        }
                        self.pending_dirty |= dirty;
                        self.reschedule_if_needed(sender);
                        sender.output(#event_ident::Target(hit));
                        if !dirty.is_empty() {
                            sender.output(#event_ident::Redraw);
                        }
                        Ok(!dirty.is_empty())
                    }
                    #msg_ident::AnimationTick(seq, now) => {
                        if self.scheduled_tick.map(|(scheduled_seq, _)| scheduled_seq != seq).unwrap_or(true) {
                            return Ok(false);
                        }
                        self.scheduled_tick = None;
                        let mut dirty = ::griffr_gui::ui::DirtyFlags::empty();
                        for (id, widget) in &mut self.widgets {
                            let widget_dirty = widget.on_animation_frame(now);
                            self.plan.mark_widget_dirty(*id, widget_dirty);
                            dirty |= widget_dirty;
                        }
                        self.pending_dirty |= dirty;
                        self.reschedule_if_needed(sender);
                        if !dirty.is_empty() {
                            sender.output(#event_ident::Redraw);
                        }
                        Ok(!dirty.is_empty())
                    }
                }
            }
            fn render(&mut self, _sender: &::winio::prelude::ComponentSender<Self>) -> ::winio::prelude::Result<()> {
                let size = self.root.size()?;
                let mut dirty = self.pending_dirty | self.plan.dirty_summary();
                if self.plan.size != size {
                    dirty |= ::griffr_gui::ui::DirtyFlags::LAYOUT
                        | ::griffr_gui::ui::DirtyFlags::TILE_PLAN
                        | ::griffr_gui::ui::DirtyFlags::PAINT;
                    self.mark_all_widgets_dirty(dirty);
                }
                dirty |= self.sync_widgets_rendering();
                self.pending_dirty |= dirty;
                if self.pending_dirty.contains(::griffr_gui::ui::DirtyFlags::RESOURCES) {
                    self.draw_resources.clear();
                }
                self.plan = Self::compile_plan(&self.widget_nodes, size, Some(&self.plan), self.pending_dirty);
                self.canvas_pool.prepare_frame(self.plan.tile_plan.tiles.len());
                let released_slots = self.canvas_pool.drain_released_slots().collect::<Vec<_>>();
                for slot_idx in released_slots {
                    if self.canvas_pool.release_slot(slot_idx).hide {
                        Self::canvas_mut(self, slot_idx).set_visible(false)?;
                    }
                }
                for tile_idx in 0..self.plan.tile_plan.tiles.len() {
                    let slot_idx = self.canvas_pool.slot_for_tile(tile_idx);
                    let tile = &self.plan.tile_plan.tiles[tile_idx];
                    let placement = ::griffr_gui::ui::CanvasPlacement::from_tile_bounds(
                        tile.bounds,
                        ::griffr_gui::ui::CANVAS_OVERDRAW_PX,
                    );
                    let slot_update = self.canvas_pool.apply_placement(slot_idx, placement);
                    let canvas = match slot_idx {
                        #(#canvas_match_arms)*
                        _ => unreachable!("widget_tree generated invalid canvas slot"),
                    };
                    if slot_update.show {
                        canvas.set_visible(true)?;
                    }
                    if slot_update.move_or_resize {
                        canvas.set_loc(placement.loc)?;
                        canvas.set_size(placement.size)?;
                    }
                    if !tile.widgets.is_empty() {
                        let mut ctx = canvas.context()?;
                        for &id in &tile.widgets {
                            if let Some((_, widget)) = self.widgets.iter_mut().find(|(w_id, _)| *w_id == id) {
                                let widget_bounds = &self.plan.bounds[id.0 as usize];
                                let tx = widget_bounds.origin.x - tile.bounds.origin.x;
                                let ty = widget_bounds.origin.y - tile.bounds.origin.y;
                                ctx.set_transform(::winio::prelude::Transform::translation(tx, ty))?;
                                widget.draw(
                                    &mut ctx,
                                    &mut self.draw_resources,
                                    ::winio::prelude::Size::new(widget_bounds.size.width, widget_bounds.size.height),
                                    tile.clipped,
                                )?;
                            }
                        }
                    }
                }
                self.plan.clear_dirty();
                self.pending_dirty = ::griffr_gui::ui::DirtyFlags::empty();
                self.reschedule_if_needed(_sender);
                Ok(())
            }
            fn render_children(&mut self) -> ::winio::prelude::Result<()> {
                for tile_idx in 0..self.canvas_pool.active_count() {
                    let slot_idx = self.canvas_pool.slot_for_tile(tile_idx);
                    Self::canvas_mut(self, slot_idx).render()?;
                }
                self.root.render()
            }
        }
    }
}
