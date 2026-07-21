use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use md5::{Digest, Md5};

use super::save_volumes::volume_temp_path;
use crate::api::types::GameFileEntry;
use crate::download::extractor::MultiVolumeLayout;
use crate::error::{Error, Result};
use crate::runtime::task_pool::fs_ops::{commit_partial_download, make_partial_download_path};
use crate::runtime::task_pool::graph::TaskRun;
use crate::runtime::task_pool::types::{
    ArchivePart, ArchiveRepairSession, ArchiveRetention, ArchiveSource, ArchiveWork, Task,
};
use crate::runtime::task_pool::verify;
use crate::runtime::PatchApplyOptions;

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_open_archive(
    base_name: String,
    source: ArchiveSource,
    dest: PathBuf,
    retention: ArchiveRetention,
    password: Option<String>,
    patch_options: PatchApplyOptions,
    expected_files: Arc<std::collections::BTreeMap<String, GameFileEntry>>,
    excluded_commit_paths: Arc<std::collections::BTreeSet<String>>,
) -> TaskRun {
    let result = match source {
        ArchiveSource::Remote(parts) => prepare_remote_archive_work(
            base_name,
            parts,
            dest,
            retention,
            password,
            patch_options,
            expected_files,
            excluded_commit_paths,
        ),
        ArchiveSource::Local(volumes) => {
            if volumes.is_empty() {
                Err(Error::Message {
                    context: "Task pool error: ",
                    detail: "local archive has no volumes".to_string(),
                })
            } else {
                MultiVolumeLayout::from_files(volumes).and_then(|layout| {
                    ArchiveWork::new(
                        base_name,
                        layout.clone(),
                        vec![None; layout.volume_count()],
                        dest,
                        retention,
                        Vec::new(),
                        password,
                        patch_options,
                        expected_files,
                        excluded_commit_paths,
                    )
                })
            }
        }
    };

    match result {
        Ok(work) => TaskRun::then(Task::DiscoverArchiveDirectory {
            work,
            required_range: None,
        }),
        Err(error) => TaskRun::failed(error.to_string()),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_remote_archive_work(
    base_name: String,
    parts: Vec<ArchivePart>,
    dest: PathBuf,
    retention: ArchiveRetention,
    password: Option<String>,
    patch_options: PatchApplyOptions,
    expected_files: Arc<std::collections::BTreeMap<String, GameFileEntry>>,
    excluded_commit_paths: Arc<std::collections::BTreeSet<String>>,
) -> Result<Arc<ArchiveWork>> {
    let parts = sorted_remote_parts(parts)?;
    prepare_trusted_archive_files(&parts)?;
    let cache_dir = remote_cache_dir(&base_name, &parts, &dest);
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
        excluded_commit_paths,
    )
}

/// Builds an ephemeral range-only view for integrity repair. Existing full
/// volume files are deliberately not accepted by size alone and are not
/// pre-hashed, so repair startup stays bounded by archive metadata rather than
/// the total retained package size. Repair uses an isolated ephemeral cache,
/// so cleanup cannot remove install or update resume ranges for the same pack.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_remote_archive_range_work(
    base_name: String,
    parts: Vec<ArchivePart>,
    dest: PathBuf,
    password: Option<String>,
    patch_options: PatchApplyOptions,
    expected_files: Arc<std::collections::BTreeMap<String, GameFileEntry>>,
    excluded_commit_paths: Arc<std::collections::BTreeSet<String>>,
    session: Weak<ArchiveRepairSession>,
    group_index: usize,
) -> Result<Arc<ArchiveWork>> {
    let parts = sorted_remote_parts(parts)?;
    let cache_dir = remote_cache_dir(&base_name, &parts, &dest).join("repair");
    let layout = MultiVolumeLayout::from_remote(
        parts
            .iter()
            .enumerate()
            .map(|(index, part)| {
                (
                    cache_dir.join(format!("volume-{index:04}.full")),
                    part.url.clone(),
                    part.expected_size,
                )
            })
            .collect(),
        cache_dir,
    )?;
    ArchiveWork::new_repair(
        base_name,
        layout.clone(),
        vec![None; layout.volume_count()],
        dest,
        parts,
        password,
        patch_options,
        expected_files,
        excluded_commit_paths,
        session,
        group_index,
    )
}

fn sorted_remote_parts(mut parts: Vec<ArchivePart>) -> Result<Vec<ArchivePart>> {
    parts.sort_by(|left, right| {
        left.sequence
            .cmp(&right.sequence)
            .then_with(|| left.logical_path.cmp(&right.logical_path))
    });
    if parts.is_empty() {
        return Err(Error::Message {
            context: "Task pool error: ",
            detail: "remote archive has no parts".to_string(),
        });
    }
    Ok(parts)
}

fn remote_cache_dir(base_name: &str, parts: &[ArchivePart], dest: &Path) -> PathBuf {
    let archive_parent = parts[0]
        .dest
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| dest.to_path_buf());
    let cache_key = safe_cache_key(base_name);
    let identity = archive_identity(parts);
    archive_parent
        .join(".griffr-range-cache")
        .join(format!("{cache_key}-{}", &identity[..16]))
}

fn safe_cache_key(base_name: &str) -> String {
    base_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn archive_identity(parts: &[ArchivePart]) -> String {
    let mut identity = Md5::new();
    for part in parts {
        identity.update(part.expected_md5.as_bytes());
        identity.update(part.expected_size.to_le_bytes());
    }
    crate::to_hex(&identity.finalize())
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
            return Err(Error::IoAt {
                action: "query file metadata/stat for",
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
        Err(source) => Err(Error::IoAt {
            action: "remove file or directory",
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_volume_partial_is_promoted_before_range_planning() {
        let temp = tempfile::tempdir().unwrap();
        let bytes = b"full-volume";
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

    #[test]
    fn repair_range_work_does_not_scan_or_trust_retained_full_volume() {
        let temp = tempfile::tempdir().unwrap();
        let bytes = b"retained-volume";
        let dest = temp.path().join("bundle.zip.001");
        std::fs::write(&dest, bytes).unwrap();
        let part = ArchivePart {
            sequence: 1,
            url: "https://example.invalid/bundle.zip.001".to_string(),
            dest: dest.clone(),
            logical_path: "bundle.zip.001".to_string(),
            expected_md5: crate::to_hex(&Md5::digest(bytes)),
            expected_size: bytes.len() as u64,
        };

        let work = prepare_remote_archive_range_work(
            "bundle".to_string(),
            vec![part],
            temp.path().join("game"),
            None,
            PatchApplyOptions::default(),
            Arc::new(std::collections::BTreeMap::new()),
            Arc::new(std::collections::BTreeSet::new()),
            Arc::downgrade(&ArchiveRepairSession::new(
                Vec::new(),
                temp.path().join("game"),
                Arc::new(std::collections::BTreeMap::new()),
            )),
            0,
        )
        .unwrap();

        assert!(!work.layout.range_is_available(&(0..bytes.len() as u64)));
        let requests = work
            .layout
            .missing_range_requests([0..bytes.len() as u64])
            .unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]
                .cache_path
                .parent()
                .unwrap()
                .file_name()
                .unwrap(),
            "repair"
        );
        assert_eq!(std::fs::read(dest).unwrap(), bytes);
    }
}
