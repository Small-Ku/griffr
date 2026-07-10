use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

use super::path_safety::parse_safe_relative_path;

pub(super) const DELETE_FILES_MANIFEST_NAME: &str = "delete_files.txt";

fn parse_delete_files_entry(line: &str) -> Result<Option<PathBuf>> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    Ok(Some(parse_safe_relative_path(
        DELETE_FILES_MANIFEST_NAME,
        trimmed,
    )?))
}

pub(crate) fn apply_delete_files_manifest(dest_root: &Path) -> Result<()> {
    let manifest_path = dest_root.join(DELETE_FILES_MANIFEST_NAME);
    if !manifest_path.is_file() {
        return Ok(());
    }

    let manifest = std::fs::read_to_string(&manifest_path).map_err(|e| Error::OpenFileFailed {
        path: manifest_path.clone(),
        source: e,
    })?;
    for (line_idx, line) in manifest.lines().enumerate() {
        let Some(relative) = parse_delete_files_entry(line).map_err(|e| {
            Error::Config(format!(
                "Failed to parse {} line {}: {e}",
                DELETE_FILES_MANIFEST_NAME,
                line_idx + 1
            ))
        })?
        else {
            continue;
        };

        let target_path = dest_root.join(relative);
        match std::fs::symlink_metadata(&target_path) {
            Ok(meta) => {
                if meta.is_dir() {
                    std::fs::remove_dir_all(&target_path).map_err(|e| Error::RemoveFailed {
                        path: target_path.clone(),
                        source: e,
                    })?;
                } else {
                    std::fs::remove_file(&target_path).map_err(|e| Error::RemoveFailed {
                        path: target_path.clone(),
                        source: e,
                    })?;
                }
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => {
                return Err(Error::StatFailed {
                    path: target_path.clone(),
                    source: err,
                });
            }
        }
    }

    std::fs::remove_file(&manifest_path).map_err(|e| Error::RemoveFailed {
        path: manifest_path.clone(),
        source: e,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        apply_delete_files_manifest, parse_delete_files_entry, DELETE_FILES_MANIFEST_NAME,
    };

    #[test]
    fn parse_delete_files_entry_accepts_relative_paths() {
        let parsed =
            parse_delete_files_entry("Endfield_Data/StreamingAssets/VFS/ABC/file.chk").unwrap();
        assert_eq!(
            parsed,
            Some(PathBuf::from(
                "Endfield_Data/StreamingAssets/VFS/ABC/file.chk"
            ))
        );
    }

    #[test]
    fn parse_delete_files_entry_rejects_escape_paths() {
        let err = parse_delete_files_entry("..\\outside.txt").unwrap_err();
        assert!(err.to_string().contains("unsupported path"));
    }

    #[test]
    fn apply_delete_files_manifest_removes_listed_files_and_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let dest_root = temp.path().join("install");
        let obsolete_path = dest_root.join("Endfield_Data/Plugins/x86_64/libHAPI.dll");
        std::fs::create_dir_all(obsolete_path.parent().unwrap()).unwrap();
        std::fs::write(&obsolete_path, b"obsolete").unwrap();
        std::fs::write(
            dest_root.join(DELETE_FILES_MANIFEST_NAME),
            "Endfield_Data/Plugins/x86_64/libHAPI.dll\n",
        )
        .unwrap();

        apply_delete_files_manifest(&dest_root).unwrap();
        assert!(!obsolete_path.exists());
        assert!(!dest_root.join(DELETE_FILES_MANIFEST_NAME).exists());
    }
}
