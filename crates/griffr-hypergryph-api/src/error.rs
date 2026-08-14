use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[cfg(feature = "client")]
    #[error("HTTP client error: {0}")]
    Cyper(#[from] cyper::Error),

    #[error("UTF-8 error: {0}")]
    FromUtf8(#[from] std::string::FromUtf8Error),

    #[cfg(feature = "crypto")]
    #[error("Base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),

    #[error("Failed to {action} {path}: {source}")]
    IoAt {
        action: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("{context}{detail}")]
    Message {
        context: &'static str,
        detail: String,
    },
}

impl From<griffr_core::Error> for Error {
    fn from(error: griffr_core::Error) -> Self {
        Self::Message {
            context: "Target resolution error: ",
            detail: error.to_string(),
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
