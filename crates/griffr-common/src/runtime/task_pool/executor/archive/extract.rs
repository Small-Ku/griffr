use std::sync::Arc;

use crate::download::extractor::{ArchiveInspection, MultiVolumeExtractor};
use crate::error::{Error, Result};
use crate::runtime::build_patch_execution_plan_with_cache;

use crate::runtime::task_pool::fs_ops::{
    commit_staged_extract, execute_patch_transaction, make_extract_staging_dir,
};
use crate::runtime::task_pool::graph::{GraphExpansion, TaskExecution};
use crate::runtime::task_pool::types::{
    ArchiveRangeReleaseState, ArchiveShardExecutionState, ArchiveShardTask, ArchiveWork,
    PreparedArchive, Task, WorkerEvent,
};
use crate::runtime::task_pool::verify::VerifiedArtifactCache;

pub(crate) fn execute_plan_archive_extraction(
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
            if work.should_complete_volumes() {
                let fill_gaps =
                    expansion.add_root(Task::FillArchiveVolumeGaps { work: work.clone() });
                expansion.add_task_with_tokens(
                    Task::CommitArchive { work: work.clone() },
                    [fill_gaps],
                    commit_tokens,
                )?;
            } else {
                expansion.add_root_with_tokens(
                    Task::CommitArchive { work: work.clone() },
                    commit_tokens,
                )?;
            }
            return Ok(expansion);
        }

        let plan_ranges = plans
            .iter()
            .map(|plan| MultiVolumeExtractor::source_ranges_for_indices(&inspection, &plan.entries))
            .collect::<Vec<_>>();
        let execution_state = ArchiveShardExecutionState::new();
        let range_release = (work.layout.is_remote() && !work.retention.keeps_complete_volumes())
            .then(|| ArchiveRangeReleaseState::new(work.layout.clone(), plan_ranges.clone()));
        let mut shard_nodes = Vec::with_capacity(plans.len());
        for (shard_index, (plan, ranges)) in plans.into_iter().zip(plan_ranges).enumerate() {
            let local_dependencies = if work.layout.is_remote() {
                work.layout
                    .missing_range_requests(ranges.clone())?
                    .into_iter()
                    .map(|request| {
                        expansion.add_root(Task::FetchArchiveRange {
                            work: work.clone(),
                            request,
                            retry_count: 0,
                        })
                    })
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            let external_dependencies = work.tokens_for_indices(&plan.volume_indices);
            let node = expansion.add_task_with_tokens(
                Task::ExtractArchiveShard {
                    shard: ArchiveShardTask {
                        work: work.clone(),
                        inspection: inspection.clone(),
                        staging_dir: staging_dir.clone(),
                        entries: plan.entries,
                        volume_indices: plan.volume_indices,
                        uncompressed_bytes: plan.uncompressed_bytes,
                        execution_state: execution_state.clone(),
                        range_release: range_release
                            .as_ref()
                            .map(|state| (state.clone(), shard_index)),
                    },
                },
                local_dependencies,
                external_dependencies,
            )?;
            shard_nodes.push(node);
        }
        let commit_dependencies = if work.should_complete_volumes() {
            vec![expansion.add_task(
                Task::FillArchiveVolumeGaps { work: work.clone() },
                shard_nodes,
            )?]
        } else {
            shard_nodes
        };
        expansion.add_task_with_tokens(
            Task::CommitArchive { work: work.clone() },
            commit_dependencies,
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

pub(crate) fn execute_extract_archive_shard(
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
        &work.expected_files,
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
    if !succeeded {
        work.invalidate_range_cache();
    } else if let Some((release, index)) = shard.range_release.as_ref() {
        release.complete_shard(*index);
    }
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

pub(crate) fn execute_commit_archive(
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

pub(crate) fn execute_cleanup_archive(
    work: std::sync::Arc<ArchiveWork>,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
    let result = if work.retention.keeps_complete_volumes() {
        work.layout.cleanup_cache();
        Ok(())
    } else {
        MultiVolumeExtractor::from_layout(work.layout.clone()).cleanup()
    };
    match result {
        Ok(()) => {
            let _ = event_tx.send(WorkerEvent::Extracted {
                path: work.dest.clone(),
            });
            if work.layout.is_remote() && !work.retention.keeps_complete_volumes() {
                let _ = event_tx.send(WorkerEvent::Verified {
                    path: work.base_name.clone(),
                    ok: true,
                    issue: None,
                });
            }
            TaskExecution::succeeded()
        }
        Err(error) => TaskExecution::failed(error.to_string()),
    }
}
