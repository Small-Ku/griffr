use std::sync::Arc;

use crate::download::extractor::{ArchiveIndex, MultiVolumeExtractor};
use crate::error::{Error, Result};
use crate::runtime::{build_patch_plan_with_probe_cache, plan_patch_probes};

use crate::runtime::task_pool::fs_ops::make_extract_staging_dir;
use crate::runtime::task_pool::graph::{GraphExpansion, TaskRun};
use crate::runtime::task_pool::types::{
    ArchiveRangePriority, ArchiveRangeReleaseState, ArchiveShardRunState, ArchiveShardTask,
    ArchiveWork, PatchApplyWork, PatchCheckWork, PreparedArchive, Task, WorkerEvent,
};

pub(crate) fn run_plan_archive_extraction(
    work: Arc<ArchiveWork>,
    archive_index: Arc<ArchiveIndex>,
    extract_shards: usize,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    let result: Result<GraphExpansion, Error> = (|| {
        let patch_options = work.patch_options.resolved_for_install(&work.dest)?;
        let staging_dir = make_extract_staging_dir(
            &work.dest,
            &work.base_name,
            patch_options.work_dir.as_deref(),
        )?;
        std::fs::create_dir_all(&staging_dir).map_err(|source| Error::IoAt {
            action: "create directory",
            path: staging_dir.clone(),
            source,
        })?;
        *work.prepared.lock().unwrap() = Some(PreparedArchive {
            staging_dir: staging_dir.clone(),
            patch_plan: None,
        });

        let plans = MultiVolumeExtractor::extraction_shards(&archive_index, extract_shards);
        let _ = event_tx.send(WorkerEvent::progress(
            crate::runtime::ProgressPhase::Extract,
            work.base_name.clone(),
            0,
            archive_index.total_uncompressed_bytes,
            false,
        ));

        let mut expansion = GraphExpansion::new();
        let patch_check_node = if archive_index.patch_manifest.is_some() {
            let probe_plan = plan_patch_probes(&work.dest, &archive_index, &patch_options)?;
            let patch_check = PatchCheckWork::new(probe_plan);
            let mut dependencies = (0..patch_check.probe_count())
                .map(|probe_index| {
                    expansion.add_root(Task::ProbePatchArtifact {
                        patch_check: patch_check.clone(),
                        probe_index,
                    })
                })
                .collect::<Vec<_>>();
            if patch_check.relocation_root().is_some() {
                dependencies.push(expansion.add_root(Task::MeasurePatchRelocation {
                    patch_check: patch_check.clone(),
                }));
            }
            Some(expansion.add_task(
                Task::SavePatchPlan {
                    work: work.clone(),
                    archive_index: archive_index.clone(),
                    patch_check,
                },
                dependencies,
            )?)
        } else {
            None
        };
        let commit_tokens = work.all_tokens();
        if plans.is_empty() {
            if work.should_save_full_volumes() {
                let mut volume_nodes = Vec::with_capacity(work.layout.volume_count());
                for volume_index in 0..work.layout.volume_count() {
                    let node = expansion.add_task_with_tokens(
                        Task::FillArchiveVolumeGaps {
                            work: work.clone(),
                            volume_index,
                        },
                        std::iter::empty(),
                        work.tokens_for_indices(&[volume_index]),
                    )?;
                    volume_nodes.push(node);
                }
                let volumes_ready = expansion.add_task(
                    Task::ArchiveVolumesReady { work: work.clone() },
                    volume_nodes,
                )?;
                let mut commit_dependencies = vec![volumes_ready];
                commit_dependencies.extend(patch_check_node);
                expansion.add_task_with_tokens(
                    Task::CommitArchive { work: work.clone() },
                    commit_dependencies,
                    commit_tokens,
                )?;
            } else {
                if let Some(patch_check_node) = patch_check_node {
                    expansion.add_task_with_tokens(
                        Task::CommitArchive { work: work.clone() },
                        [patch_check_node],
                        commit_tokens,
                    )?;
                } else {
                    expansion.add_root_with_tokens(
                        Task::CommitArchive { work: work.clone() },
                        commit_tokens,
                    )?;
                }
            }
            return Ok(expansion);
        }

        let plan_ranges = plans
            .iter()
            .map(|plan| {
                MultiVolumeExtractor::source_ranges_for_indices(&archive_index, &plan.entries)
            })
            .collect::<Vec<_>>();
        let run_state = ArchiveShardRunState::new();
        let range_release = (work.layout.is_remote() && !work.retention.keeps_full_volumes())
            .then(|| ArchiveRangeReleaseState::new(work.layout.clone(), plan_ranges.clone()));
        let mut shard_nodes = Vec::with_capacity(plans.len());
        let mut volume_shards = vec![Vec::new(); work.layout.volume_count()];
        for (shard_index, (plan, ranges)) in plans.into_iter().zip(plan_ranges).enumerate() {
            let mut local_dependencies = if work.layout.is_remote() {
                work.layout
                    .missing_range_requests(ranges.clone())?
                    .into_iter()
                    .map(|request| {
                        expansion.add_root(Task::FetchArchiveRange {
                            work: work.clone(),
                            request,
                            retry_count: 0,
                            priority: ArchiveRangePriority::ExtractionCritical,
                        })
                    })
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            local_dependencies.extend(patch_check_node);
            let external_dependencies = work.tokens_for_indices(&plan.volume_indices);
            let shard_volume_indices = plan.volume_indices.clone();
            let node = expansion.add_task_with_tokens(
                Task::ExtractArchiveShard {
                    shard: ArchiveShardTask {
                        work: work.clone(),
                        archive_index: archive_index.clone(),
                        staging_dir: staging_dir.clone(),
                        entries: plan.entries,
                        volume_indices: plan.volume_indices,
                        estimated_cost: plan.estimated_cost,
                        run_state: run_state.clone(),
                        range_release: range_release
                            .as_ref()
                            .map(|state| (state.clone(), shard_index)),
                    },
                },
                local_dependencies,
                external_dependencies,
            )?;
            for volume_index in shard_volume_indices {
                if let Some(readers) = volume_shards.get_mut(volume_index) {
                    readers.push(node);
                }
            }
            shard_nodes.push(node);
        }
        let commit_dependencies = if work.should_save_full_volumes() {
            let mut volume_nodes = Vec::with_capacity(work.layout.volume_count());
            for (volume_index, readers) in volume_shards.into_iter().enumerate() {
                let node = expansion.add_task_with_tokens(
                    Task::FillArchiveVolumeGaps {
                        work: work.clone(),
                        volume_index,
                    },
                    readers,
                    work.tokens_for_indices(&[volume_index]),
                )?;
                volume_nodes.push(node);
            }
            vec![expansion.add_task(
                Task::ArchiveVolumesReady { work: work.clone() },
                volume_nodes,
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
        Ok(expansion) => TaskRun::expand(expansion),
        Err(error) => {
            work.cleanup_prepared();
            TaskRun::failed(error.to_string())
        }
    }
}

pub(crate) fn run_probe_patch_artifact(
    patch_check: Arc<PatchCheckWork>,
    probe_index: usize,
) -> TaskRun {
    match patch_check.run_probe(probe_index) {
        Ok(()) => TaskRun::succeeded(),
        Err(error) => TaskRun::failed(error.to_string()),
    }
}

pub(crate) fn run_measure_patch_relocation(patch_check: Arc<PatchCheckWork>) -> TaskRun {
    match patch_check.measure_relocation() {
        Ok(()) => TaskRun::succeeded(),
        Err(error) => TaskRun::failed(error.to_string()),
    }
}

pub(crate) fn run_save_patch_plan(
    work: Arc<ArchiveWork>,
    archive_index: Arc<ArchiveIndex>,
    patch_check: Arc<PatchCheckWork>,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    let staging_dir = match work.prepared.lock().unwrap().as_ref() {
        Some(prepared) => prepared.staging_dir.clone(),
        None => {
            return TaskRun::failed("patch plan finish started without archive staging state");
        }
    };
    let measured_relocation_bytes = match patch_check.measured_relocation_bytes() {
        Ok(bytes) => bytes,
        Err(error) => return TaskRun::failed(error.to_string()),
    };
    let result = build_patch_plan_with_probe_cache(
        &work.dest,
        &staging_dir,
        &archive_index,
        &work.patch_options,
        patch_check.verification_cache(),
        measured_relocation_bytes,
    );
    match result {
        Ok(patch_plan) => {
            let report = patch_plan.1.clone();
            let mut prepared_state = work.prepared.lock().unwrap();
            let Some(prepared) = prepared_state.as_mut() else {
                return TaskRun::failed(
                    "archive staging state disappeared while finishing the patch plan",
                );
            };
            prepared.patch_plan = Some(patch_plan);
            let _ = event_tx.send(WorkerEvent::archive_check(work.base_name.clone(), report));
            TaskRun::succeeded()
        }
        Err(error) => TaskRun::failed(error.to_string()),
    }
}

pub(crate) fn run_extract_archive_shard(
    shard: ArchiveShardTask,
    extraction_progress_buffer_bytes: usize,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    let work = shard.work.clone();
    let archive_index = shard.archive_index.clone();
    let staging_dir = shard.staging_dir.clone();
    let entries = shard.entries.clone();
    let run_state = shard.run_state.clone();
    match run_state.try_begin() {
        Ok(()) => {}
        Err(cleanup_staging) => {
            if cleanup_staging {
                work.cleanup_prepared();
            }
            return TaskRun::cancelled();
        }
    }

    let extractor = MultiVolumeExtractor::from_layout(work.layout.clone());
    let result = extractor.extract_entries_with_progress(
        &staging_dir,
        work.password.as_deref(),
        &archive_index,
        &entries,
        &work.expected_files,
        extraction_progress_buffer_bytes,
        |bytes| {
            let extracted = work
                .extracted_bytes
                .fetch_add(bytes, std::sync::atomic::Ordering::AcqRel)
                .saturating_add(bytes);
            let _ = event_tx.send(WorkerEvent::progress(
                crate::runtime::ProgressPhase::Extract,
                work.base_name.clone(),
                extracted.min(archive_index.total_uncompressed_bytes),
                archive_index.total_uncompressed_bytes,
                false,
            ));
        },
    );

    let succeeded = result.is_ok();
    if !succeeded {
        work.invalidate_range_cache();
    } else if let Some((release, index)) = shard.range_release.as_ref() {
        release.finish_shard(*index);
    }
    let (report_failure, cleanup_staging) = run_state.finish(succeeded);
    if cleanup_staging {
        work.cleanup_prepared();
    } else if succeeded && !run_state.is_failed() {
        let extracted = work
            .extracted_bytes
            .load(std::sync::atomic::Ordering::Acquire);
        if extracted >= archive_index.total_uncompressed_bytes {
            let _ = event_tx.send(WorkerEvent::progress(
                crate::runtime::ProgressPhase::Extract,
                work.base_name.clone(),
                archive_index.total_uncompressed_bytes,
                archive_index.total_uncompressed_bytes,
                false,
            ));
        }
    }

    match result {
        Ok(()) => TaskRun::succeeded(),
        Err(error) if report_failure => TaskRun::failed(error.to_string()),
        Err(error) => TaskRun::silent_failure(error.to_string()),
    }
}

pub(crate) fn run_commit_archive(
    work: std::sync::Arc<ArchiveWork>,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    let prepared = match work.prepared.lock().unwrap().clone() {
        Some(prepared) => prepared,
        None => {
            return TaskRun::failed("archive commit started without prepared state");
        }
    };
    if let Some((plan, _report)) = prepared.patch_plan {
        return super::patch::schedule_patch_apply(work, PatchApplyWork::new(plan));
    }
    super::commit::schedule_archive_commit(work, prepared.staging_dir, event_tx)
}

pub(crate) fn run_clean_archive(
    work: std::sync::Arc<ArchiveWork>,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    let result = if work.retention.keeps_full_volumes() {
        work.layout.cleanup_cache();
        Ok(())
    } else {
        MultiVolumeExtractor::from_layout(work.layout.clone()).cleanup()
    };
    match result {
        Ok(()) => {
            if work.layout.is_remote() && !work.retention.keeps_full_volumes() {
                let _ = event_tx.send(WorkerEvent::verified(work.base_name.clone(), true, None));
            }
            TaskRun::succeeded()
        }
        Err(error) => TaskRun::failed(error.to_string()),
    }
}
