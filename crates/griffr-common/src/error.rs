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

    #[error("API error: {0}")]
    Api(Box<crate::api::client::ApiError>),

    #[error("Try from slice error: {0}")]
    TryFromSlice(#[from] std::array::TryFromSliceError),

    #[error("UTF-8 error: {0}")]
    FromUtf8(#[from] std::string::FromUtf8Error),

    #[error("Base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),

    #[error("Timeout error")]
    Timeout(#[from] compio::time::Elapsed),

    #[error("Failed to open file {path}: {source}")]
    OpenFileFailed {
        path: std::path::PathBuf,
        source: std::io::Error,
    },

    #[error("Failed to read directory {path}: {source}")]
    ReadDirFailed {
        path: std::path::PathBuf,
        source: std::io::Error,
    },

    #[error("Failed to create directory {path}: {source}")]
    CreateDirFailed {
        path: std::path::PathBuf,
        source: std::io::Error,
    },

    #[error("Failed to remove file or directory {path}: {source}")]
    RemoveFailed {
        path: std::path::PathBuf,
        source: std::io::Error,
    },

    #[error("Failed to copy file from {src} to {dest}: {source}")]
    CopyFailed {
        src: std::path::PathBuf,
        dest: std::path::PathBuf,
        source: std::io::Error,
    },

    #[error("Failed to rename file from {src} to {dest}: {source}")]
    RenameFailed {
        src: std::path::PathBuf,
        dest: std::path::PathBuf,
        source: std::io::Error,
    },

    #[error("Failed to query file metadata/stat for {path}: {source}")]
    StatFailed {
        path: std::path::PathBuf,
        source: std::io::Error,
    },

    #[error("Failed to write to file {path}: {source}")]
    WriteFileFailed {
        path: std::path::PathBuf,
        source: std::io::Error,
    },

    #[error("Task pool error: {0}")]
    TaskPool(String),

    #[error("Invalid path: {0}")]
    InvalidPath(String),

    #[error("Extraction error: {0}")]
    Extraction(String),

    #[error("Download error: {0}")]
    Download(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Launcher/Process error: {0}")]
    Launcher(String),

    #[error("VFS error: {0}")]
    Vfs(String),

    #[error("API client wrapper error: {0}")]
    ApiClient(String),

    #[error("Crypto error: {0}")]
    Crypto(String),

    #[error("Integrity error: {0}")]
    Integrity(String),

    #[error("Game error: {0}")]
    Game(String),

    #[error("{0}")]
    Other(String),
}

impl From<crate::api::client::ApiError> for Error {
    fn from(err: crate::api::client::ApiError) -> Self {
        Self::Api(Box::new(err))
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
