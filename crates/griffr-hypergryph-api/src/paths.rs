use crate::{Error, Result};

pub const CONFIG_INI_NAME: &str = "config.ini";
pub const GAME_FILES_NAME: &str = "game_files";
pub const PACKAGE_FILES_NAME: &str = "package_files";
pub const CDN_FILES_DIR: &str = "files";

pub fn files_base_url(file_path: &str) -> Result<&str> {
    let normalized = file_path.trim_end_matches('/');
    let Some((base, final_segment)) = normalized.rsplit_once('/') else {
        return Err(invalid_files_path(file_path));
    };
    match final_segment {
        GAME_FILES_NAME => Ok(base),
        CDN_FILES_DIR => Ok(normalized),
        _ => Err(invalid_files_path(file_path)),
    }
}

fn invalid_files_path(file_path: &str) -> Error {
    Error::Message {
        context: "Hypergryph metadata path error: ",
        detail: format!(
            "expected file_path to end with '/{GAME_FILES_NAME}' or '/{CDN_FILES_DIR}', got: {file_path}"
        ),
    }
}

pub fn launcher_metadata_url(file_path: &str, filename: &str) -> Result<String> {
    Ok(format!("{}/{}", files_base_url(file_path)?, filename))
}
