use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::runtime::{PATCH_MANIFEST_NAME, PATCH_STAGE_DIR};

use crate::api::types::ResourcePatch;

use super::path_safety::parse_safe_relative_path;

mod materialize;

fn resolve_patch_stage_path(
    install_root: &Path,
    stage_root: &Path,
    stage_subdir: &str,
    label: &str,
    raw: &str,
) -> Result<PathBuf> {
    let relative = parse_safe_relative_path(label, raw)?;
    let subdir_path = Path::new(stage_subdir);
    if relative.starts_with(PATCH_STAGE_DIR) {
        return Ok(install_root.join(relative));
    }
    if relative.starts_with(subdir_path) {
        return Ok(stage_root.join(relative));
    }
    Ok(stage_root.join(stage_subdir).join(relative))
}

pub(crate) fn apply_extracted_vfs_patch_manifest(
    install_root: &Path,
    mut progress_callback: Option<&mut dyn FnMut(&str, usize, usize)>,
) -> Result<()> {
    let manifest_path = install_root.join(PATCH_MANIFEST_NAME);
    let stage_root = install_root.join(PATCH_STAGE_DIR);
    if !manifest_path.exists() && !stage_root.exists() {
        return Ok(());
    }
    if !manifest_path.is_file() {
        return Err(Error::Vfs(format!(
            "Extracted VFS patch manifest is incomplete: missing {}",
            manifest_path.display()
        )));
    }

    let manifest: ResourcePatch =
        serde_json::from_slice(&std::fs::read(&manifest_path).map_err(|e| {
            Error::OpenFileFailed {
                path: manifest_path.clone(),
                source: e,
            }
        })?)?;

    let vfs_base_path =
        parse_safe_relative_path("patch.json vfs_base_path", manifest.vfs_base_path.trim())?;
    let dest_root = install_root.join(vfs_base_path);

    let total_entries = manifest.files.len();
    if total_entries > 0 {
        if let Some(cb) = progress_callback.as_deref_mut() {
            cb("", 0, total_entries);
        }
    }
    for (index, entry) in manifest.files.iter().enumerate() {
        materialize::materialize_vfs_patch_entry(install_root, &stage_root, &dest_root, entry)
            .map_err(|e| {
                Error::Other(format!(
                    "Failed to materialize patch entry {}: {e}",
                    entry.name
                ))
            })?;
        if let Some(cb) = progress_callback.as_deref_mut() {
            cb(&entry.name, index + 1, total_entries);
        }
    }

    std::fs::remove_file(&manifest_path).map_err(|e| Error::RemoveFailed {
        path: manifest_path.clone(),
        source: e,
    })?;
    if stage_root.exists() {
        std::fs::remove_dir_all(&stage_root).map_err(|e| Error::RemoveFailed {
            path: stage_root.clone(),
            source: e,
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::apply_extracted_vfs_patch_manifest;
    use crate::runtime::{PATCH_MANIFEST_NAME, PATCH_STAGE_DIR};

    #[test]
    fn apply_extracted_vfs_patch_manifest_moves_local_files_and_cleans_staging() {
        let temp = tempfile::tempdir().unwrap();
        let install_root = temp.path().join("install");
        let staged_file = install_root
            .join(PATCH_STAGE_DIR)
            .join("files")
            .join("ui")
            .join("direct.ab");
        std::fs::create_dir_all(staged_file.parent().unwrap()).unwrap();
        std::fs::write(&staged_file, b"patched bytes").unwrap();
        std::fs::write(
            install_root.join(PATCH_MANIFEST_NAME),
            r#"{
  "version": "75.0.0",
  "vfs_base_path": "Arknights_Data/StreamingAssets/AB/Windows",
  "files": [
    {
      "name": "ui/direct.ab",
      "md5": "75c4e133155014e946c3ef39652b0ba8",
      "size": 13,
      "local_path": "files/ui/direct.ab",
      "diffType": 0,
      "patch": []
    }
  ]
}"#,
        )
        .unwrap();

        let mut progress = Vec::new();
        let mut on_progress = |path: &str, completed: usize, total: usize| {
            progress.push((path.to_string(), completed, total));
        };
        apply_extracted_vfs_patch_manifest(&install_root, Some(&mut on_progress)).unwrap();

        assert_eq!(
            progress,
            vec![("".to_string(), 0, 1), ("ui/direct.ab".to_string(), 1, 1),]
        );
        assert_eq!(
            std::fs::read(
                install_root.join("Arknights_Data/StreamingAssets/AB/Windows/ui/direct.ab")
            )
            .unwrap(),
            b"patched bytes"
        );
        assert!(!install_root.join(PATCH_MANIFEST_NAME).exists());
        assert!(!install_root.join(PATCH_STAGE_DIR).exists());
    }
}
