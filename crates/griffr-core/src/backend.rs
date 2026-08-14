/// Launcher/update backend selected by a deployment region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendKind {
    Hypergryph,
    Yostar,
}

impl std::fmt::Display for BackendKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Hypergryph => "hypergryph",
            Self::Yostar => "yostar",
        })
    }
}
