use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

pub(crate) fn parse_safe_relative_path(label: &str, raw: &str) -> Result<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(invalid_path(label, "contains an empty path"));
    }

    let normalized_slashes = trimmed.replace('\\', "/");
    if normalized_slashes.starts_with("//") {
        return Err(invalid_path(
            label,
            &format!("contains unsupported path: {trimmed}"),
        ));
    }

    // Launcher and patch manifests use both separator styles and occasionally include
    // a leading slash (e.g. "/Arknights.exe"). Strip a single leading slash before normalization
    // so manifest root references resolve to relative paths inside the install root.
    let stripped = trimmed.strip_prefix(['/', '\\']).unwrap_or(trimmed);
    if stripped.is_empty() {
        return Err(invalid_path(label, "contains an empty path"));
    }

    let normalized = stripped.replace('\\', "/");
    if normalized.starts_with('/')
        || normalized
            .as_bytes()
            .get(1)
            .is_some_and(|byte| *byte == b':')
        || normalized.split('/').any(|part| part.contains(':'))
    {
        return Err(invalid_path(
            label,
            &format!("contains unsupported path: {trimmed}"),
        ));
    }

    let mut relative = PathBuf::new();
    for component in Path::new(&normalized).components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(part) => relative.push(part),
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => {
                return Err(invalid_path(
                    label,
                    &format!("contains unsupported path: {trimmed}"),
                ));
            }
        }
    }

    if relative.as_os_str().is_empty() {
        return Err(invalid_path(label, "contains an empty path"));
    }

    Ok(relative)
}

fn invalid_path(label: &str, detail: &str) -> Error {
    Error::Message {
        context: "Invalid path: ",
        detail: format!("{label} {detail}"),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_safe_relative_path;
    use std::path::PathBuf;

    #[test]
    fn parse_safe_relative_path_rejects_escape_and_absolute_paths() {
        for raw in [
            "..\\outside.ab",
            "../outside.ab",
            "/../outside.ab",
            "C:\\outside.ab",
            "\\\\server\\share\\outside.ab",
            "file.bin:stream",
        ] {
            let error = parse_safe_relative_path("manifest path", raw).unwrap_err();
            assert!(error.to_string().contains("unsupported path"));
        }
    }

    #[test]
    fn parse_safe_relative_path_normalizes_manifest_separators_and_leading_slashes() {
        assert_eq!(
            parse_safe_relative_path("manifest path", "Data\\sub/file.bin").unwrap(),
            PathBuf::from("Data").join("sub").join("file.bin")
        );
        assert_eq!(
            parse_safe_relative_path("manifest path", "/Arknights.exe").unwrap(),
            PathBuf::from("Arknights.exe")
        );
        assert_eq!(
            parse_safe_relative_path("manifest path", "\\Arknights_Data/boot.config").unwrap(),
            PathBuf::from("Arknights_Data").join("boot.config")
        );
    }
}
