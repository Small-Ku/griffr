use proc_macro2::TokenStream;
use quote::quote;
use std::collections::BTreeSet;
use syn::{Ident, ItemStruct, Type};

use crate::model::{Clip, Direction, FlatNode, Sizing};

use super::component;

pub(super) struct ComponentTokens<'a> {
    pub(super) root: &'a ItemStruct,
    pub(super) ident: &'a Ident,
    pub(super) comp_ident: &'a Ident,
    pub(super) msg_ident: &'a Ident,
    pub(super) event_ident: &'a Ident,
    pub(super) routed_ident: &'a Ident,
    pub(super) canvas_count: usize,
    pub(super) widget_count: usize,
    pub(super) parents: Vec<TokenStream>,
    pub(super) z_order_back_to_front: Vec<TokenStream>,
    pub(super) z_order_front_to_back: Vec<TokenStream>,
    pub(super) static_widgets: Vec<TokenStream>,
    pub(super) canvas_fields: Vec<TokenStream>,
    pub(super) canvas_inits: Vec<TokenStream>,
    pub(super) canvas_struct_inits: Vec<TokenStream>,
    pub(super) start_arms: Vec<TokenStream>,
    pub(super) update_children_items: Vec<TokenStream>,
    pub(super) canvas_match_arms: Vec<TokenStream>,
    pub(super) widget_ctor_arms: Vec<TokenStream>,
}

pub(crate) fn expand_widget_tree(root: ItemStruct, flat: Vec<FlatNode>) -> TokenStream {
    let ident = &root.ident;
    let comp_ident = Ident::new(&format!("{}Component", ident), ident.span());
    let msg_ident = Ident::new(&format!("{}ComponentMessage", ident), ident.span());
    let event_ident = Ident::new(&format!("{}ComponentEvent", ident), ident.span());
    let routed_ident = Ident::new(&format!("{}RoutedEvent", ident), ident.span());
    // Allocate the safe upper bound. Runtime widget capabilities are the sole
    // source of truth and may change the final tile plan after construction.
    let canvas_count = flat.len();

    let widget_count = flat.len();
    let parents: Vec<_> = flat
        .iter()
        .map(|n| {
            let parent = n.parent;
            if parent >= 0 {
                quote! { Some(::griffr_gui::ui::WidgetId(#parent as u16)) }
            } else {
                quote! { None }
            }
        })
        .collect();

    let mut sorted_back_to_front = flat.clone();
    sorted_back_to_front.sort_by_key(|n| (n.z, n.id));
    let mut sorted_front_to_back = sorted_back_to_front.clone();
    sorted_front_to_back.reverse();

    let z_order_back_to_front: Vec<_> = sorted_back_to_front
        .iter()
        .map(|n| {
            let id = n.id;
            quote! { ::griffr_gui::ui::WidgetId(#id) }
        })
        .collect();
    let z_order_front_to_back: Vec<_> = sorted_front_to_back
        .iter()
        .map(|n| {
            let id = n.id;
            quote! { ::griffr_gui::ui::WidgetId(#id) }
        })
        .collect();
    let static_widgets = flat
        .iter()
        .map(|n| {
            let id = n.id;
            let parent = n.parent;
            let hoverable = n.hoverable;
            let clickable = n.clickable;
            let scrollable = n.scrollable;
            let opaque = n.opaque;
            let clip = match n.clip {
                Clip::Infer => quote! { ::griffr_gui::ui::ClipPolicy::InferFromCapabilities },
                Clip::ForceClip => quote! { ::griffr_gui::ui::ClipPolicy::ForceClip },
                Clip::ForceNoClip => quote! { ::griffr_gui::ui::ClipPolicy::ForceNoClip },
            };
            let z = n.z;
            let kind = &n.kind;
            let direction = match n.direction {
                Direction::Row => quote! { ::griffr_gui::ui::LayoutDirection::Row },
                Direction::Column => quote! { ::griffr_gui::ui::LayoutDirection::Column },
            };
            let sizing = match n.sizing {
                Sizing::AspectRatio(ratio) => {
                    quote! { ::griffr_gui::ui::SizingPolicy::AspectRatio(#ratio) }
                }
                Sizing::Flex {
                    grow,
                    shrink,
                    basis,
                } => {
                    let grow = grow.map(|value| quote! { #value }).unwrap_or_else(
                        || quote! { ::griffr_gui::ui::SizingPolicy::DEFAULT_FLEX_GROW },
                    );
                    let shrink = shrink.map(|value| quote! { #value }).unwrap_or_else(
                        || quote! { ::griffr_gui::ui::SizingPolicy::DEFAULT_FLEX_SHRINK },
                    );
                    let basis = basis.map(|value| quote! { #value }).unwrap_or_else(
                        || quote! { ::griffr_gui::ui::SizingPolicy::DEFAULT_FLEX_BASIS },
                    );
                    quote! {
                        ::griffr_gui::ui::SizingPolicy::Flex {
                            grow: #grow,
                            shrink: #shrink,
                            basis: #basis,
                        }
                    }
                }
            };
            let margin = n
                .margin
                .map(|value| quote! { #value })
                .unwrap_or_else(|| quote! { ::griffr_gui::ui::LayoutSpec::DEFAULT_MARGIN });
            let padding = n
                .padding
                .map(|value| quote! { #value })
                .unwrap_or_else(|| quote! { ::griffr_gui::ui::LayoutSpec::DEFAULT_PADDING });
            quote! {
                ::griffr_gui::ui::WidgetNode {
                    id: ::griffr_gui::ui::WidgetId(#id),
                    parent: (#parent >= 0).then_some(::griffr_gui::ui::WidgetId(#parent as u16)),
                    hoverable: #hoverable,
                    clickable: #clickable,
                    scrollable: #scrollable,
                    opaque: #opaque,
                    clip: #clip,
                    layout: ::griffr_gui::ui::LayoutSpec {
                        direction: #direction,
                        margin: #margin,
                        padding: #padding,
                        sizing: #sizing,
                    },
                    z_order: #z,
                    widget_type: #kind,
                }
            }
        })
        .collect::<Vec<_>>();
    let canvas_fields: Vec<_> = (0..canvas_count)
        .map(|idx| {
            let field = Ident::new(&format!("tile{}", idx), ident.span());
            quote! { #field: ::winio::prelude::Child<::winio::widgets::Canvas>, }
        })
        .collect();
    let canvas_inits: Vec<_> = (0..canvas_count)
        .map(|idx| {
            let field = Ident::new(&format!("tile{}", idx), ident.span());
            quote! { #field: ::winio::widgets::Canvas = (&root), }
        })
        .collect();
    let canvas_struct_inits: Vec<_> = (0..canvas_count)
        .map(|idx| {
            let field = Ident::new(&format!("tile{}", idx), ident.span());
            quote! { #field, }
        })
        .collect();
    let start_arms: Vec<_> = (0..canvas_count)
        .map(|idx| {
            let field = Ident::new(&format!("tile{}", idx), ident.span());
            quote! { self.#field => { e => #msg_ident::Canvas(#idx, e), }, }
        })
        .collect();
    let update_children_items: Vec<_> = (0..canvas_count)
        .map(|idx| {
            let field = Ident::new(&format!("tile{}", idx), ident.span());
            quote! { self.#field, }
        })
        .collect();
    let canvas_match_arms: Vec<_> = (0..canvas_count)
        .map(|idx| {
            let field = Ident::new(&format!("tile{}", idx), ident.span());
            quote! { #idx => &mut self.#field, }
        })
        .collect();
    let widget_kinds: BTreeSet<String> = flat.iter().map(|n| n.kind.clone()).collect();
    let widget_ctor_arms: Vec<_> = widget_kinds
        .iter()
        .map(|kind| {
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
        })
        .collect();

    component::render(ComponentTokens {
        root: &root,
        ident,
        comp_ident: &comp_ident,
        msg_ident: &msg_ident,
        event_ident: &event_ident,
        routed_ident: &routed_ident,
        canvas_count,
        widget_count,
        parents,
        z_order_back_to_front,
        z_order_front_to_back,
        static_widgets,
        canvas_fields,
        canvas_inits,
        canvas_struct_inits,
        start_arms,
        update_children_items,
        canvas_match_arms,
        widget_ctor_arms,
    })
}
