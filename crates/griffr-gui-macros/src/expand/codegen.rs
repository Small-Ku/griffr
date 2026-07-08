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
    let last_canvas_idx = canvas_count.saturating_sub(1);
    let last_canvas_field = Ident::new(&format!("tile{}", last_canvas_idx), ident.span());

    let topology = flat.iter().map(|n| {
        let id = n.id;
        let parent = n.parent;
        quote! { (#id, #parent) }
    });
    let capabilities = flat.iter().map(|n| {
        let id = n.id;
        let h = n.hoverable;
        let c = n.clickable;
        let s = n.scrollable;
        let o = n.opaque;
        quote! { (#id, #h, #c, #s, #o) }
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
    let render_match_arms = (0..last_canvas_idx).map(|idx| {
        let field = Ident::new(&format!("tile{}", idx), ident.span());
        quote! { #idx => &mut self.#field, }
    });
    let render_children_stmts = (0..canvas_count).map(|idx| {
        let field = Ident::new(&format!("tile{}", idx), ident.span());
        quote! { self.#field.render()?; }
    });
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
            pub const TOPOLOGY: &'static [(u16, i16)] = &[#(#topology),*];
            pub const CAPABILITIES: &'static [(u16, bool, bool, bool, bool)] = &[#(#capabilities),*];
            pub const CANVAS_COUNT: usize = #canvas_count;
            pub fn initial_widget_nodes() -> Vec<::griffr_gui::ui::WidgetNode> {
                let mut widgets = vec![#(#static_widgets),*];
                widgets.sort_by_key(|w| (w.z_order, w.id));
                widgets
            }
        }

        #[derive(Debug)]
        pub enum #event_ident { Redraw, Target(Option<::griffr_gui::ui::WidgetId>), }

        pub struct #comp_ident {
            root: ::winio::prelude::Child<::winio::widgets::View>,
            #(#canvas_fields)*
            widget_nodes: Vec<::griffr_gui::ui::WidgetNode>,
            plan: ::griffr_gui::ui::CompiledPlan,
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
                let mut plan = Self::compile_plan(&widget_nodes, size);
                let widgets = Self::build_widgets(&plan)?;
                for (id, w) in &widgets {
                    if let Some(node) = widget_nodes.iter_mut().find(|n| n.id == *id) {
                        node.hoverable = w.hoverable();
                        node.clickable = w.clickable();
                        node.scrollable = w.scrollable();
                        node.opaque = w.opaque();
                    }
                }
                plan = Self::compile_plan(&widget_nodes, size);
                let mut this = Self {
                    root, #(#canvas_struct_inits)* widgets, widget_nodes, plan, hovered: None,
                    pointers: [::winio::prelude::Point::new(0.0, 0.0); #canvas_count],
                    next_tick_seq: 0,
                    scheduled_tick: None,
                    halign: ::winio::prelude::HAlign::Stretch,
                    valign: ::winio::prelude::VAlign::Stretch,
                    margin: ::winio::prelude::Margin::default(),
                };
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
                    #msg_ident::Layout(rect) => { self.root.set_loc(rect.origin)?; self.root.set_size(Self::expand_size(rect.size))?; Ok(true) }
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
                        self.sync_widgets_routing(); // Sync before dispatch
                        let routed = Self::map_canvas_event(&ev, p.x, p.y);
                        self.hovered = routed.and_then(|e| Self::route_event(&self.plan, e));
                        let hit = self.hovered;
                        for (id, widget) in &mut self.widgets { widget.handle_event(&ev, hit.is_some_and(|hit_id| hit_id == *id))?; }
                        self.reschedule_if_needed(sender);
                        sender.output(#event_ident::Target(hit));
                        sender.output(#event_ident::Redraw);
                        Ok(true)
                    }
                    #msg_ident::AnimationTick(seq, now) => {
                        if self.scheduled_tick.map(|(scheduled_seq, _)| scheduled_seq != seq).unwrap_or(true) {
                            return Ok(false);
                        }
                        self.scheduled_tick = None;
                        let mut needs_redraw = false;
                        for (_, widget) in &mut self.widgets {
                            needs_redraw |= widget.on_animation_frame(now);
                        }
                        self.reschedule_if_needed(sender);
                        if needs_redraw {
                            sender.output(#event_ident::Redraw);
                        }
                        Ok(needs_redraw)
                    }
                }
            }
            fn render(&mut self, _sender: &::winio::prelude::ComponentSender<Self>) -> ::winio::prelude::Result<()> {
                const TILE_OVERLAP_PX: f64 = 0.5;
                let size = self.root.size()?;
                self.sync_widgets_rendering(); // Sync before relayout
                self.plan = Self::compile_plan(&self.widget_nodes, size);
                for idx in 0..#canvas_count {
                    let canvas = match idx { #(#render_match_arms)* _ => &mut self.#last_canvas_field, };
                    if let Some(tile) = self.plan.tile_plan.tiles.get(idx) {
                        let draw_w = tile.bounds.size.width + TILE_OVERLAP_PX;
                        let draw_h = tile.bounds.size.height + TILE_OVERLAP_PX;
                        canvas.set_visible(true)?;
                        canvas.set_loc(::winio::prelude::Point::new(tile.bounds.origin.x, tile.bounds.origin.y))?;
                        canvas.set_size(::winio::prelude::Size::new(draw_w, draw_h))?;
                        if !tile.widgets.is_empty() {
                            let mut ctx = canvas.context()?;
                            for &id in &tile.widgets {
                                if let Some((_, widget)) = self.widgets.iter_mut().find(|(w_id, _)| *w_id == id) {
                                    let widget_bounds = &self.plan.bounds[id.0 as usize];
                                    let tx = widget_bounds.origin.x - tile.bounds.origin.x;
                                    let ty = widget_bounds.origin.y - tile.bounds.origin.y;
                                    ctx.set_transform(::winio::prelude::Transform::translation(tx, ty))?;
                                    widget.draw(&mut ctx, ::winio::prelude::Size::new(widget_bounds.size.width, widget_bounds.size.height), tile.clipped)?;
                                }
                            }
                        }
                    } else {
                        canvas.set_visible(false)?;
                    }
                }
                self.reschedule_if_needed(_sender);
                Ok(())
            }
            fn render_children(&mut self) -> ::winio::prelude::Result<()> { #(#render_children_stmts)* self.root.render() }
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
                    if let Some(node) = self.plan.widgets.iter_mut().find(|n| n.id == *id) {
                        node.hoverable = h;
                        node.clickable = c;
                        node.scrollable = s;
                    }
                    if let Some(node) = self.widget_nodes.iter_mut().find(|n| n.id == *id) {
                        node.hoverable = h;
                        node.clickable = c;
                        node.scrollable = s;
                    }
                }
            }
            fn sync_widgets_rendering(&mut self) {
                for (id, w) in &self.widgets {
                    let o = w.opaque();
                    let s = w.scrollable();
                    let sz = w.sizing_policy();
                    if let Some(node) = self.widget_nodes.iter_mut().find(|n| n.id == *id) {
                        node.opaque = o;
                        node.scrollable = s;
                        node.layout.sizing = sz;
                    }
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
                self.plan.tile_plan.tiles.get(idx).map(|t| ::winio::prelude::Point::new(p.x + t.bounds.origin.x, p.y + t.bounds.origin.y)).unwrap_or(p)
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
            ) -> ::griffr_gui::ui::CompiledPlan {
                let widgets = widget_nodes.to_vec();
                let bounds = ::griffr_gui::ui::layout::compute_layout(&widgets, size);
                let mut tiles = ::griffr_gui::ui::tile_plan::compile::partition_non_overlapping_tiles(&widgets, &bounds);
                tiles = ::griffr_gui::ui::tile_plan::merge::merge_adjacent_non_clipped(tiles, &bounds, &widgets);
                for (idx, t) in tiles.iter_mut().enumerate() {
                    t.id = ::griffr_gui::ui::TileId(idx as u16);
                }
                let num_widgets = widgets.len();
                ::griffr_gui::ui::CompiledPlan {
                    widgets,
                    bounds,
                    dirty: vec![false; num_widgets].into_boxed_slice(),
                    tile_plan: ::griffr_gui::ui::TilePlan { tiles },
                    size,
                }
            }
            fn route_event(
                plan: &::griffr_gui::ui::CompiledPlan,
                event: #routed_ident,
            ) -> Option<::griffr_gui::ui::WidgetId> {
                let (x, y, predicate): (f64, f64, fn(bool, bool, bool) -> bool) = match event {
                    #routed_ident::MouseMove { x, y } => (x, y, |h, _, _| h),
                    #routed_ident::MouseDown { x, y } | #routed_ident::MouseUp { x, y } => (x, y, |_, c, _| c),
                    #routed_ident::MouseWheel { x, y } => (x, y, |_, _, s| s),
                };
                let mut best: Option<(i32, ::griffr_gui::ui::WidgetId)> = None;
                for node in &plan.widgets {
                    let bounds = &plan.bounds[node.id.0 as usize];
                    if !bounds.contains(::winio::prelude::Point::new(x, y)) {
                        continue;
                    }
                    if predicate(node.hoverable, node.clickable, node.scrollable) {
                        match best {
                            Some((z, _)) if z >= node.z_order => {}
                            _ => best = Some((node.z_order, node.id)),
                        }
                    }
                }
                best.map(|(_, id)| id)
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
