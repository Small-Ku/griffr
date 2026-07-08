use proc_macro2::TokenStream;
use quote::quote;
use std::collections::BTreeSet;
use syn::{Ident, ItemStruct, Type};

use crate::expand::sim::merged_tile_count_for_flat;
use crate::model::FlatNode;

pub(crate) fn expand_widget_tree(root: ItemStruct, flat: Vec<FlatNode>) -> TokenStream {
    let ident = &root.ident;
    let comp_ident = Ident::new(&format!("{}Component", ident), ident.span());
    let msg_ident = Ident::new(&format!("{}ComponentMessage", ident), ident.span());
    let event_ident = Ident::new(&format!("{}ComponentEvent", ident), ident.span());
    let routed_ident = Ident::new(&format!("{}RoutedEvent", ident), ident.span());
    let canvas_count = merged_tile_count_for_flat(&flat);

    let widget_count = flat.len();
    let parents = flat.iter().map(|n| {
        let parent = n.parent;
        if parent >= 0 {
            quote! { Some(::griffr_gui::ui::WidgetId(#parent as u16)) }
        } else {
            quote! { None }
        }
    });

    let mut sorted_back_to_front = flat.clone();
    sorted_back_to_front.sort_by_key(|n| (n.z, n.id));
    let mut sorted_front_to_back = sorted_back_to_front.clone();
    sorted_front_to_back.reverse();

    let z_order_back_to_front = sorted_back_to_front.iter().map(|n| {
        let id = n.id;
        quote! { ::griffr_gui::ui::WidgetId(#id) }
    });
    let z_order_front_to_back = sorted_front_to_back.iter().map(|n| {
        let id = n.id;
        quote! { ::griffr_gui::ui::WidgetId(#id) }
    });
    let click_targets_front_to_back =
        sorted_front_to_back
            .iter()
            .filter(|n| n.clickable)
            .map(|n| {
                let id = n.id;
                quote! { ::griffr_gui::ui::WidgetId(#id) }
            });
    let hover_targets_front_to_back =
        sorted_front_to_back
            .iter()
            .filter(|n| n.hoverable)
            .map(|n| {
                let id = n.id;
                quote! { ::griffr_gui::ui::WidgetId(#id) }
            });
    let scroll_targets_front_to_back =
        sorted_front_to_back
            .iter()
            .filter(|n| n.scrollable)
            .map(|n| {
                let id = n.id;
                quote! { ::griffr_gui::ui::WidgetId(#id) }
            });
    let static_widgets = flat.iter().map(|n| {
        let id = n.id;
        let parent = n.parent;
        let hoverable = n.hoverable;
        let clickable = n.clickable;
        let scrollable = n.scrollable;
        let opaque = n.opaque;
        let clip = n.clip;
        let z = n.z;
        let kind = &n.kind;
        let direction = n.direction;
        let sizing_mode = n.sizing_mode;
        let sizing_f1 = n.sizing_f1;
        let sizing_f2 = n.sizing_f2;
        let sizing_f3 = n.sizing_f3;
        let margin = n.margin;
        let padding = n.padding;
        quote! {
            ::griffr_gui::ui::WidgetNode {
                id: ::griffr_gui::ui::WidgetId(#id),
                parent: (#parent >= 0).then_some(::griffr_gui::ui::WidgetId(#parent as u16)),
                hoverable: #hoverable,
                clickable: #clickable,
                scrollable: #scrollable,
                opaque: #opaque,
                clip: match #clip {
                    1 => ::griffr_gui::ui::ClipPolicy::ForceClip,
                    -1 => ::griffr_gui::ui::ClipPolicy::ForceNoClip,
                    _ => ::griffr_gui::ui::ClipPolicy::InferFromCapabilities,
                },
                layout: ::griffr_gui::ui::LayoutSpec {
                    direction: if #direction == 0 { ::griffr_gui::ui::LayoutDirection::Row } else { ::griffr_gui::ui::LayoutDirection::Column },
                    margin: #margin,
                    padding: #padding,
                    sizing: match #sizing_mode {
                        1 => ::griffr_gui::ui::SizingPolicy::AspectRatio(#sizing_f1),
                        2 => ::griffr_gui::ui::SizingPolicy::Fixed(::winio::prelude::Size::new(#sizing_f1, #sizing_f2)),
                        _ => ::griffr_gui::ui::SizingPolicy::Flex { grow: #sizing_f1, shrink: #sizing_f2, basis: #sizing_f3 },
                    },
                },
                z_order: #z,
                widget_type: #kind,
            }
        }
    });
    let canvas_fields = (0..canvas_count).map(|idx| {
        let field = Ident::new(&format!("tile{}", idx), ident.span());
        quote! { #field: ::winio::prelude::Child<::winio::widgets::Canvas>, }
    });
    let canvas_inits = (0..canvas_count).map(|idx| {
        let field = Ident::new(&format!("tile{}", idx), ident.span());
        quote! { #field: ::winio::widgets::Canvas = (&root), }
    });
    let canvas_struct_inits = (0..canvas_count).map(|idx| {
        let field = Ident::new(&format!("tile{}", idx), ident.span());
        quote! { #field, }
    });
    let start_arms = (0..canvas_count).map(|idx| {
        let idx = idx as usize;
        let field = Ident::new(&format!("tile{}", idx), ident.span());
        quote! { self.#field => { e => #msg_ident::Canvas(#idx, e), }, }
    });
    let update_children_items = (0..canvas_count).map(|idx| {
        let field = Ident::new(&format!("tile{}", idx), ident.span());
        quote! { self.#field, }
    });
    let canvas_match_arms: Vec<_> = (0..canvas_count)
        .map(|idx| {
            let field = Ident::new(&format!("tile{}", idx), ident.span());
            quote! { #idx => &mut self.#field, }
        })
        .collect();
    let widget_kinds: BTreeSet<String> = flat.iter().map(|n| n.kind.clone()).collect();
    let widget_ctor_arms = widget_kinds.iter().map(|kind| {
        let widget_ty: Type = if kind.contains("::") {
            syn::parse_str(kind)
                .unwrap_or_else(|_| panic!("widget_tree: invalid widget type path `{kind}`"))
        } else {
            let base = Ident::new(kind, ident.span());
            syn::parse_quote!(::griffr_gui::widget::#base)
        };
        quote! {
            #kind => Box::new(<#widget_ty as ::griffr_gui::ui::Widget>::init(slot)?),
        }
    });

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
            pub const CLICK_TARGETS_FRONT_TO_BACK: &'static [::griffr_gui::ui::WidgetId] = &[
                #(#click_targets_front_to_back),*
            ];
            pub const HOVER_TARGETS_FRONT_TO_BACK: &'static [::griffr_gui::ui::WidgetId] = &[
                #(#hover_targets_front_to_back),*
            ];
            pub const SCROLL_TARGETS_FRONT_TO_BACK: &'static [::griffr_gui::ui::WidgetId] = &[
                #(#scroll_targets_front_to_back),*
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
                const TILE_OVERLAP_PX: f64 = 0.5;
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
                    let placement = ::griffr_gui::ui::CanvasPlacement::from_tile_bounds(tile.bounds, TILE_OVERLAP_PX);
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
                const COMPONENT_OVERDRAW_PX: f64 = 0.5;
                ::winio::prelude::Size::new(size.width + COMPONENT_OVERDRAW_PX, size.height + COMPONENT_OVERDRAW_PX)
            }
            fn shrink_size(size: ::winio::prelude::Size) -> ::winio::prelude::Size {
                const COMPONENT_OVERDRAW_PX: f64 = 0.5;
                ::winio::prelude::Size::new(size.width - COMPONENT_OVERDRAW_PX, size.height - COMPONENT_OVERDRAW_PX)
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
                        for &id in #ident::HOVER_TARGETS_FRONT_TO_BACK {
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
                        for &id in #ident::CLICK_TARGETS_FRONT_TO_BACK {
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
                        for &id in #ident::SCROLL_TARGETS_FRONT_TO_BACK {
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
