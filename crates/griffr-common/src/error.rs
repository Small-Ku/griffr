use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("HTTP client error: {0}")]
    Cyper(#[from] cyper::Error),

    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("Chrono parse error: {0}")]
    ChronoParse(#[from] chrono::ParseError),

    #[error("Strip prefix error: {0}")]
    StripPrefix(#[from] std::path::StripPrefixError),

    #[error("Parse int error: {0}")]
    ParseInt(#[from] std::num::ParseIntError),

    #[error("Try from slice error: {0}")]
    TryFromSlice(#[from] std::array::TryFromSliceError),

    #[error("UTF-8 error: {0}")]
    FromUtf8(#[from] std::string::FromUtf8Error),

    #[error("Base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),

    #[error("Timeout error")]
    Timeout(#[from] compio::time::Elapsed),

    #[error("Failed to {action} {path}: {source}")]
    IoAt {
        action: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("Failed to {action} from {src} to {dest}: {source}")]
    IoBetween {
        action: &'static str,
        src: PathBuf,
        dest: PathBuf,
        source: std::io::Error,
    },

    #[error("{context}{detail}")]
    Message {
        context: &'static str,
        detail: String,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
