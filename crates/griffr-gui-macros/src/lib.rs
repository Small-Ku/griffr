use proc_macro::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{
    braced, parenthesized, parse_macro_input, Expr, Ident, ItemStruct, LitBool, LitFloat, LitInt, Result, Token,
};

#[derive(Clone, Default)]
struct NodeProps {
    direction: Option<i8>,
    flex_grow: Option<f64>,
    flex_shrink: Option<f64>,
    flex_basis: Option<f64>,
    margin: Option<f64>,
    padding: Option<f64>,
    hoverable: Option<bool>,
    clickable: Option<bool>,
    scrollable: Option<bool>,
    clip: Option<i8>,
    z: Option<i32>,
}

#[derive(Clone)]
struct NodeInput {
    kind: Ident,
    props: NodeProps,
    children: Vec<NodeInput>,
}

impl Parse for NodeInput {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let kind: Ident = input.parse()?;
        let mut props = NodeProps::default();
        if input.peek(syn::token::Paren) {
            let content;
            parenthesized!(content in input);
            while !content.is_empty() {
                let key: Ident = content.parse()?;
                content.parse::<Token![=]>()?;
                let key_s = key.to_string();
                match key_s.as_str() {
                    "flex_direction" => {
                        let v: Ident = content.parse()?;
                        props.direction = Some(if v == "Row" { 0 } else { 1 });
                    }
                    "flex_grow" => props.flex_grow = Some(parse_num(&content)?),
                    "flex_shrink" => props.flex_shrink = Some(parse_num(&content)?),
                    "flex_basis" => props.flex_basis = Some(parse_num(&content)?),
                    "margin" => props.margin = Some(parse_num(&content)?),
                    "padding" => props.padding = Some(parse_num(&content)?),
                    "hoverable" => props.hoverable = Some(content.parse::<LitBool>()?.value),
                    "clickable" => props.clickable = Some(content.parse::<LitBool>()?.value),
                    "scrollable" => props.scrollable = Some(content.parse::<LitBool>()?.value),
                    "clip" => {
                        let v: Ident = content.parse()?;
                        props.clip = Some(match v.to_string().as_str() {
                            "ForceClip" => 1,
                            "ForceNoClip" => -1,
                            _ => 0,
                        });
                    }
                    "z" => props.z = Some(content.parse::<LitInt>()?.base10_parse::<i32>()?),
                    "label" => {
                        let _ = content.parse::<Expr>()?;
                    }
                    _ => return Err(content.error("unknown property")),
                }
                if content.peek(Token![,]) {
                    let _ = content.parse::<Token![,]>()?;
                }
            }
        }

        let mut children = Vec::new();
        if input.peek(syn::token::Brace) {
            let content;
            braced!(content in input);
            while !content.is_empty() {
                children.push(content.parse()?);
                if content.peek(Token![,]) {
                    let _ = content.parse::<Token![,]>()?;
                }
            }
        }
        Ok(Self {
            kind,
            props,
            children,
        })
    }
}

struct TreeInput {
    root: NodeInput,
}

impl Parse for TreeInput {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        Ok(Self { root: input.parse()? })
    }
}

#[derive(Clone)]
struct FlatNode {
    id: u16,
    parent: i16,
    kind: String,
    hoverable: bool,
    clickable: bool,
    scrollable: bool,
    clip: i8,
    z: i32,
    direction: i8,
    flex_grow: f64,
    flex_shrink: f64,
    flex_basis: f64,
    margin: f64,
    padding: f64,
}

#[proc_macro_attribute]
pub fn widget_tree(attr: TokenStream, item: TokenStream) -> TokenStream {
    let tree = parse_macro_input!(attr as TreeInput);
    let root = parse_macro_input!(item as ItemStruct);
    let ident = &root.ident;
    let comp_ident = Ident::new(&format!("{}Component", ident), ident.span());
    let msg_ident = Ident::new(&format!("{}ComponentMessage", ident), ident.span());

    let mut flat = Vec::new();
    let mut next_id: u16 = 0;
    flatten(&tree.root, -1, &mut next_id, &mut flat);

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
    .into()
}

fn flatten(node: &NodeInput, parent: i16, next_id: &mut u16, out: &mut Vec<FlatNode>) {
    let id = *next_id;
    *next_id += 1;
    let kind = node.kind.to_string();
    let defaults = defaults_for_kind(&kind);
    let z = node.props.z.unwrap_or(id as i32);
    out.push(FlatNode {
        id,
        parent,
        kind,
        hoverable: node.props.hoverable.unwrap_or(defaults.0),
        clickable: node.props.clickable.unwrap_or(defaults.1),
        scrollable: node.props.scrollable.unwrap_or(defaults.2),
        clip: node.props.clip.unwrap_or(if defaults.2 { 1 } else { 0 }),
        z,
        direction: node.props.direction.unwrap_or(1),
        flex_grow: node.props.flex_grow.unwrap_or(0.0),
        flex_shrink: node.props.flex_shrink.unwrap_or(1.0),
        flex_basis: node.props.flex_basis.unwrap_or(100.0),
        margin: node.props.margin.unwrap_or(0.0),
        padding: node.props.padding.unwrap_or(0.0),
    });
    for child in &node.children {
        flatten(child, id as i16, next_id, out);
    }
}

fn parse_num(content: ParseStream<'_>) -> Result<f64> {
    if content.peek(LitFloat) {
        Ok(content.parse::<LitFloat>()?.base10_parse::<f64>()?)
    } else {
        Ok(content.parse::<LitInt>()?.base10_parse::<f64>()?)
    }
}

fn defaults_for_kind(kind: &str) -> (bool, bool, bool) {
    match kind {
        "Button" => (true, true, false),
        "Banner" => (true, false, true),
        _ => (false, false, false),
    }
}
