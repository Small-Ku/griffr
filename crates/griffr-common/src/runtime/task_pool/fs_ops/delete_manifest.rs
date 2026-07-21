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

pub(crate) fn parse_delete_files_manifest(manifest: &str) -> Result<Vec<PathBuf>> {
    manifest
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
        .collect()
}

pub(crate) async fn apply_delete_files_manifest_async<F>(
    dest_root: &Path,
    mut progress_callback: Option<F>,
) -> Result<()>
where
    F: FnMut(&Path, usize, usize) + Send,
{
    let manifest_path = dest_root.join(DELETE_FILES_MANIFEST_NAME);
    match compio::fs::metadata(&manifest_path).await {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Ok(()),
        Err(source) if source.kind() == ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(Error::StatFailed {
                path: manifest_path,
                source,
            })
        }
    }

    let bytes = compio::fs::read(&manifest_path)
        .await
        .map_err(|source| Error::OpenFileFailed {
            path: manifest_path.clone(),
            source,
        })?;
    let manifest = String::from_utf8(bytes).map_err(|source| Error::OpenFileFailed {
        path: manifest_path.clone(),
        source: std::io::Error::new(ErrorKind::InvalidData, source),
    })?;
    let entries = parse_delete_files_manifest(&manifest)?;
    let total_entries = entries.len();
    if total_entries > 0 {
        if let Some(callback) = progress_callback.as_mut() {
            callback(Path::new("."), 0, total_entries);
        }
    }

    for (index, relative) in entries.iter().enumerate() {
        let target_path = dest_root.join(relative);
        match compio::fs::symlink_metadata(&target_path).await {
            Ok(metadata) if metadata.is_dir() => {
                crate::runtime::remove_dir_all(target_path.clone()).await?;
            }
            Ok(_) => {
                compio::fs::remove_file(&target_path)
                    .await
                    .map_err(|source| Error::RemoveFailed {
                        path: target_path.clone(),
                        source,
                    })?;
            }
            Err(source) if source.kind() == ErrorKind::NotFound => {}
            Err(source) => {
                return Err(Error::StatFailed {
                    path: target_path,
                    source,
                })
            }
        }
        if let Some(callback) = progress_callback.as_mut() {
            callback(relative, index + 1, total_entries);
        }
    }

    compio::fs::remove_file(&manifest_path)
        .await
        .map_err(|source| Error::RemoveFailed {
            path: manifest_path,
            source,
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        apply_delete_files_manifest_async, parse_delete_files_entry, DELETE_FILES_MANIFEST_NAME,
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

    #[compio::test]
    async fn apply_delete_files_manifest_removes_listed_files_and_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let dest_root = temp.path().join("install");
        let obsolete_path = dest_root.join("Endfield_Data/Plugins/x86_64/libHAPI.dll");
        compio::fs::create_dir_all(obsolete_path.parent().unwrap())
            .await
            .unwrap();
        compio::fs::write(&obsolete_path, b"obsolete".to_vec())
            .await
            .0
            .unwrap();
        compio::fs::write(
            dest_root.join(DELETE_FILES_MANIFEST_NAME),
            b"Endfield_Data/Plugins/x86_64/libHAPI.dll\n".to_vec(),
        )
        .await
        .0
        .unwrap();

        let mut progress = Vec::new();
        apply_delete_files_manifest_async(
            &dest_root,
            Some(|path: &Path, completed: usize, total: usize| {
                progress.push((path.to_path_buf(), completed, total));
            }),
        )
        .await
        .unwrap();
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
        assert!(matches!(
            compio::fs::metadata(&obsolete_path).await,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound
        ));
        assert!(matches!(
            compio::fs::metadata(dest_root.join(DELETE_FILES_MANIFEST_NAME)).await,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound
        ));
    }
}
