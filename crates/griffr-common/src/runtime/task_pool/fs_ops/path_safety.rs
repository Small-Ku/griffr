use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

pub(crate) fn parse_safe_relative_path(label: &str, raw: &str) -> Result<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(invalid_path(label, "contains an empty path"));
    }

    // Launcher and patch manifests use both separator styles. Normalize before
    // component parsing so the same traversal rules apply on Windows and in
    // non-Windows tests.
    let normalized = trimmed.replace('\\', "/");
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
            "C:\\outside.ab",
            "\\\\server\\share\\outside.ab",
            "/outside.ab",
            "file.bin:stream",
        ] {
            let error = parse_safe_relative_path("manifest path", raw).unwrap_err();
            assert!(error.to_string().contains("unsupported path"));
        }
    }

    #[test]
    fn parse_safe_relative_path_normalizes_manifest_separators() {
        assert_eq!(
            parse_safe_relative_path("manifest path", "Data\\sub/file.bin").unwrap(),
            PathBuf::from("Data").join("sub").join("file.bin")
        );
    }
}
