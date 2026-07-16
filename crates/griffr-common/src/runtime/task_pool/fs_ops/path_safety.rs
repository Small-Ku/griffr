use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

pub(crate) fn parse_safe_relative_path(label: &str, raw: &str) -> Result<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidPath(format!(
            "{label} contains an empty path"
        )));
    }

    let mut relative = PathBuf::new();
    for component in Path::new(trimmed).components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(part) => relative.push(part),
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => {
                return Err(Error::InvalidPath(format!(
                    "{label} contains unsupported path: {trimmed}"
                )));
            }
        }
    }

    if relative.as_os_str().is_empty() {
        return Err(Error::InvalidPath(format!(
            "{label} contains an empty path"
        )));
    }

    Ok(relative)
}

#[cfg(test)]
mod tests {
    use super::parse_safe_relative_path;

    #[test]
    fn parse_safe_relative_path_rejects_escape_paths() {
        let err = parse_safe_relative_path("patch.json local_path", "..\\outside.ab").unwrap_err();
        assert!(err.to_string().contains("unsupported path"));
    }
}
