use proc_macro2::TokenStream;
use quote::quote;
use syn::{Ident, ItemStruct};

use crate::model::FlatNode;

pub(crate) fn expand_widget_tree(root: ItemStruct, flat: Vec<FlatNode>) -> TokenStream {
    let ident = &root.ident;
    let comp_ident = Ident::new(&format!("{}Component", ident), ident.span());
    let msg_ident = Ident::new(&format!("{}ComponentMessage", ident), ident.span());

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
                id: #id,
                parent: #parent,
                widget_type: #widget_type,
                hoverable: #hoverable,
                clickable: #clickable,
                scrollable: #scrollable,
                clip: #clip,
                z: #z,
                direction: #direction,
                flex_grow: #flex_grow,
                flex_shrink: #flex_shrink,
                flex_basis: #flex_basis,
                margin: #margin,
                padding: #padding,
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

    quote! {
        #root
        impl #ident {
            pub const DECLS: &'static [::griffr_gui::ui::WidgetDecl] = &[#(#decls),*];
            pub const TOPOLOGY: &'static [(u16, i16)] = &[#(#topology),*];
            pub const CAPABILITIES: &'static [(u16, bool, bool, bool)] = &[#(#capabilities),*];
            pub fn build_runtime(size: ::winio::prelude::Size) -> ::griffr_gui::ui::UiRuntime {
                ::griffr_gui::ui::UiRuntime::new(Self::DECLS, size)
            }
        }

        pub struct #comp_ident {
            inner: ::winio::prelude::Child<::griffr_gui::ui::UiComponent>,
        }

        #[derive(Debug)]
        pub enum #msg_ident {
            Noop,
            Resize(::winio::prelude::Size),
            Inner(::griffr_gui::ui::UiEvent),
        }

        impl ::winio::prelude::Component for #comp_ident {
            type Error = ::winio::prelude::Error;
            type Event = ::griffr_gui::ui::UiEvent;
            type Init<'a> = &'a ::winio::prelude::Child<::winio::widgets::Window>;
            type Message = #msg_ident;

            async fn init(init: Self::Init<'_>, _sender: &::winio::prelude::ComponentSender<Self>) -> ::winio::prelude::Result<Self> {
                let runtime = #ident::build_runtime(init.client_size()?);
                let inner = ::winio::prelude::Child::<::griffr_gui::ui::UiComponent>::init((init, runtime)).await?;
                Ok(Self { inner })
            }

            async fn start(&mut self, sender: &::winio::prelude::ComponentSender<Self>) -> ! {
                ::winio::prelude::start! {
                    sender, default: #msg_ident::Noop,
                    self.inner => {
                        e => #msg_ident::Inner(e),
                    }
                }
            }

            async fn update_children(&mut self) -> ::winio::prelude::Result<bool> {
                ::winio::prelude::update_children!(self.inner)
            }

            async fn update(
                &mut self,
                message: Self::Message,
                sender: &::winio::prelude::ComponentSender<Self>,
            ) -> ::winio::prelude::Result<bool> {
                match message {
                    #msg_ident::Noop => Ok(false),
                    #msg_ident::Resize(size) => {
                        self.inner.post(::griffr_gui::ui::UiMessage::Resize(size));
                        Ok(true)
                    }
                    #msg_ident::Inner(e) => {
                        sender.output(e);
                        Ok(true)
                    }
                }
            }

            fn render(&mut self, _sender: &::winio::prelude::ComponentSender<Self>) -> ::winio::prelude::Result<()> {
                Ok(())
            }

            fn render_children(&mut self) -> ::winio::prelude::Result<()> {
                self.inner.render()
            }
        }
    }
}
