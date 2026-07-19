use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use crate::download::extractor::{
    ArchiveDirectory, ArchiveDirectoryDiscovery, ArchiveInspection, MultiVolumeExtractor,
    MultiVolumeLayout,
};
use crate::error::Error;
use crate::runtime::{build_patch_execution_plan_with_cache, PatchApplyOptions};

use super::super::fs_ops::{
    commit_staged_extract, execute_patch_transaction, make_extract_staging_dir,
};
use super::super::graph::{GraphExpansion, TaskDependencyToken, TaskExecution};
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

    let layout = match MultiVolumeLayout::from_expected(
        parts
            .iter()
            .map(|part| (part.dest.clone(), part.expected_size))
            .collect(),
    ) {
        Ok(layout) => layout,
        Err(error) => return TaskExecution::failed(error.to_string()),
    };
    let tokens = (0..parts.len())
        .map(|_| TaskDependencyToken::new())
        .collect::<Vec<_>>();
    let work = match ArchiveWork::new(
        base_name,
        layout.clone(),
        tokens.iter().copied().map(Some).collect(),
        dest,
        cleanup,
        password,
        patch_options,
    ) {
        Ok(work) => work,
        Err(error) => return TaskExecution::failed(error.to_string()),
    };

    // Tail volumes are inserted first so the dispatcher can obtain the EOCD
    // and central directory while earlier package parts continue downloading.
    let tail_indices = layout.volume_indices_for_range(layout.tail_probe_range());
    let tail_set = tail_indices.iter().copied().collect::<BTreeSet<_>>();
    let order = tail_indices
        .iter()
        .rev()
        .copied()
        .chain((0..parts.len()).filter(|index| !tail_set.contains(index)))
        .collect::<Vec<_>>();
    let mut expansion = GraphExpansion::new();
    let mut nodes = vec![None; parts.len()];
    for index in order {
        let node = expansion.add_root_bound(
            Task::InstallArchivePart {
                part: parts[index].clone(),
                retry_count: 0,
            },
            tokens[index],
        );
        nodes[index] = Some(node);
    }
    let tail_dependencies = tail_indices
        .into_iter()
        .filter_map(|index| nodes[index])
        .collect::<Vec<_>>();
    match expansion.add_task(
        Task::DiscoverArchiveDirectory {
            work,
            required_range: None,
        },
        tail_dependencies,
    ) {
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
    let layout = match MultiVolumeLayout::from_files(volumes) {
        Ok(layout) => layout,
        Err(error) => return TaskExecution::failed(error.to_string()),
    };
    let work = match ArchiveWork::new(
        base_name,
        layout.clone(),
        vec![None; layout.volume_count()],
        dest,
        cleanup,
        password,
        patch_options,
    ) {
        Ok(work) => work,
        Err(error) => return TaskExecution::failed(error.to_string()),
    };
    TaskExecution::then(Task::DiscoverArchiveDirectory {
        work,
        required_range: None,
    })
}

pub(super) fn execute_discover_archive_directory(
    work: Arc<ArchiveWork>,
    required_range: Option<std::ops::Range<u64>>,
) -> TaskExecution {
    if let Some(range) = required_range.as_ref() {
        if !work.layout.range_is_available(range) {
            return TaskExecution::failed(format!(
                "archive dependency completed without making byte range {}..{} available",
                range.start, range.end
            ));
        }
    }
    let extractor = MultiVolumeExtractor::from_layout(work.layout.clone());
    match extractor.discover_archive_directory() {
        Ok(ArchiveDirectoryDiscovery::Ready(directory)) => {
            let mut required_indices = work
                .layout
                .volume_indices_for_range(directory.central_directory.clone());
            required_indices.extend(
                work.layout
                    .volume_indices_for_range(directory.end_records.clone()),
            );
            required_indices.sort_unstable();
            required_indices.dedup();
            let dependencies = work.tokens_for_indices(&required_indices);
            let mut expansion = GraphExpansion::new();
            match expansion
                .add_root_with_tokens(Task::InspectArchiveIndex { work, directory }, dependencies)
            {
                Ok(_) => TaskExecution::expand(expansion),
                Err(error) => TaskExecution::failed(error.to_string()),
            }
        }
        Ok(ArchiveDirectoryDiscovery::NeedsRange(range)) => {
            let dependencies = work.tokens_for_range(range.clone());
            if dependencies.is_empty() {
                return TaskExecution::failed(format!(
                    "archive directory needs unavailable range {}..{}",
                    range.start, range.end
                ));
            }
            let mut expansion = GraphExpansion::new();
            match expansion.add_root_with_tokens(
                Task::DiscoverArchiveDirectory {
                    work,
                    required_range: Some(range),
                },
                dependencies,
            ) {
                Ok(_) => TaskExecution::expand(expansion),
                Err(error) => TaskExecution::failed(error.to_string()),
            }
        }
        Err(error) => TaskExecution::failed(error.to_string()),
    }
}

pub(super) fn execute_inspect_archive_index(
    work: Arc<ArchiveWork>,
    directory: ArchiveDirectory,
) -> TaskExecution {
    let extractor = MultiVolumeExtractor::from_layout(work.layout.clone());
    match extractor.inspect_archive_index(&directory) {
        Ok(inspection) => {
            let inspection = Arc::new(inspection);
            let control_volumes = MultiVolumeExtractor::control_volume_indices(&inspection);
            let dependencies = work.tokens_for_indices(&control_volumes);
            let mut expansion = GraphExpansion::new();
            match expansion
                .add_root_with_tokens(Task::ReadArchiveControls { work, inspection }, dependencies)
            {
                Ok(_) => TaskExecution::expand(expansion),
                Err(error) => TaskExecution::failed(error.to_string()),
            }
        }
        Err(error) => TaskExecution::failed(error.to_string()),
    }
}

pub(super) fn execute_read_archive_controls(
    work: Arc<ArchiveWork>,
    inspection: Arc<ArchiveInspection>,
) -> TaskExecution {
    let extractor = MultiVolumeExtractor::from_layout(work.layout.clone());
    match extractor.read_control_payloads(&inspection, work.password.as_deref()) {
        Ok(inspection) => TaskExecution::then(Task::PlanArchiveExtraction {
            work,
            inspection: Arc::new(inspection),
        }),
        Err(error) => TaskExecution::failed(error.to_string()),
    }
}

pub(super) fn execute_plan_archive_extraction(
    work: Arc<ArchiveWork>,
    inspection: Arc<ArchiveInspection>,
    extract_shards: usize,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
    let result: Result<GraphExpansion, Error> = (|| {
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
        *work.prepared.lock().unwrap() = Some(PreparedArchive {
            staging_dir: staging_dir.clone(),
            patch_plan: None,
        });

        // Transaction preflight deliberately remains before any shard becomes
        // ready, preserving the destructive-update safety barrier.
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
        work.prepared
            .lock()
            .unwrap()
            .as_mut()
            .expect("archive staging state disappeared during preflight")
            .patch_plan = patch_plan;
        let plans = MultiVolumeExtractor::extraction_shards(&inspection, extract_shards);
        let _ = event_tx.send(WorkerEvent::ExtractedBytes {
            path: work.base_name.clone(),
            bytes: 0,
            total_bytes: inspection.total_uncompressed_bytes,
        });

        let mut expansion = GraphExpansion::new();
        let commit_tokens = work.all_tokens();
        if plans.is_empty() {
            expansion
                .add_root_with_tokens(Task::CommitArchive { work: work.clone() }, commit_tokens)?;
            return Ok(expansion);
        }
        let execution_state = super::super::types::ArchiveShardExecutionState::new();
        let mut shard_nodes = Vec::with_capacity(plans.len());
        for plan in plans {
            let dependencies = work.tokens_for_indices(&plan.volume_indices);
            let node = expansion.add_root_with_tokens(
                Task::ExtractArchiveShard {
                    shard: ArchiveShardTask {
                        work: work.clone(),
                        inspection: inspection.clone(),
                        staging_dir: staging_dir.clone(),
                        entries: plan.entries,
                        volume_indices: plan.volume_indices,
                        uncompressed_bytes: plan.uncompressed_bytes,
                        execution_state: execution_state.clone(),
                    },
                },
                dependencies,
            )?;
            shard_nodes.push(node);
        }
        expansion.add_task_with_tokens(
            Task::CommitArchive { work: work.clone() },
            shard_nodes,
            commit_tokens,
        )?;
        Ok(expansion)
    })();

    match result {
        Ok(expansion) => TaskExecution::expand(expansion),
        Err(error) => {
            work.cleanup_prepared();
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
    let entries = shard.entries.clone();
    let execution_state = shard.execution_state.clone();
    match execution_state.try_begin() {
        Ok(()) => {}
        Err(cleanup_staging) => {
            if cleanup_staging {
                work.cleanup_prepared();
            }
            return TaskExecution::cancelled();
        }
    }

    let extractor = MultiVolumeExtractor::from_layout(work.layout.clone());
    let result = extractor.extract_entries_with_progress(
        &staging_dir,
        work.password.as_deref(),
        &inspection,
        &entries,
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
    );

    let succeeded = result.is_ok();
    let (report_failure, cleanup_staging) = execution_state.finish(succeeded);
    if cleanup_staging {
        work.cleanup_prepared();
    } else if succeeded && !execution_state.is_failed() {
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
            let cleanup_dependencies = work.all_tokens();
            let result = if needs_manifest_follow_up {
                let apply = expansion.add_root(Task::ApplyExtractedVfsPatchManifest {
                    install_root: work.dest.clone(),
                });
                expansion.add_task_with_tokens(
                    Task::CleanupArchive { work },
                    [apply],
                    cleanup_dependencies,
                )
            } else {
                expansion.add_root_with_tokens(Task::CleanupArchive { work }, cleanup_dependencies)
            };
            match result {
                Ok(_) => TaskExecution::expand(expansion),
                Err(error) => TaskExecution::failed(error.to_string()),
            }
        }
        Err(error) => {
            work.cleanup_prepared();
            TaskExecution::failed(error.to_string())
        }
    }
}

pub(super) fn execute_cleanup_archive(
    work: std::sync::Arc<ArchiveWork>,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
    let result = if work.cleanup {
        MultiVolumeExtractor::from_layout(work.layout.clone()).cleanup()
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
