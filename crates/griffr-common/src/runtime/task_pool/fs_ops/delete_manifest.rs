use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::runtime::DELETE_FILES_MANIFEST_NAME;

use super::path_safety::parse_safe_relative_path;

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

pub(crate) fn apply_delete_files_manifest(
    dest_root: &Path,
    mut progress_callback: Option<&mut dyn FnMut(&Path, usize, usize)>,
) -> Result<()> {
    let manifest_path = dest_root.join(DELETE_FILES_MANIFEST_NAME);
    if !manifest_path.is_file() {
        return Ok(());
    }

    let manifest = std::fs::read_to_string(&manifest_path).map_err(|e| Error::OpenFileFailed {
        path: manifest_path.clone(),
        source: e,
    })?;
    let entries = manifest
        .lines()
        .enumerate()
        .filter_map(|(line_idx, line)| match parse_delete_files_entry(line) {
            Ok(Some(relative)) => Some(Ok(relative)),
            Ok(None) => None,
            Err(err) => Some(Err(Error::Config(format!(
                "Failed to parse {} line {}: {err}",
                DELETE_FILES_MANIFEST_NAME,
                line_idx + 1
            )))),
        })
        .collect::<Result<Vec<_>>>()?;
    let total_entries = entries.len();
    if total_entries > 0 {
        if let Some(cb) = progress_callback.as_deref_mut() {
            cb(Path::new("."), 0, total_entries);
        }
    }
    for (index, relative) in entries.iter().enumerate() {
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
        if let Some(cb) = progress_callback.as_deref_mut() {
            cb(relative, index + 1, total_entries);
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
    use std::path::{Path, PathBuf};

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

        let mut progress = Vec::new();
        let mut on_progress = |path: &Path, completed: usize, total: usize| {
            progress.push((path.to_path_buf(), completed, total));
        };
        apply_delete_files_manifest(&dest_root, Some(&mut on_progress)).unwrap();
        assert_eq!(
            progress,
            vec![
                (PathBuf::from("."), 0, 1),
                (
                    PathBuf::from("Endfield_Data/Plugins/x86_64/libHAPI.dll"),
                    1,
                    1,
                ),
            ]
        );
        assert!(!obsolete_path.exists());
        assert!(!dest_root.join(DELETE_FILES_MANIFEST_NAME).exists());
    }
}
