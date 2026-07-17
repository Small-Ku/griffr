use std::path::PathBuf;

use crate::error::Error;
use crate::runtime::{build_patch_execution_plan_with_cache, PatchApplyOptions};
use compio::dispatcher::Dispatcher;

use super::super::fs_ops::{
    commit_staged_extract, execute_patch_transaction, make_extract_staging_dir,
};
use super::super::types::{
    ArchiveInstallGroup, ArchivePart, ArchiveShardTask, ArchiveWork, PreparedArchive, Task,
    WorkerEvent,
};
use super::super::verify::VerifiedArtifactCache;

pub(super) fn execute_install_archive(
    base_name: String,
    dest: PathBuf,
    cleanup: bool,
    password: Option<String>,
    patch_options: PatchApplyOptions,
    mut parts: Vec<ArchivePart>,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<WorkerEvent>,
) {
    parts.sort_by(|left, right| {
        left.sequence
            .cmp(&right.sequence)
            .then_with(|| left.logical_path.cmp(&right.logical_path))
    });
    if parts.is_empty() {
        let _ = event_tx.send(WorkerEvent::Failed {
            path: base_name,
            reason: "install archive has no parts".to_string(),
        });
        return;
    }

    let volumes = parts.iter().map(|part| part.dest.clone()).collect();
    let continuation = Task::Extract {
        base_name,
        volumes,
        dest,
        cleanup,
        password,
        patch_options,
    };
    let group = ArchiveInstallGroup::new(parts.len(), continuation);
    spawned.extend(parts.into_iter().map(|part| Task::InstallArchivePart {
        part,
        group: group.clone(),
        retry_count: 0,
    }));
}

pub(super) fn execute_install_archive_part(
    part: ArchivePart,
    group: std::sync::Arc<ArchiveInstallGroup>,
    retry_count: u32,
    max_retries: u32,
    io_dispatcher: Option<&Dispatcher>,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<WorkerEvent>,
) {
    if super::super::verify::build_issue(
        &part.dest,
        &part.logical_path,
        &part.expected_md5,
        Some(part.expected_size),
    )
    .is_none()
    {
        let _ = event_tx.send(WorkerEvent::Verified {
            path: part.logical_path.clone(),
            ok: true,
            issue: None,
        });
        group.finish_part(true, spawned);
        return;
    }

    match super::super::download::prepare_download(
        io_dispatcher,
        &part.dest,
        &part.expected_md5,
        Some(part.expected_size),
    ) {
        Ok(super::super::download::DownloadPreparation::Complete(bytes)) => {
            let _ = event_tx.send(WorkerEvent::Downloaded {
                path: part.logical_path.clone(),
                bytes,
            });
            let _ = event_tx.send(WorkerEvent::Verified {
                path: part.logical_path.clone(),
                ok: true,
                issue: None,
            });
            group.finish_part(true, spawned);
        }
        Ok(super::super::download::DownloadPreparation::Ready(resume)) => {
            spawned.push(Task::TransferArchivePart {
                part,
                group,
                retry_count,
                resume,
            });
        }
        Err(error) if retry_count < max_retries => {
            let _ = event_tx.send(WorkerEvent::Retried {
                path: part.logical_path.clone(),
                reason: format!(
                    "install-archive preparation attempt {} failed: {}",
                    retry_count + 1,
                    error
                ),
            });
            spawned.push(Task::InstallArchivePart {
                part,
                group,
                retry_count: retry_count + 1,
            });
        }
        Err(error) => {
            let _ = event_tx.send(WorkerEvent::Verified {
                path: part.logical_path.clone(),
                ok: false,
                issue: super::super::verify::build_issue(
                    &part.dest,
                    &part.logical_path,
                    &part.expected_md5,
                    Some(part.expected_size),
                ),
            });
            let _ = event_tx.send(WorkerEvent::Failed {
                path: part.logical_path.clone(),
                reason: format!(
                    "install-archive preparation failed after retries: {}",
                    error
                ),
            });
            group.finish_part(false, spawned);
        }
    }
}

pub(super) fn execute_transfer_archive_part(
    part: ArchivePart,
    group: std::sync::Arc<ArchiveInstallGroup>,
    retry_count: u32,
    resume: super::super::types::DownloadResumeState,
    max_retries: u32,
    download_progress_buffer_bytes: usize,
    io_dispatcher: Option<&Dispatcher>,
    http_client: &cyper::Client,
    user_agent: &str,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<WorkerEvent>,
) {
    let event_tx_clone = event_tx.clone();
    let logical_path_clone = part.logical_path.clone();
    let expected_size = part.expected_size;
    let _ = event_tx.send(WorkerEvent::DownloadStarted {
        path: part.logical_path.clone(),
        total_bytes: expected_size,
    });
    match super::super::download::do_prepared_download(
        io_dispatcher,
        http_client,
        user_agent,
        &part.url,
        &part.dest,
        &part.expected_md5,
        Some(part.expected_size),
        resume,
        download_progress_buffer_bytes,
        Some(move |progress| match progress {
            super::super::download::DownloadProgress::Advanced(bytes) => {
                let _ = event_tx_clone.send(WorkerEvent::DownloadedBytes {
                    path: logical_path_clone.clone(),
                    bytes,
                    total_bytes: expected_size,
                });
            }
            super::super::download::DownloadProgress::Reset(bytes) => {
                let _ = event_tx_clone.send(WorkerEvent::DownloadReset {
                    path: logical_path_clone.clone(),
                    bytes,
                });
            }
        }),
    ) {
        Ok(bytes) => {
            let _ = event_tx.send(WorkerEvent::Downloaded {
                path: part.logical_path.clone(),
                bytes,
            });
            let _ = event_tx.send(WorkerEvent::Verified {
                path: part.logical_path.clone(),
                ok: true,
                issue: None,
            });
            group.finish_part(true, spawned);
        }
        Err(error) if retry_count < max_retries => {
            let _ = event_tx.send(WorkerEvent::Retried {
                path: part.logical_path.clone(),
                reason: format!(
                    "install-archive download attempt {} failed: {}",
                    retry_count + 1,
                    error
                ),
            });
            spawned.push(Task::InstallArchivePart {
                part,
                group,
                retry_count: retry_count + 1,
            });
        }
        Err(error) => {
            let _ = event_tx.send(WorkerEvent::Verified {
                path: part.logical_path.clone(),
                ok: false,
                issue: super::super::verify::build_issue(
                    &part.dest,
                    &part.logical_path,
                    &part.expected_md5,
                    Some(part.expected_size),
                ),
            });
            let _ = event_tx.send(WorkerEvent::Failed {
                path: part.logical_path.clone(),
                reason: format!("install-archive download failed after retries: {}", error),
            });
            group.finish_part(false, spawned);
        }
    }
}

pub(super) fn execute_schedule_extract(
    base_name: String,
    volumes: Vec<PathBuf>,
    dest: PathBuf,
    cleanup: bool,
    password: Option<String>,
    patch_options: PatchApplyOptions,
    spawned: &mut Vec<Task>,
) {
    spawned.push(Task::PrepareArchive {
        work: ArchiveWork::new(base_name, volumes, dest, cleanup, password, patch_options),
    });
}

pub(super) fn execute_prepare_archive(
    work: std::sync::Arc<ArchiveWork>,
    extract_shards: usize,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<WorkerEvent>,
) {
    let result = (|| {
        let extractor =
            crate::download::extractor::MultiVolumeExtractor::new(work.volumes.clone())?;
        let patch_options = work.patch_options.resolved_for_install(&work.dest)?;
        let staging_dir = make_extract_staging_dir(
            &work.dest,
            &work.base_name,
            patch_options.work_dir.as_deref(),
        )?;
        std::fs::create_dir_all(&staging_dir).map_err(|source| Error::CreateDirFailed {
            path: staging_dir.clone(),
            source,
        })?;

        let inspection = std::sync::Arc::new(
            extractor.inspect_patch_payload(work.password.as_deref())?,
        );
        let verification_cache = VerifiedArtifactCache::default();
        let patch_plan = if inspection.patch_manifest.is_some() {
            Some(build_patch_execution_plan_with_cache(
                &work.dest,
                &staging_dir,
                &inspection,
                &patch_options,
                &verification_cache,
            )?)
        } else {
            None
        };
        if let Some((_, report)) = patch_plan.as_ref() {
            let _ = event_tx.send(WorkerEvent::ArchivePreflight {
                path: work.base_name.clone(),
                report: report.clone(),
            });
        }

        *work.prepared.lock().unwrap() = Some(PreparedArchive {
            staging_dir: staging_dir.clone(),
            inspection: inspection.clone(),
            patch_plan,
        });
        let ranges = crate::download::extractor::MultiVolumeExtractor::extraction_ranges(
            &inspection,
            extract_shards,
        );
        let _ = event_tx.send(WorkerEvent::ExtractedBytes {
            path: work.base_name.clone(),
            bytes: 0,
            total_bytes: inspection.total_uncompressed_bytes,
        });
        if ranges.is_empty() {
            spawned.push(Task::CommitArchive { work: work.clone() });
            return Ok(());
        }

        let group = super::super::types::ArchiveExtractionGroup::new(
            ranges.len(),
            Task::CommitArchive { work: work.clone() },
        );
        spawned.extend(ranges.into_iter().map(|range| Task::ExtractArchiveShard {
            shard: ArchiveShardTask {
                work: work.clone(),
                inspection: inspection.clone(),
                staging_dir: staging_dir.clone(),
                range,
                group: group.clone(),
            },
        }));
        Ok(())
    })();

    if let Err(error) = result {
        if let Some(prepared) = work.prepared.lock().unwrap().take() {
            let _ = std::fs::remove_dir_all(prepared.staging_dir);
        }
        let _ = event_tx.send(WorkerEvent::Failed {
            path: work.base_name.clone(),
            reason: error.to_string(),
        });
    }
}

pub(super) fn execute_extract_archive_shard(
    shard: ArchiveShardTask,
    extraction_progress_buffer_bytes: usize,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<WorkerEvent>,
) {
    let work = shard.work.clone();
    let inspection = shard.inspection.clone();
    let staging_dir = shard.staging_dir.clone();
    let range = shard.range.clone();
    let group = shard.group.clone();
    let extractor = crate::download::extractor::MultiVolumeExtractor::new(work.volumes.clone());
    let result = extractor.and_then(|extractor| {
        extractor.extract_range_with_progress(
            &staging_dir,
            work.password.as_deref(),
            &inspection,
            range,
            extraction_progress_buffer_bytes,
            |bytes| {
                let extracted = work
                    .extracted_bytes
                    .fetch_add(bytes, std::sync::atomic::Ordering::AcqRel)
                    .saturating_add(bytes);
                let _ = event_tx.send(WorkerEvent::ExtractedBytes {
                    path: work.base_name.clone(),
                    bytes: extracted.min(inspection.total_uncompressed_bytes),
                    total_bytes: inspection.total_uncompressed_bytes,
                });
            },
        )
    });

    let succeeded = result.is_ok();
    if let Err(error) = result {
        if group.record_failure() {
            let _ = event_tx.send(WorkerEvent::Failed {
                path: work.base_name.clone(),
                reason: error.to_string(),
            });
        }
    }
    let last_failed = group.finish_shard(succeeded, spawned);
    if last_failed {
        let _ = std::fs::remove_dir_all(&staging_dir);
        work.prepared.lock().unwrap().take();
    } else if succeeded && !group.is_failed() {
        let extracted = work.extracted_bytes.load(std::sync::atomic::Ordering::Acquire);
        if extracted >= inspection.total_uncompressed_bytes {
            let _ = event_tx.send(WorkerEvent::ExtractedBytes {
                path: work.base_name.clone(),
                bytes: inspection.total_uncompressed_bytes,
                total_bytes: inspection.total_uncompressed_bytes,
            });
        }
    }
}

pub(super) fn execute_commit_archive(
    work: std::sync::Arc<ArchiveWork>,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<WorkerEvent>,
) {
    let prepared = match work.prepared.lock().unwrap().clone() {
        Some(prepared) => prepared,
        None => {
            let _ = event_tx.send(WorkerEvent::Failed {
                path: work.base_name.clone(),
                reason: "archive commit started without prepared state".to_string(),
            });
            return;
        }
    };
    let result = (|| {
        let mut on_commit = |path: &std::path::Path, completed: usize, total: usize| {
            let normalized = path.to_string_lossy().replace('\\', "/");
            if completed > 0 {
                let _ = event_tx.send(WorkerEvent::Changed {
                    path: normalized.clone(),
                });
            }
            let _ = event_tx.send(WorkerEvent::ArchiveCommitProgress {
                path: normalized,
                completed,
                total,
            });
        };
        let mut on_patch = |path: &str, completed: usize, total: usize| {
            if completed > 0 {
                let _ = event_tx.send(WorkerEvent::Changed {
                    path: path.replace('\\', "/"),
                });
            }
            let _ = event_tx.send(WorkerEvent::PatchProgress {
                path: path.to_string(),
                completed,
                total,
            });
        };
        let mut on_delete = |path: &std::path::Path, completed: usize, total: usize| {
            let normalized = path.to_string_lossy().replace('\\', "/");
            if completed > 0 {
                let _ = event_tx.send(WorkerEvent::Changed {
                    path: normalized.clone(),
                });
            }
            let _ = event_tx.send(WorkerEvent::DeleteProgress {
                path: normalized,
                completed,
                total,
            });
        };
        let verification_cache = VerifiedArtifactCache::default();
        if let Some((plan, report)) = prepared.patch_plan.clone() {
            // Patch entries are dependency ordered. Run each wave serially here;
            // concurrency belongs to the task scheduler rather than nested threads.
            execute_patch_transaction(
                &plan,
                Some(&report),
                Some(&mut on_commit),
                Some(&mut on_patch),
                Some(&mut on_delete),
                1,
                1,
                &verification_cache,
            )?;
            if prepared.staging_dir.exists() {
                std::fs::remove_dir_all(&prepared.staging_dir).map_err(|source| {
                    Error::RemoveFailed {
                        path: prepared.staging_dir.clone(),
                        source,
                    }
                })?;
            }
        } else {
            commit_staged_extract(
                &prepared.staging_dir,
                &work.dest,
                1,
                Some(&mut on_commit),
            )?;
            spawned.push(Task::ApplyExtractedVfsPatchManifest {
                install_root: work.dest.clone(),
            });
        }
        Ok(())
    })();

    match result {
        Ok(()) => {
            work.prepared.lock().unwrap().take();
            spawned.push(Task::CleanupArchive { work });
        }
        Err(error) => {
            let _ = std::fs::remove_dir_all(&prepared.staging_dir);
            work.prepared.lock().unwrap().take();
            let _ = event_tx.send(WorkerEvent::Failed {
                path: work.base_name.clone(),
                reason: error.to_string(),
            });
        }
    }
}

pub(super) fn execute_cleanup_archive(
    work: std::sync::Arc<ArchiveWork>,
    event_tx: &flume::Sender<WorkerEvent>,
) {
    let result = if work.cleanup {
        crate::download::extractor::MultiVolumeExtractor::new(work.volumes.clone())
            .and_then(|extractor| extractor.cleanup())
    } else {
        Ok(())
    };
    match result {
        Ok(()) => {
            let _ = event_tx.send(WorkerEvent::Extracted {
                path: work.dest.clone(),
            });
        }
        Err(error) => {
            let _ = event_tx.send(WorkerEvent::Failed {
                path: work.base_name.clone(),
                reason: error.to_string(),
            });
        }
    }
}
