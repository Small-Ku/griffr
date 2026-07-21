use std::path::{Path, PathBuf};
use std::sync::Arc;

use md5::{Digest, Md5};

use super::complete::volume_temp_path;
use crate::api::types::GameFileEntry;
use crate::download::extractor::MultiVolumeLayout;
use crate::error::{Error, Result};
use crate::runtime::task_pool::fs_ops::{commit_partial_download, make_partial_download_path};
use crate::runtime::task_pool::graph::TaskExecution;
use crate::runtime::task_pool::types::{ArchivePart, ArchiveRetention, ArchiveWork, Task};
use crate::runtime::task_pool::verify;
use crate::runtime::PatchApplyOptions;

pub(crate) fn execute_install_archive(
    base_name: String,
    dest: PathBuf,
    retention: ArchiveRetention,
    password: Option<String>,
    patch_options: PatchApplyOptions,
    expected_files: Arc<std::collections::BTreeMap<String, GameFileEntry>>,
    mut parts: Vec<ArchivePart>,
) -> TaskExecution {
    let result = (|| -> Result<_> {
        parts.sort_by(|left, right| {
            left.sequence
                .cmp(&right.sequence)
                .then_with(|| left.logical_path.cmp(&right.logical_path))
        });
        if parts.is_empty() {
            return Err(Error::TaskPool("install archive has no parts".to_string()));
        }

        prepare_trusted_archive_files(&parts)?;
        let archive_parent = parts[0]
            .dest
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| dest.clone());
        let cache_key = base_name
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>();
        let mut identity = Md5::new();
        for part in &parts {
            identity.update(part.expected_md5.as_bytes());
            identity.update(part.expected_size.to_le_bytes());
        }
        let identity = crate::to_hex(&identity.finalize());
        let cache_dir = archive_parent
            .join(".griffr-range-cache")
            .join(format!("{cache_key}-{}", &identity[..16]));
        let layout = MultiVolumeLayout::from_remote(
            parts
                .iter()
                .map(|part| (part.dest.clone(), part.url.clone(), part.expected_size))
                .collect(),
            cache_dir,
        )?;
        ArchiveWork::new(
            base_name,
            layout.clone(),
            vec![None; layout.volume_count()],
            dest,
            retention,
            parts,
            password,
            patch_options,
            expected_files,
        )
    })();

    match result {
        Ok(work) => TaskExecution::then(Task::DiscoverArchiveDirectory {
            work,
            required_range: None,
        }),
        Err(error) => TaskExecution::failed(error.to_string()),
    }
}

fn prepare_trusted_archive_files(parts: &[ArchivePart]) -> Result<()> {
    for part in parts {
        let legacy_partial = make_partial_download_path(&part.dest)?;
        let volume_partial = volume_temp_path(&part.dest)?;
        if is_trusted_archive_file(&part.dest, part)? {
            remove_file_if_exists(&legacy_partial)?;
            remove_file_if_exists(&volume_partial)?;
            continue;
        }

        let mut promoted = false;
        for candidate in [&legacy_partial, &volume_partial] {
            if is_trusted_archive_file(candidate, part)? {
                commit_partial_download(candidate, &part.dest)?;
                promoted = true;
                break;
            }
        }
        remove_file_if_exists(&legacy_partial)?;
        remove_file_if_exists(&volume_partial)?;
        if promoted {
            continue;
        }
        remove_file_if_exists(&part.dest)?;
    }
    Ok(())
}

fn is_trusted_archive_file(path: &Path, part: &ArchivePart) -> Result<bool> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(Error::StatFailed {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if !metadata.is_file() || metadata.len() != part.expected_size {
        return Ok(false);
    }
    Ok(verify::file_md5(path)? == part.expected_md5.to_ascii_lowercase())
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::RemoveFailed {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_volume_partial_is_promoted_before_range_planning() {
        let temp = tempfile::tempdir().unwrap();
        let bytes = b"complete-volume";
        let dest = temp.path().join("bundle.zip.001");
        let retained_partial = volume_temp_path(&dest).unwrap();
        std::fs::write(&retained_partial, bytes).unwrap();
        let part = ArchivePart {
            sequence: 1,
            url: "https://example.invalid/bundle.zip.001".to_string(),
            dest: dest.clone(),
            logical_path: "bundle.zip.001".to_string(),
            expected_md5: crate::to_hex(&Md5::digest(bytes)),
            expected_size: bytes.len() as u64,
        };

        prepare_trusted_archive_files(&[part]).unwrap();

        assert_eq!(std::fs::read(dest).unwrap(), bytes);
        assert!(!retained_partial.exists());
    }
}
