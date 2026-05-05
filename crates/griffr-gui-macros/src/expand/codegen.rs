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
    let canvas_count = merged_tile_count_for_flat(&flat);
    let last_canvas_idx = canvas_count.saturating_sub(1);
    let last_canvas_field = Ident::new(&format!("tile{}", last_canvas_idx), ident.span());

    let decls = flat.iter().map(|n| {
        let id = n.id;
        let parent = n.parent;
        let widget_type = &n.kind;
        let hoverable = n.hoverable;
        let clickable = n.clickable;
        let scrollable = n.scrollable;
        let clip = n.clip;
        let z = n.z;
        let direction = n.direction;
        let flex_grow = n.flex_grow;
        let flex_shrink = n.flex_shrink;
        let flex_basis = n.flex_basis;
        let margin = n.margin;
        let padding = n.padding;
        quote! {
            ::griffr_gui::ui::WidgetDecl {
                id: #id, parent: #parent, widget_type: #widget_type,
                hoverable: #hoverable, clickable: #clickable, scrollable: #scrollable,
                clip: #clip, z: #z, direction: #direction, flex_grow: #flex_grow,
                flex_shrink: #flex_shrink, flex_basis: #flex_basis, margin: #margin, padding: #padding,
            }
        }
    });
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
        quote! { (#id, #h, #c, #s) }
    });
    let static_widgets = flat.iter().map(|n| {
        let id = n.id;
        let parent = n.parent;
        let hoverable = n.hoverable;
        let clickable = n.clickable;
        let scrollable = n.scrollable;
        let clip = n.clip;
        let z = n.z;
        let kind = &n.kind;
        let direction = n.direction;
        let flex_grow = n.flex_grow;
        let flex_shrink = n.flex_shrink;
        let flex_basis = n.flex_basis;
        let margin = n.margin;
        let padding = n.padding;
        quote! {
            ::griffr_gui::ui::WidgetNode {
                id: ::griffr_gui::ui::WidgetId(#id),
                parent: (#parent >= 0).then_some(::griffr_gui::ui::WidgetId(#parent as u16)),
                capabilities: ::griffr_gui::ui::WidgetCapabilities::new(#hoverable, #clickable, #scrollable),
                clip: match #clip {
                    1 => ::griffr_gui::ui::ClipPolicy::ForceClip,
                    -1 => ::griffr_gui::ui::ClipPolicy::ForceNoClip,
                    _ => ::griffr_gui::ui::ClipPolicy::InferFromCapabilities,
                },
                layout: ::griffr_gui::ui::LayoutSpec {
                    direction: if #direction == 0 { ::griffr_gui::ui::LayoutDirection::Row } else { ::griffr_gui::ui::LayoutDirection::Column },
                    flex_grow: #flex_grow, flex_shrink: #flex_shrink, flex_basis: #flex_basis, margin: #margin, padding: #padding,
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
            syn::parse_quote!(::griffr_gui::ui::widget::#base)
        };
        quote! {
            #kind => Box::new(<#widget_ty as ::griffr_gui::ui::widget::Widget>::init(slot)?),
        }
    });

    quote! {
        #root
        impl #ident {
            pub const DECLS: &'static [::griffr_gui::ui::WidgetDecl] = &[#(#decls),*];
            pub const TOPOLOGY: &'static [(u16, i16)] = &[#(#topology),*];
            pub const CAPABILITIES: &'static [(u16, bool, bool, bool)] = &[#(#capabilities),*];
            pub const CANVAS_COUNT: usize = #canvas_count;
            pub fn build_static_plan() -> ::griffr_gui::ui::StaticPlan {
                let mut widgets = vec![#(#static_widgets),*];
                widgets.sort_by_key(|w| (w.z_order, w.id));
                ::griffr_gui::ui::StaticPlan { widgets, merged_tile_count: Self::CANVAS_COUNT }
            }
            pub fn build_runtime(size: ::winio::prelude::Size) -> ::griffr_gui::ui::UiRuntime {
                ::griffr_gui::ui::UiRuntime::from_static(Self::build_static_plan(), size)
            }
        }

        #[derive(Debug)]
        pub enum #event_ident { Redraw, Target(Option<::griffr_gui::ui::WidgetId>), }

        pub struct #comp_ident {
            root: ::winio::prelude::Child<::winio::widgets::View>,
            #(#canvas_fields)*
            runtime: ::griffr_gui::ui::UiRuntime,
            widgets: Vec<(::griffr_gui::ui::WidgetId, Box<dyn ::griffr_gui::ui::widget::Widget>)>,
            pointers: [::winio::prelude::Point; #canvas_count],
        }

        #[derive(Debug)]
        pub enum #msg_ident { Noop, Resize(::winio::prelude::Size), Canvas(usize, ::winio::prelude::CanvasEvent), }

        impl ::winio::prelude::Component for #comp_ident {
            type Error = ::winio::prelude::Error;
            type Event = #event_ident;
            type Init<'a> = &'a ::winio::prelude::Child<::winio::widgets::Window>;
            type Message = #msg_ident;
            async fn init(init: Self::Init<'_>, _sender: &::winio::prelude::ComponentSender<Self>) -> ::winio::prelude::Result<Self> {
                let mut runtime = #ident::build_runtime(init.client_size()?);
                ::winio::prelude::init! { root: ::winio::widgets::View = (init), #(#canvas_inits)* }
                let size = init.client_size()?;
                root.set_loc(::winio::prelude::Point::new(0.0, 0.0))?;
                root.set_size(Self::expand_size(size))?;
                let widgets = Self::build_widgets(&runtime)?;
                for (id, w) in &widgets {
                    if let Some(node) = runtime.static_plan.widgets.iter_mut().find(|n| n.id == *id) {
                        node.capabilities = w.capabilities();
                    }
                }
                Ok(Self { root, #(#canvas_struct_inits)* widgets, runtime, pointers: [::winio::prelude::Point::new(0.0, 0.0); #canvas_count] })
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
                    #msg_ident::Resize(size) => { self.root.set_loc(::winio::prelude::Point::new(0.0, 0.0))?; self.root.set_size(Self::expand_size(size))?; Ok(true) }
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
                        let hit = self.runtime.dispatch_with_pointer(&ev, p.x, p.y);
                        for (id, widget) in &mut self.widgets { widget.handle_event(&ev, hit.is_some_and(|hit_id| hit_id == *id))?; }
                        sender.output(#event_ident::Target(hit));
                        sender.output(#event_ident::Redraw);
                        Ok(true)
                    }
                }
            }
            fn render(&mut self, _sender: &::winio::prelude::ComponentSender<Self>) -> ::winio::prelude::Result<()> {
                const TILE_OVERLAP_PX: f64 = 0.5;
                let size = self.root.size()?;
                self.runtime.relayout(size);
                for idx in 0..#canvas_count {
                    let canvas = match idx { #(#render_match_arms)* _ => &mut self.#last_canvas_field, };
                    if let Some(tile) = self.runtime.plan.tile_plan.tiles.get(idx) {
                        let draw_w = tile.bounds.w + TILE_OVERLAP_PX;
                        let draw_h = tile.bounds.h + TILE_OVERLAP_PX;
                        canvas.set_visible(true)?;
                        canvas.set_loc(::winio::prelude::Point::new(tile.bounds.x, tile.bounds.y))?;
                        canvas.set_size(::winio::prelude::Size::new(draw_w, draw_h))?;
                        if let Some(top_id) = tile.widgets.last().copied() {
                            if let Some((_, widget)) = self.widgets.iter_mut().find(|(id, _)| *id == top_id) {
                                let mut ctx = canvas.context()?;
                                if let Some((_, widget_bounds)) = self.runtime.plan.bounds.iter().find(|(id, _)| *id == top_id) {
                                    let tx = widget_bounds.x - tile.bounds.x;
                                    let ty = widget_bounds.y - tile.bounds.y;
                                    ctx.set_transform(::winio::prelude::Transform::translation(tx, ty))?;
                                    widget.draw(&mut ctx, ::griffr_gui::ui::Rect::new(0.0, 0.0, widget_bounds.w, widget_bounds.h), tile.clipped)?;
                                }
                            }
                        }
                    } else {
                        canvas.set_visible(false)?;
                    }
                }
                Ok(())
            }
            fn render_children(&mut self) -> ::winio::prelude::Result<()> { #(#render_children_stmts)* self.root.render() }
        }

        impl #comp_ident {
            fn build_widgets(runtime: &::griffr_gui::ui::UiRuntime) -> ::winio::prelude::Result<Vec<(::griffr_gui::ui::WidgetId, Box<dyn ::griffr_gui::ui::widget::Widget>)>> {
                let mut out = Vec::<(::griffr_gui::ui::WidgetId, Box<dyn ::griffr_gui::ui::widget::Widget>)>::new();
                for node in &runtime.plan.widgets {
                    let bounds = runtime.plan.bounds.iter().find(|(id, _)| *id == node.id).map(|(_, b)| *b).unwrap_or(::griffr_gui::ui::Rect::new(0.0, 0.0, 0.0, 0.0));
                    let clipped = runtime.plan.tile_plan.tiles.iter().find(|tile| tile.widgets.iter().any(|id| *id == node.id)).map(|t| t.clipped).unwrap_or(false);
                    let slot = ::griffr_gui::ui::widget::TileSlot { bounds, clipped };
                    let widget: Box<dyn ::griffr_gui::ui::widget::Widget> = match node.widget_type {
                        #(#widget_ctor_arms)*
                        _ => unreachable!("widget_tree generated unknown widget kind"),
                    };
                    out.push((node.id, widget));
                }
                Ok(out)
            }
            fn local_to_global(&self, idx: usize, p: ::winio::prelude::Point) -> ::winio::prelude::Point {
                self.runtime.plan.tile_plan.tiles.get(idx).map(|t| ::winio::prelude::Point::new(p.x + t.bounds.x, p.y + t.bounds.y)).unwrap_or(p)
            }
            fn expand_size(size: ::winio::prelude::Size) -> ::winio::prelude::Size {
                const COMPONENT_OVERDRAW_PX: f64 = 0.5;
                ::winio::prelude::Size::new(size.width + COMPONENT_OVERDRAW_PX, size.height + COMPONENT_OVERDRAW_PX)
            }
        }
    }
}
