use syn::Path;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Direction {
    Row,
    #[default]
    Column,
}

#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Clip {
    #[default]
    Infer,
    ForceClip,
    ForceNoClip,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Sizing {
    Flex {
        grow: Option<f64>,
        shrink: Option<f64>,
        basis: Option<f64>,
    },
    AspectRatio(f64),
}

#[derive(Clone, Default)]
pub(crate) struct NodeProps {
    pub(crate) direction: Option<Direction>,
    pub(crate) flex_grow: Option<f64>,
    pub(crate) flex_shrink: Option<f64>,
    pub(crate) flex_basis: Option<f64>,
    pub(crate) margin: Option<f64>,
    pub(crate) padding: Option<f64>,
    pub(crate) clip: Option<Clip>,
    pub(crate) z: Option<i32>,
    pub(crate) aspect_ratio: Option<f64>,
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
    pub(crate) clip: Clip,
    pub(crate) z: i32,
    pub(crate) direction: Direction,
    pub(crate) sizing: Sizing,
    pub(crate) margin: Option<f64>,
    pub(crate) padding: Option<f64>,
}
