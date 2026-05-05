use syn::Path;

#[derive(Clone, Default)]
pub(crate) struct NodeProps {
    pub(crate) direction: Option<i8>,
    pub(crate) flex_grow: Option<f64>,
    pub(crate) flex_shrink: Option<f64>,
    pub(crate) flex_basis: Option<f64>,
    pub(crate) margin: Option<f64>,
    pub(crate) padding: Option<f64>,
    pub(crate) hoverable: Option<bool>,
    pub(crate) clickable: Option<bool>,
    pub(crate) scrollable: Option<bool>,
    pub(crate) opaque: Option<bool>,
    pub(crate) clip: Option<i8>,
    pub(crate) z: Option<i32>,
}

#[derive(Clone)]
pub(crate) struct NodeInput {
    pub(crate) kind: Path,
    pub(crate) props: NodeProps,
    pub(crate) children: Vec<NodeInput>,
}

pub(crate) struct TreeInput {
    pub(crate) root: NodeInput,
}

#[derive(Clone)]
pub(crate) struct FlatNode {
    pub(crate) id: u16,
    pub(crate) parent: i16,
    pub(crate) kind: String,
    pub(crate) hoverable: bool,
    pub(crate) clickable: bool,
    pub(crate) scrollable: bool,
    pub(crate) opaque: bool,
    pub(crate) clip: i8,
    pub(crate) z: i32,
    pub(crate) direction: i8,
    pub(crate) flex_grow: f64,
    pub(crate) flex_shrink: f64,
    pub(crate) flex_basis: f64,
    pub(crate) margin: f64,
    pub(crate) padding: f64,
}
