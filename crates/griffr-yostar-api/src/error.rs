#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP client error: {0}")]
    Cyper(#[from] cyper::Error),

    #[error("{context}{detail}")]
    Message {
        context: &'static str,
        detail: String,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
