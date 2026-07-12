use super::codegen::ComponentTokens;
use proc_macro2::TokenStream;
use quote::quote;
pub(super) fn render(tokens: &ComponentTokens<'_>) -> TokenStream {
    let ComponentTokens {
        canvas_match_arms,
        comp_ident,
        ident,
        msg_ident,
        routed_ident,
        widget_ctor_arms,
        ..
    } = tokens;
    quote! {
        impl ::winio::prelude::Layoutable for #comp_ident {
            fn loc(&self) -> ::winio::prelude::Result<::winio::prelude::Point> {
                self.root.loc()
            }
            fn set_loc(&mut self, p: ::winio::prelude::Point) -> ::winio::prelude::Result<()> {
                self.root.set_loc(p)
            }
            fn size(&self) -> ::winio::prelude::Result<::winio::prelude::Size> {
                self.root.size().map(Self::shrink_size)
            }
            fn set_size(&mut self, s: ::winio::prelude::Size) -> ::winio::prelude::Result<()> {
                self.root.set_size(Self::expand_size(s))
            }
        }
        impl ::winio::prelude::Failable for #comp_ident {
            type Error = ::winio::prelude::Error;
        }
        impl #comp_ident {
            fn canvas_mut(&mut self, idx: usize) -> &mut ::winio::prelude::Child<::winio::widgets::Canvas> {
                match idx {
                    #(#canvas_match_arms)*
                    _ => unreachable!("widget_tree generated invalid canvas slot"),
                }
            }
            pub fn set_halign(&mut self, halign: ::winio::prelude::HAlign) {
                self.halign = halign;
            }
            pub fn set_valign(&mut self, valign: ::winio::prelude::VAlign) {
                self.valign = valign;
            }
            pub fn set_margin(&mut self, margin: ::winio::prelude::Margin) {
                self.margin = margin;
            }
            pub fn halign(&self) -> ::winio::prelude::HAlign {
                self.halign
            }
            pub fn valign(&self) -> ::winio::prelude::VAlign {
                self.valign
            }
            pub fn margin(&self) -> ::winio::prelude::Margin {
                self.margin
            }
            pub fn set_visible(&mut self, visible: bool) -> ::winio::prelude::Result<()> {
                self.root.set_visible(visible)
            }
            fn sync_widgets_routing(&mut self) {
                for (id, w) in &self.widgets {
                    let h = w.hoverable();
                    let c = w.clickable();
                    let s = w.scrollable();
                    let idx = id.0 as usize;
                    self.plan.widgets[idx].hoverable = h;
                    self.plan.widgets[idx].clickable = c;
                    self.plan.widgets[idx].scrollable = s;
                    self.widget_nodes[idx].hoverable = h;
                    self.widget_nodes[idx].clickable = c;
                    self.widget_nodes[idx].scrollable = s;
                }
            }
            fn sync_widgets_rendering(&mut self) -> ::griffr_gui::ui::DirtyFlags {
                let mut dirty = ::griffr_gui::ui::DirtyFlags::empty();
                for (id, w) in &self.widgets {
                    let o = w.opaque();
                    let s = w.scrollable();
                    let sz = w.sizing_policy();
                    let idx = id.0 as usize;
                    if self.widget_nodes[idx].opaque != o || self.widget_nodes[idx].scrollable != s {
                        dirty |= ::griffr_gui::ui::DirtyFlags::TILE_PLAN | ::griffr_gui::ui::DirtyFlags::PAINT;
                        self.plan.mark_widget_dirty(*id, ::griffr_gui::ui::DirtyFlags::TILE_PLAN | ::griffr_gui::ui::DirtyFlags::PAINT);
                    }
                    if self.widget_nodes[idx].layout.sizing != sz {
                        dirty |= ::griffr_gui::ui::DirtyFlags::LAYOUT
                            | ::griffr_gui::ui::DirtyFlags::TILE_PLAN
                            | ::griffr_gui::ui::DirtyFlags::PAINT;
                        self.plan.mark_widget_dirty(*id, ::griffr_gui::ui::DirtyFlags::LAYOUT | ::griffr_gui::ui::DirtyFlags::TILE_PLAN | ::griffr_gui::ui::DirtyFlags::PAINT);
                    }
                    self.widget_nodes[idx].opaque = o;
                    self.widget_nodes[idx].scrollable = s;
                    self.widget_nodes[idx].layout.sizing = sz;
                }
                dirty
            }
            fn mark_all_widgets_dirty(&mut self, dirty: ::griffr_gui::ui::DirtyFlags) {
                if dirty.is_empty() {
                    return;
                }
                for idx in 0..self.widget_nodes.len() {
                    self.plan.mark_widget_dirty(::griffr_gui::ui::WidgetId(idx as u16), dirty);
                }
            }
            fn build_widgets(plan: &::griffr_gui::ui::CompiledPlan) -> ::winio::prelude::Result<Vec<(::griffr_gui::ui::WidgetId, Box<dyn ::griffr_gui::ui::Widget>)>> {
                let mut out = Vec::<(::griffr_gui::ui::WidgetId, Box<dyn ::griffr_gui::ui::Widget>)>::new();
                for node in &plan.widgets {
                    let bounds = plan.bounds.get(node.id.0 as usize).copied().unwrap_or(::winio::primitive::Rect::from_size(::winio::prelude::Size::new(0.0, 0.0)));
                    let clipped = plan.tile_plan.tiles.iter().find(|tile| tile.widgets.iter().any(|id| *id == node.id)).map(|t| t.clipped).unwrap_or(false);
                    let slot = ::griffr_gui::ui::TileSlot { bounds, clipped, sizing: node.layout.sizing };
                    let widget: Box<dyn ::griffr_gui::ui::Widget> = match node.widget_type {
                        #(#widget_ctor_arms)*
                        _ => unreachable!("widget_tree generated unknown widget kind"),
                    };
                    out.push((node.id, widget));
                }
                Ok(out)
            }
            fn local_to_global(&self, idx: usize, p: ::winio::prelude::Point) -> ::winio::prelude::Point {
                self.canvas_pool
                    .tile_for_slot(idx)
                    .and_then(|tile_idx| self.plan.tile_plan.tiles.get(tile_idx))
                    .map(|t| ::winio::prelude::Point::new(p.x + t.bounds.origin.x, p.y + t.bounds.origin.y))
                    .unwrap_or(p)
            }
            fn expand_size(size: ::winio::prelude::Size) -> ::winio::prelude::Size {
                ::winio::prelude::Size::new(
                    size.width + ::griffr_gui::ui::CANVAS_OVERDRAW_PX,
                    size.height + ::griffr_gui::ui::CANVAS_OVERDRAW_PX,
                )
            }
            fn shrink_size(size: ::winio::prelude::Size) -> ::winio::prelude::Size {
                ::winio::prelude::Size::new(
                    size.width - ::griffr_gui::ui::CANVAS_OVERDRAW_PX,
                    size.height - ::griffr_gui::ui::CANVAS_OVERDRAW_PX,
                )
            }
            fn compile_plan(
                widget_nodes: &[::griffr_gui::ui::WidgetNode],
                size: ::winio::prelude::Size,
                old_plan: Option<&::griffr_gui::ui::CompiledPlan>,
                dirty: ::griffr_gui::ui::DirtyFlags,
            ) -> ::griffr_gui::ui::CompiledPlan {
                let widgets = widget_nodes.to_vec();
                let bounds = if let Some(old) = old_plan {
                    if !dirty.contains(::griffr_gui::ui::DirtyFlags::LAYOUT) && old.size == size {
                        old.bounds.clone()
                    } else {
                        ::griffr_gui::ui::layout::compute_layout(&widgets, size)
                    }
                } else {
                    ::griffr_gui::ui::layout::compute_layout(&widgets, size)
                };
                let tile_plan = if let Some(old) = old_plan {
                    let needs_tile_plan = dirty.intersects(
                        ::griffr_gui::ui::DirtyFlags::LAYOUT | ::griffr_gui::ui::DirtyFlags::TILE_PLAN
                    );
                    if !needs_tile_plan && old.can_reuse_tile_plan(&widgets, &bounds) {
                        old.tile_plan.clone()
                    } else {
                        let mut tiles = ::griffr_gui::ui::tile_plan::compile::partition_non_overlapping_tiles(&widgets, &bounds);
                        tiles = ::griffr_gui::ui::tile_plan::merge::merge_adjacent_non_clipped(tiles, &bounds, &widgets);
                        for (idx, t) in tiles.iter_mut().enumerate() {
                            t.id = ::griffr_gui::ui::TileId(idx as u16);
                        }
                        ::griffr_gui::ui::TilePlan { tiles }
                    }
                } else {
                    let mut tiles = ::griffr_gui::ui::tile_plan::compile::partition_non_overlapping_tiles(&widgets, &bounds);
                    tiles = ::griffr_gui::ui::tile_plan::merge::merge_adjacent_non_clipped(tiles, &bounds, &widgets);
                    for (idx, t) in tiles.iter_mut().enumerate() {
                        t.id = ::griffr_gui::ui::TileId(idx as u16);
                    }
                    ::griffr_gui::ui::TilePlan { tiles }
                };
                let num_widgets = widgets.len();
                ::griffr_gui::ui::CompiledPlan {
                    widgets,
                    bounds,
                    dirty: old_plan
                        .filter(|old| old.dirty.len() == num_widgets)
                        .map(|old| old.dirty.clone())
                        .unwrap_or_else(|| vec![::griffr_gui::ui::DirtyFlags::empty(); num_widgets].into_boxed_slice()),
                    tile_plan,
                    size,
                }
            }
            fn route_event(
                plan: &::griffr_gui::ui::CompiledPlan,
                event: #routed_ident,
            ) -> Option<::griffr_gui::ui::WidgetId> {
                match event {
                    #routed_ident::MouseMove { x, y } => {
                        for &id in #ident::Z_ORDER_FRONT_TO_BACK.iter() {
                            let bounds = &plan.bounds[id.0 as usize];
                            if bounds.contains(::winio::prelude::Point::new(x, y)) {
                                let node = &plan.widgets[id.0 as usize];
                                if node.hoverable {
                                    return Some(id);
                                }
                            }
                        }
                    }
                    #routed_ident::MouseDown { x, y } | #routed_ident::MouseUp { x, y } => {
                        for &id in #ident::Z_ORDER_FRONT_TO_BACK.iter() {
                            let bounds = &plan.bounds[id.0 as usize];
                            if bounds.contains(::winio::prelude::Point::new(x, y)) {
                                let node = &plan.widgets[id.0 as usize];
                                if node.clickable {
                                    return Some(id);
                                }
                            }
                        }
                    }
                    #routed_ident::MouseWheel { x, y } => {
                        for &id in #ident::Z_ORDER_FRONT_TO_BACK.iter() {
                            let bounds = &plan.bounds[id.0 as usize];
                            if bounds.contains(::winio::prelude::Point::new(x, y)) {
                                let node = &plan.widgets[id.0 as usize];
                                if node.scrollable {
                                    return Some(id);
                                }
                            }
                        }
                    }
                }
                None
            }
            fn map_canvas_event(
                event: &::winio::prelude::CanvasEvent,
                x: f64,
                y: f64,
            ) -> Option<#routed_ident> {
                match event {
                    ::winio::prelude::CanvasEvent::MouseMove(_) => Some(#routed_ident::MouseMove { x, y }),
                    ::winio::prelude::CanvasEvent::MouseDown(_) => Some(#routed_ident::MouseDown { x, y }),
                    ::winio::prelude::CanvasEvent::MouseUp(_) => Some(#routed_ident::MouseUp { x, y }),
                    ::winio::prelude::CanvasEvent::MouseWheel(_) => Some(#routed_ident::MouseWheel { x, y }),
                    _ => None,
                }
            }
            fn next_deadline(&self) -> Option<::std::time::Instant> {
                self.widgets.iter().filter_map(|(_, w)| w.next_redraw_at()).min()
            }
            fn post_deadline_tick(
                sender: ::winio::prelude::ComponentSender<Self>,
                seq: u64,
                deadline: ::std::time::Instant,
            ) {
                ::std::thread::spawn(move || {
                    let now = ::std::time::Instant::now();
                    if deadline > now {
                        ::std::thread::sleep(deadline.duration_since(now));
                    }
                    sender.post(#msg_ident::AnimationTick(seq, ::std::time::Instant::now()));
                });
            }
            fn reschedule_if_needed(&mut self, sender: &::winio::prelude::ComponentSender<Self>) {
                let next = self.next_deadline();
                if self.scheduled_tick.map(|(_, deadline)| deadline) == next {
                    return;
                }
                self.scheduled_tick = None;
                if let Some(deadline) = next {
                    self.next_tick_seq = self.next_tick_seq.wrapping_add(1);
                    let seq = self.next_tick_seq;
                    self.scheduled_tick = Some((seq, deadline));
                    Self::post_deadline_tick(sender.clone(), seq, deadline);
                }
            }
        }
    }
}
