use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use md5::{Digest, Md5};

use crate::error::{Error, Result};
use crate::runtime::preallocate_file;
use crate::runtime::task_pool::fs_ops::commit_partial_download;
use crate::runtime::task_pool::graph::{GraphExpansion, TaskExecution};
use crate::runtime::task_pool::types::{ArchivePart, ArchiveWork, Task, WorkerEvent};
use crate::runtime::task_pool::verify;

const COPY_BUFFER_BYTES: usize = 1024 * 1024;

pub(crate) fn execute_fill_archive_volume_gaps(work: Arc<ArchiveWork>) -> TaskExecution {
    if !work.should_complete_volumes() {
        return TaskExecution::succeeded();
    }
    let requests = match work
        .layout
        .missing_range_requests([work.layout.complete_range()])
    {
        Ok(requests) => requests,
        Err(error) => return TaskExecution::failed(error.to_string()),
    };
    if requests.is_empty() {
        return TaskExecution::then(Task::FinalizeArchiveVolumes { work });
    }

    let mut expansion = GraphExpansion::new();
    let fetches = requests
        .into_iter()
        .map(|request| {
            expansion.add_root(Task::FetchArchiveRange {
                work: work.clone(),
                request,
                retry_count: 0,
            })
        })
        .collect::<Vec<_>>();
    match expansion.add_task(Task::FinalizeArchiveVolumes { work }, fetches) {
        Ok(_) => TaskExecution::expand(expansion),
        Err(error) => TaskExecution::failed(error.to_string()),
    }
}

pub(crate) fn execute_finalize_archive_volumes(
    work: Arc<ArchiveWork>,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
    let result = finalize_archive_volumes(&work, event_tx);
    match result {
        Ok(()) => TaskExecution::succeeded(),
        Err(error) => {
            if matches!(
                &error,
                Error::Integrity(_) | Error::Extraction(_) | Error::Io(_)
            ) {
                work.invalidate_range_cache();
            }
            TaskExecution::failed(error.to_string())
        }
    }
}

fn finalize_archive_volumes(
    work: &ArchiveWork,
    event_tx: &flume::Sender<WorkerEvent>,
) -> Result<()> {
    if !work.should_complete_volumes() {
        return Ok(());
    }
    for (index, part) in work.parts.iter().enumerate() {
        if verify::build_issue(
            &part.dest,
            &part.logical_path,
            &part.expected_md5,
            Some(part.expected_size),
        )
        .is_none()
        {
            report_verified(part, event_tx);
            release_finalized_volume_ranges(work, index);
            continue;
        }
        if let Err(error) = materialize_volume(work, index, part) {
            let _ = event_tx.send(WorkerEvent::Verified {
                path: part.logical_path.clone(),
                ok: false,
                issue: verify::build_issue(
                    &part.dest,
                    &part.logical_path,
                    &part.expected_md5,
                    Some(part.expected_size),
                ),
            });
            return Err(error);
        }
        report_verified(part, event_tx);
        release_finalized_volume_ranges(work, index);
    }
    Ok(())
}

fn materialize_volume(work: &ArchiveWork, index: usize, part: &ArchivePart) -> Result<()> {
    let volume_range = work.layout.volume_range(index).ok_or_else(|| {
        Error::Extraction(format!("archive volume index {index} is out of range"))
    })?;
    let expected_size = volume_range.end - volume_range.start;
    if expected_size != part.expected_size {
        return Err(Error::Extraction(format!(
            "archive volume {} has layout size {expected_size}, expected {}",
            part.logical_path, part.expected_size
        )));
    }
    if !work.layout.range_is_available(&volume_range) {
        return Err(Error::Extraction(format!(
            "archive volume {} is not fully cached before finalization",
            part.logical_path
        )));
    }
    if let Some(parent) = part.dest.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::CreateDirFailed {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let temp = volume_temp_path(&part.dest)?;
    let temp_is_complete = std::fs::metadata(&temp)
        .map(|metadata| metadata.is_file() && metadata.len() == part.expected_size)
        .unwrap_or(false)
        && verify::build_issue(
            &temp,
            &part.logical_path,
            &part.expected_md5,
            Some(part.expected_size),
        )
        .is_none();
    if temp_is_complete {
        commit_partial_download(&temp, &part.dest)?;
        return Ok(());
    }
    match std::fs::remove_file(&temp) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(Error::RemoveFailed { path: temp, source });
        }
    }

    let mut input = work.layout.open_stream()?;
    input.seek(SeekFrom::Start(volume_range.start))?;
    let mut output = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .map_err(|source| Error::OpenFileFailed {
            path: temp.clone(),
            source,
        })?;
    let write_result = (|| -> Result<String> {
        preallocate_file(&output, &temp, part.expected_size)?;
        let mut remaining = part.expected_size;
        let mut hasher = Md5::new();
        let mut buffer = vec![0u8; COPY_BUFFER_BYTES];
        while remaining > 0 {
            let limit = usize::try_from(remaining)
                .unwrap_or(usize::MAX)
                .min(buffer.len());
            let read = input.read(&mut buffer[..limit])?;
            if read == 0 {
                return Err(Error::Extraction(format!(
                    "archive stream ended while finalizing {}",
                    part.logical_path
                )));
            }
            output
                .write_all(&buffer[..read])
                .map_err(|source| Error::WriteFileFailed {
                    path: temp.clone(),
                    source,
                })?;
            hasher.update(&buffer[..read]);
            remaining -= read as u64;
        }
        output.sync_all().map_err(|source| Error::WriteFileFailed {
            path: temp.clone(),
            source,
        })?;
        Ok(crate::to_hex(&hasher.finalize()))
    })();
    drop(output);
    drop(input);

    let actual_md5 = match write_result {
        Ok(actual_md5) => actual_md5,
        Err(error) => {
            let _ = std::fs::remove_file(&temp);
            return Err(error);
        }
    };
    if actual_md5 != part.expected_md5.to_ascii_lowercase() {
        let _ = std::fs::remove_file(&temp);
        return Err(Error::Integrity(format!(
            "archive volume {} MD5 mismatch: expected {}, got {actual_md5}",
            part.logical_path, part.expected_md5
        )));
    }

    if let Err(error) = commit_partial_download(&temp, &part.dest) {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    Ok(())
}

fn release_finalized_volume_ranges(work: &ArchiveWork, finalized_index: usize) {
    let still_needed = ((finalized_index + 1)..work.layout.volume_count())
        .filter_map(|index| work.layout.volume_range(index))
        .collect::<Vec<_>>();
    if let Some(range) = work.layout.volume_range(finalized_index) {
        let _ = work.layout.range_is_available(&range);
    }
    work.layout.prune_range_cache(&still_needed);
}

pub(super) fn volume_temp_path(path: &Path) -> Result<PathBuf> {
    let file_name = path.file_name().ok_or_else(|| {
        Error::InvalidPath(format!(
            "archive volume has no filename: {}",
            path.display()
        ))
    })?;
    Ok(path.with_file_name(format!(
        ".{}.griffr-volume.part",
        file_name.to_string_lossy()
    )))
}

fn report_verified(part: &ArchivePart, event_tx: &flume::Sender<WorkerEvent>) {
    let _ = event_tx.send(WorkerEvent::Verified {
        path: part.logical_path.clone(),
        ok: true,
        issue: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::extractor::{ArchiveRangeRequest, MultiVolumeLayout};
    use crate::runtime::task_pool::types::ArchiveRetention;
    use crate::runtime::PatchApplyOptions;

    #[test]
    fn retained_ranges_are_materialized_as_verified_complete_volumes() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let first_bytes = b"first-volume";
        let second_bytes = b"second-volume";
        let first_dest = temp.path().join("bundle.zip.001");
        let second_dest = temp.path().join("bundle.zip.002");
        let layout = MultiVolumeLayout::from_remote(
            vec![
                (
                    first_dest.clone(),
                    "https://example.invalid/bundle.zip.001".to_string(),
                    first_bytes.len() as u64,
                ),
                (
                    second_dest.clone(),
                    "https://example.invalid/bundle.zip.002".to_string(),
                    second_bytes.len() as u64,
                ),
            ],
            cache.clone(),
        )
        .unwrap();
        for (index, bytes) in [&first_bytes[..], &second_bytes[..]]
            .into_iter()
            .enumerate()
        {
            let cache_path = cache.join(format!("v{index:04}-0-{}.range", bytes.len()));
            std::fs::write(&cache_path, bytes).unwrap();
            let global_start = if index == 0 {
                0
            } else {
                first_bytes.len() as u64
            };
            layout
                .register_range(&ArchiveRangeRequest {
                    volume_index: index,
                    local_range: 0..bytes.len() as u64,
                    global_range: global_start..global_start + bytes.len() as u64,
                    url: "https://example.invalid/archive".to_string(),
                    cache_path,
                })
                .unwrap();
        }
        let parts = vec![
            ArchivePart {
                sequence: 1,
                url: "https://example.invalid/bundle.zip.001".to_string(),
                dest: first_dest.clone(),
                logical_path: "bundle.zip.001".to_string(),
                expected_md5: crate::to_hex(&Md5::digest(first_bytes)),
                expected_size: first_bytes.len() as u64,
            },
            ArchivePart {
                sequence: 2,
                url: "https://example.invalid/bundle.zip.002".to_string(),
                dest: second_dest.clone(),
                logical_path: "bundle.zip.002".to_string(),
                expected_md5: crate::to_hex(&Md5::digest(second_bytes)),
                expected_size: second_bytes.len() as u64,
            },
        ];
        let work = ArchiveWork::new(
            "bundle".to_string(),
            layout,
            vec![None, None],
            temp.path().join("install"),
            ArchiveRetention::KeepCompleteVolumes,
            parts,
            None,
            PatchApplyOptions::default(),
            Arc::new(std::collections::BTreeMap::new()),
        )
        .unwrap();
        let (event_tx, _event_rx) = flume::unbounded();

        finalize_archive_volumes(&work, &event_tx).unwrap();

        assert_eq!(std::fs::read(first_dest).unwrap(), first_bytes);
        assert_eq!(std::fs::read(second_dest).unwrap(), second_bytes);
        assert!(
            cache.exists(),
            "cache directory is removed by cleanup, not finalization"
        );
        assert!(
            std::fs::read_dir(cache).unwrap().all(|entry| entry
                .unwrap()
                .path()
                .extension()
                .and_then(|value| value.to_str())
                != Some("range")),
            "finalized volume ranges should be released as complete files are promoted"
        );
    }
}
