use syn::parse::{Parse, ParseStream};
use syn::{braced, parenthesized, Expr, Ident, LitFloat, LitInt, Path, Result, Token};

use crate::tree::{Clip, Direction, NodeInput, NodeProps, TreeInput};

impl Parse for NodeInput {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let kind: Path = input.parse()?;
        let mut props = NodeProps::default();
        if input.peek(syn::token::Paren) {
            let content;
            parenthesized!(content in input);
            while !content.is_empty() {
                let key: Ident = content.parse()?;
                content.parse::<Token![=]>()?;
                match key.to_string().as_str() {
                    "flex_direction" => {
                        let v: Ident = content.parse()?;
                        props.direction = Some(match v.to_string().as_str() {
                            "Row" => Direction::Row,
                            "Column" => Direction::Column,
                            _ => return Err(syn::Error::new(v.span(), "expected Row or Column")),
                        });
                    }
                    "flex_grow" => props.flex_grow = Some(parse_num(&content)?),
                    "flex_shrink" => props.flex_shrink = Some(parse_num(&content)?),
                    "flex_basis" => props.flex_basis = Some(parse_num(&content)?),
                    "margin" => props.margin = Some(parse_num(&content)?),
                    "padding" => props.padding = Some(parse_num(&content)?),
                    "clip" => {
                        let v: Ident = content.parse()?;
                        props.clip = Some(match v.to_string().as_str() {
                            "ForceClip" => Clip::ForceClip,
                            "ForceNoClip" => Clip::ForceNoClip,
                            "InferFromCapabilities" => Clip::Infer,
                            _ => {
                                return Err(syn::Error::new(
                                    v.span(),
                                    "expected ForceClip, ForceNoClip, or InferFromCapabilities",
                                ))
                            }
                        });
                    }
                    "z" => props.z = Some(content.parse::<LitInt>()?.base10_parse::<i32>()?),
                    "aspect_ratio" => props.aspect_ratio = Some(parse_num(&content)?),
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

impl Parse for TreeInput {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        Ok(Self {
            root: input.parse()?,
        })
    }
}

fn parse_num(content: ParseStream<'_>) -> Result<f64> {
    if content.peek(LitFloat) {
        Ok(content.parse::<LitFloat>()?.base10_parse::<f64>()?)
    } else {
        Ok(content.parse::<LitInt>()?.base10_parse::<f64>()?)
    }
}
