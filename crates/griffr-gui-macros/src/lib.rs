mod expand;
mod flatten;
mod parse;
mod tree;

use proc_macro::TokenStream;
use syn::parse_macro_input;
use syn::ItemStruct;

use crate::expand::expand_widget_tree;
use crate::flatten::flatten_tree;
use crate::tree::TreeInput;

#[proc_macro_attribute]
pub fn widget_tree(attr: TokenStream, item: TokenStream) -> TokenStream {
    let tree = parse_macro_input!(attr as TreeInput);
    let root = parse_macro_input!(item as ItemStruct);
    let flat = flatten_tree(&tree.root);
    expand_widget_tree(root, flat).into()
}
