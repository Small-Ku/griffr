use std::path::PathBuf;

use crate::error::Error;
use crate::runtime::{build_patch_execution_plan_with_cache, PatchApplyOptions};

use super::super::fs_ops::{
    commit_staged_extract, execute_patch_transaction, make_extract_staging_dir,
};
use super::super::graph::{GraphExpansion, TaskExecution};
use super::super::types::{
    ArchivePart, ArchiveShardTask, ArchiveWork, PreparedArchive, Task, WorkerEvent,
};
use super::super::verify::VerifiedArtifactCache;

pub(super) fn execute_install_archive(
    base_name: String,
    dest: PathBuf,
    cleanup: bool,
    password: Option<String>,
    patch_options: PatchApplyOptions,
    mut parts: Vec<ArchivePart>,
) -> TaskExecution {
    parts.sort_by(|left, right| {
        left.sequence
            .cmp(&right.sequence)
            .then_with(|| left.logical_path.cmp(&right.logical_path))
    });
    if parts.is_empty() {
        return TaskExecution::failed("install archive has no parts");
    }

    let volumes = parts.iter().map(|part| part.dest.clone()).collect();
    let mut expansion = GraphExpansion::new();
    let part_nodes = parts
        .into_iter()
        .map(|part| {
            expansion.add_root(Task::InstallArchivePart {
                part,
                retry_count: 0,
            })
        })
        .collect::<Vec<_>>();
    let extract = Task::Extract {
        base_name,
        volumes,
        dest,
        cleanup,
        password,
        patch_options,
    };
    match expansion.add_task(extract, part_nodes) {
        Ok(_) => TaskExecution::expand(expansion),
        Err(error) => TaskExecution::failed(error.to_string()),
    }
}

pub(super) fn execute_install_archive_part(
    part: ArchivePart,
    retry_count: u32,
    max_retries: u32,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
    if super::super::verify::build_issue(
        &part.dest,
        &part.logical_path,
        &part.expected_md5,
        Some(part.expected_size),
    )
    .is_none()
    {
        let _ = event_tx.send(WorkerEvent::Verified {
            path: part.logical_path,
            ok: true,
            issue: None,
        });
        return TaskExecution::succeeded();
    }

    match super::super::download::prepare_download(
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
                path: part.logical_path,
                ok: true,
                issue: None,
            });
            TaskExecution::succeeded()
        }
        Ok(super::super::download::DownloadPreparation::Ready(resume)) => {
            TaskExecution::then(Task::TransferArchivePart {
                part,
                retry_count,
                resume,
            })
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
            TaskExecution::then(Task::InstallArchivePart {
                part,
                retry_count: retry_count + 1,
            })
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
            TaskExecution::failed(format!(
                "install-archive preparation failed after retries: {error}"
            ))
        }
    }
}

pub(super) async fn execute_transfer_archive_part(
    part: ArchivePart,
    retry_count: u32,
    resume: super::super::types::DownloadResumeState,
    max_retries: u32,
    download_progress_buffer_bytes: usize,
    user_agent: &str,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
    let event_tx_clone = event_tx.clone();
    let logical_path_clone = part.logical_path.clone();
    let expected_size = part.expected_size;
    let _ = event_tx.send(WorkerEvent::DownloadStarted {
        path: part.logical_path.clone(),
        total_bytes: expected_size,
    });
    match super::super::download::do_prepared_download(
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
    )
    .await
    {
        Ok(bytes) => {
            let _ = event_tx.send(WorkerEvent::Downloaded {
                path: part.logical_path.clone(),
                bytes,
            });
            let _ = event_tx.send(WorkerEvent::Verified {
                path: part.logical_path,
                ok: true,
                issue: None,
            });
            TaskExecution::succeeded()
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
            TaskExecution::then(Task::InstallArchivePart {
                part,
                retry_count: retry_count + 1,
            })
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
            TaskExecution::failed(format!(
                "install-archive download failed after retries: {error}"
            ))
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
) -> TaskExecution {
    TaskExecution::then(Task::PrepareArchive {
        work: ArchiveWork::new(base_name, volumes, dest, cleanup, password, patch_options),
    })
}

pub(super) fn execute_prepare_archive(
    work: std::sync::Arc<ArchiveWork>,
    extract_shards: usize,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
    let result: Result<GraphExpansion, Error> = (|| {
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

        let inspection =
            std::sync::Arc::new(extractor.inspect_patch_payload(work.password.as_deref())?);
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

        let mut expansion = GraphExpansion::new();
        if ranges.is_empty() {
            expansion.add_root(Task::CommitArchive { work: work.clone() });
            return Ok(expansion);
        }

        let group = super::super::types::ArchiveExtractionGroup::new(ranges.len());
        let shard_nodes = ranges
            .into_iter()
            .map(|range| {
                expansion.add_root(Task::ExtractArchiveShard {
                    shard: ArchiveShardTask {
                        work: work.clone(),
                        inspection: inspection.clone(),
                        staging_dir: staging_dir.clone(),
                        range,
                        group: group.clone(),
                    },
                })
            })
            .collect::<Vec<_>>();
        expansion.add_task(Task::CommitArchive { work: work.clone() }, shard_nodes)?;
        Ok(expansion)
    })();

    match result {
        Ok(expansion) => TaskExecution::expand(expansion),
        Err(error) => {
            if let Some(prepared) = work.prepared.lock().unwrap().take() {
                let _ = std::fs::remove_dir_all(prepared.staging_dir);
            }
            TaskExecution::failed(error.to_string())
        }
    }
}

pub(super) fn execute_extract_archive_shard(
    shard: ArchiveShardTask,
    extraction_progress_buffer_bytes: usize,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
    let work = shard.work.clone();
    let inspection = shard.inspection.clone();
    let staging_dir = shard.staging_dir.clone();
    let range = shard.range.clone();
    let group = shard.group.clone();
    if group.is_failed() {
        if group.finish_shard(false) {
            let _ = std::fs::remove_dir_all(&staging_dir);
            work.prepared.lock().unwrap().take();
        }
        return TaskExecution::cancelled();
    }

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
    let report_failure = if result.is_err() {
        group.record_failure()
    } else {
        false
    };
    let last = group.finish_shard(succeeded);
    if last && group.is_failed() {
        let _ = std::fs::remove_dir_all(&staging_dir);
        work.prepared.lock().unwrap().take();
    } else if succeeded && !group.is_failed() {
        let extracted = work
            .extracted_bytes
            .load(std::sync::atomic::Ordering::Acquire);
        if extracted >= inspection.total_uncompressed_bytes {
            let _ = event_tx.send(WorkerEvent::ExtractedBytes {
                path: work.base_name.clone(),
                bytes: inspection.total_uncompressed_bytes,
                total_bytes: inspection.total_uncompressed_bytes,
            });
        }
    }

    match result {
        Ok(()) => TaskExecution::succeeded(),
        Err(error) if report_failure => TaskExecution::failed(error.to_string()),
        Err(error) => TaskExecution::silent_failure(error.to_string()),
    }
}

pub(super) fn execute_commit_archive(
    work: std::sync::Arc<ArchiveWork>,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
    let prepared = match work.prepared.lock().unwrap().clone() {
        Some(prepared) => prepared,
        None => {
            return TaskExecution::failed("archive commit started without prepared state");
        }
    };
    let result: Result<bool, Error> = (|| {
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
            execute_patch_transaction(
                &plan,
                Some(&report),
                Some(&mut on_commit),
                Some(&mut on_patch),
                Some(&mut on_delete),
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
            Ok(false)
        } else {
            commit_staged_extract(&prepared.staging_dir, &work.dest, Some(&mut on_commit))?;
            Ok(true)
        }
    })();

    match result {
        Ok(needs_manifest_follow_up) => {
            work.prepared.lock().unwrap().take();
            let mut expansion = GraphExpansion::new();
            if needs_manifest_follow_up {
                let apply = expansion.add_root(Task::ApplyExtractedVfsPatchManifest {
                    install_root: work.dest.clone(),
                });
                if let Err(error) = expansion.add_task(Task::CleanupArchive { work }, [apply]) {
                    return TaskExecution::failed(error.to_string());
                }
            } else {
                expansion.add_root(Task::CleanupArchive { work });
            }
            TaskExecution::expand(expansion)
        }
        Err(error) => {
            let _ = std::fs::remove_dir_all(&prepared.staging_dir);
            work.prepared.lock().unwrap().take();
            TaskExecution::failed(error.to_string())
        }
    }
}

pub(super) fn execute_cleanup_archive(
    work: std::sync::Arc<ArchiveWork>,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
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
            TaskExecution::succeeded()
        }
        Err(error) => TaskExecution::failed(error.to_string()),
    }
}
