use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::download::extractor::{ArchiveExtractionShardPlan, ArchiveIndex, MultiVolumeExtractor};
use crate::error::{Error, Result};
use crate::runtime::{
    build_patch_plan_with_probe_cache, is_install_change_path, plan_patch_probes, write_patch_plan,
    ArtifactExpectation, ArtifactSource, PatchPlan, PlannedPatchSource, DELETE_FILES_MANIFEST_NAME,
    PATCH_MANIFEST_NAME, PATCH_STAGE_DIR,
};

use crate::runtime::task_pool::fs_ops::{
    commit_file_job, commit_observed_artifact, create_extract_staging_dir, CommitFileJob,
};
use crate::runtime::task_pool::graph::{GraphExpansion, TaskRun};
use crate::runtime::task_pool::types::{
    ArchiveDirectCommitState, ArchiveRangePriority, ArchiveRangeReleaseState, ArchiveShardRunState,
    ArchiveShardTask, ArchiveWork, PatchApplyWork, PatchCheckWork, PreparedArchive, Task,
    WorkerEvent,
};

pub(crate) fn run_plan_archive_extraction(
    work: Arc<ArchiveWork>,
    archive_index: Arc<ArchiveIndex>,
    extract_shards: usize,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    let result: Result<GraphExpansion, Error> = (|| {
        let patch_options = work.patch_options.resolved_for_install(&work.dest)?;
        let staging_work_dir = archive_index
            .patch_manifest
            .is_some()
            .then_some(patch_options.work_dir.as_deref())
            .flatten();
        let staging_dir =
            create_extract_staging_dir(&work.dest, &work.base_name, staging_work_dir)?;
        *work.prepared.lock().unwrap() = Some(PreparedArchive {
            staging_dir: staging_dir.clone(),
            deferred_commit_paths: Vec::new(),
            patch_plan: None,
        });

        if archive_index.patch_manifest.is_some() {
            // Patch extraction must wait until source probing chooses exactly
            // one source for each final entry. SavePatchPlan then expands the
            // selected extraction DAG, so unused alternate payloads never
            // consume range-cache, extraction, or staging space.
            let probe_plan = plan_patch_probes(&work.dest, &archive_index, &patch_options)?;
            let patch_check = PatchCheckWork::new(probe_plan);
            let mut expansion = GraphExpansion::new();
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
            expansion.add_task(
                Task::SavePatchPlan {
                    work: work.clone(),
                    archive_index,
                    patch_check,
                },
                dependencies,
            )?;
            return Ok(expansion);
        }

        let (direct_entries, deferred_entries, deferred_commit_paths) =
            full_archive_entry_groups(&work, &archive_index)?;
        let extraction_total_bytes = selected_entry_bytes(
            &archive_index,
            direct_entries.iter().chain(&deferred_entries),
        );
        if let Some(prepared) = work.prepared.lock().unwrap().as_mut() {
            prepared.deferred_commit_paths = deferred_commit_paths;
        }
        let direct_commit = direct_commit_state(direct_entries.len(), event_tx);
        let mut planned_shards = MultiVolumeExtractor::extraction_shards_for_indices(
            &archive_index,
            direct_entries,
            extract_shards,
        )
        .into_iter()
        .map(|plan| (plan, true))
        .collect::<Vec<_>>();
        planned_shards.extend(
            MultiVolumeExtractor::extraction_shards_for_indices(
                &archive_index,
                deferred_entries,
                1,
            )
            .into_iter()
            .map(|plan| (plan, false)),
        );
        build_archive_extraction_expansion(
            work.clone(),
            archive_index,
            staging_dir,
            planned_shards,
            extraction_total_bytes,
            direct_commit,
            event_tx,
        )
    })();

    match result {
        Ok(expansion) => TaskRun::expand(expansion),
        Err(error) => {
            work.cleanup_prepared();
            TaskRun::failed(error.to_string())
        }
    }
}

fn full_archive_entry_groups(
    work: &ArchiveWork,
    archive_index: &ArchiveIndex,
) -> Result<(Vec<usize>, Vec<usize>, Vec<PathBuf>), Error> {
    let mut direct_entries = Vec::new();
    for index in 0..archive_index.entry_sizes.len() {
        let name = archive_index
            .archive
            .name_for_index(index)
            .ok_or_else(|| Error::Message {
                context: "Extraction error: ",
                detail: format!("ZIP parser has no name for entry {index}"),
            })?;
        if name.ends_with('/') || archive_index.control_indices.contains(&index) {
            continue;
        }
        let normalized = crate::download::extractor::normalized_archive_name(name)?;
        if is_install_change_path(Path::new(&normalized)) {
            return Err(Error::Message {
                context: "Extraction error: ",
                detail: format!(
                    "Archive entry {normalized} uses the private install change namespace"
                ),
            });
        }
        if !work
            .excluded_commit_paths
            .contains(&normalized.to_ascii_lowercase())
        {
            direct_entries.push(index);
        }
    }

    // Control files are intentionally extracted into the private staging
    // directory. They become actionable only after every ordinary payload
    // entry has committed successfully.
    let deferred_entries = archive_index.control_indices.clone();
    let deferred_commit_paths = deferred_entries
        .iter()
        .map(|index| {
            let name = archive_index
                .archive
                .name_for_index(*index)
                .ok_or_else(|| Error::Message {
                    context: "Extraction error: ",
                    detail: format!("ZIP parser has no name for control entry {index}"),
                })?;
            crate::download::extractor::safe_relative_archive_path(name)
        })
        .collect::<Result<Vec<_>, Error>>()?;
    Ok((direct_entries, deferred_entries, deferred_commit_paths))
}

fn selected_patch_payloads(plan: &PatchPlan) -> BTreeSet<String> {
    plan.entries
        .iter()
        .filter_map(|entry| match &entry.source {
            PlannedPatchSource::Local { payload } | PlannedPatchSource::Hdiff { payload, .. } => {
                Some(
                    payload
                        .to_string_lossy()
                        .replace('\\', "/")
                        .to_ascii_lowercase(),
                )
            }
            PlannedPatchSource::AlreadyPresent => None,
        })
        .collect()
}

fn patch_archive_entry_groups(
    work: &ArchiveWork,
    archive_index: &ArchiveIndex,
    plan: &PatchPlan,
) -> Result<(Vec<usize>, Vec<usize>), Error> {
    let selected_payloads = selected_patch_payloads(plan);
    let deferred = plan
        .deferred_paths
        .iter()
        .map(|path| {
            path.to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
        })
        .collect::<BTreeSet<_>>();
    let mut direct_entries = Vec::new();
    let mut staged_entries = Vec::new();

    for index in 0..archive_index.entry_sizes.len() {
        let name = archive_index
            .archive
            .name_for_index(index)
            .ok_or_else(|| Error::Message {
                context: "Extraction error: ",
                detail: format!("ZIP parser has no name for entry {index}"),
            })?;
        if name.ends_with('/') {
            continue;
        }
        let normalized = crate::download::extractor::normalized_archive_name(name)?;
        if is_install_change_path(Path::new(&normalized)) {
            return Err(Error::Message {
                context: "Extraction error: ",
                detail: format!(
                    "Archive entry {normalized} uses the private install change namespace"
                ),
            });
        }
        let lookup = normalized.to_ascii_lowercase();
        if lookup == PATCH_MANIFEST_NAME.to_ascii_lowercase()
            || lookup == DELETE_FILES_MANIFEST_NAME.to_ascii_lowercase()
        {
            continue;
        }
        if Path::new(&normalized).starts_with(PATCH_STAGE_DIR) {
            if selected_payloads.contains(&lookup) {
                staged_entries.push(index);
            }
            continue;
        }
        if deferred.contains(&lookup) {
            staged_entries.push(index);
            continue;
        }
        if !work.excluded_commit_paths.contains(&lookup) {
            direct_entries.push(index);
        }
    }
    Ok((direct_entries, staged_entries))
}

fn selected_entry_bytes<'a>(
    archive_index: &ArchiveIndex,
    entries: impl IntoIterator<Item = &'a usize>,
) -> u64 {
    entries.into_iter().fold(0u64, |total, index| {
        total.saturating_add(archive_index.entry_sizes[*index])
    })
}

fn direct_commit_state(
    total_files: usize,
    event_tx: &flume::Sender<WorkerEvent>,
) -> Option<Arc<ArchiveDirectCommitState>> {
    if total_files == 0 {
        return None;
    }
    let _ = event_tx.send(WorkerEvent::progress(
        crate::runtime::ProgressPhase::Commit,
        ".".to_string(),
        0,
        total_files as u64,
        false,
    ));
    Some(ArchiveDirectCommitState::new(total_files))
}

#[allow(clippy::too_many_arguments)]
fn build_archive_extraction_expansion(
    work: Arc<ArchiveWork>,
    archive_index: Arc<ArchiveIndex>,
    staging_dir: PathBuf,
    planned_shards: Vec<(ArchiveExtractionShardPlan, bool)>,
    extraction_total_bytes: u64,
    direct_commit: Option<Arc<ArchiveDirectCommitState>>,
    event_tx: &flume::Sender<WorkerEvent>,
) -> Result<GraphExpansion, Error> {
    let _ = event_tx.send(WorkerEvent::progress(
        crate::runtime::ProgressPhase::Extract,
        work.base_name.clone(),
        0,
        extraction_total_bytes,
        false,
    ));

    let commit_tokens = work.all_tokens();
    let plan_ranges = planned_shards
        .iter()
        .map(|(plan, _)| {
            MultiVolumeExtractor::source_ranges_for_indices(&archive_index, &plan.entries)
        })
        .collect::<Vec<_>>();
    let run_state = ArchiveShardRunState::new();
    let range_release = (work.layout.is_remote() && !work.retention.keeps_full_volumes())
        .then(|| ArchiveRangeReleaseState::new(work.layout.clone(), plan_ranges.clone()));
    let mut expansion = GraphExpansion::new();
    let mut shard_nodes = Vec::with_capacity(planned_shards.len());
    let mut volume_shards = vec![Vec::new(); work.layout.volume_count()];

    for (shard_index, ((plan, commits_directly), ranges)) in
        planned_shards.into_iter().zip(plan_ranges).enumerate()
    {
        let local_dependencies = if work.layout.is_remote() {
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
                    total_extract_bytes: extraction_total_bytes,
                    run_state: run_state.clone(),
                    direct_commit: if commits_directly {
                        direct_commit.clone()
                    } else {
                        None
                    },
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
                Task::RetainArchiveVolume {
                    work: work.clone(),
                    volume_index,
                },
                readers,
                work.tokens_for_indices(&[volume_index]),
            )?;
            volume_nodes.push(node);
        }
        volume_nodes
    } else {
        shard_nodes
    };
    expansion.add_task_with_tokens(
        Task::FinishArchive { work: work.clone() },
        commit_dependencies,
        commit_tokens,
    )?;
    Ok(expansion)
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
    extract_shards: usize,
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
            let plan = patch_plan.0.clone();
            let report = patch_plan.1.clone();
            if let Err(error) = write_patch_plan(&plan) {
                return TaskRun::failed(error.to_string());
            }
            {
                let mut prepared_state = work.prepared.lock().unwrap();
                let Some(prepared) = prepared_state.as_mut() else {
                    return TaskRun::failed(
                        "archive staging state disappeared while finishing the patch plan",
                    );
                };
                prepared.patch_plan = Some(patch_plan);
            }
            let _ = event_tx.send(WorkerEvent::archive_check(work.base_name.clone(), report));

            let (direct_entries, staged_entries) =
                match patch_archive_entry_groups(&work, &archive_index, &plan) {
                    Ok(groups) => groups,
                    Err(error) => return TaskRun::failed(error.to_string()),
                };
            let extraction_total_bytes =
                selected_entry_bytes(&archive_index, direct_entries.iter().chain(&staged_entries));
            let direct_commit = direct_commit_state(direct_entries.len(), event_tx);
            let mut planned_shards = MultiVolumeExtractor::extraction_shards_for_indices(
                &archive_index,
                direct_entries,
                extract_shards,
            )
            .into_iter()
            .map(|plan| (plan, true))
            .collect::<Vec<_>>();
            planned_shards.extend(
                MultiVolumeExtractor::extraction_shards_for_indices(
                    &archive_index,
                    staged_entries,
                    extract_shards,
                )
                .into_iter()
                .map(|plan| (plan, false)),
            );
            match build_archive_extraction_expansion(
                work,
                archive_index,
                staging_dir,
                planned_shards,
                extraction_total_bytes,
                direct_commit,
                event_tx,
            ) {
                Ok(expansion) => TaskRun::expand(expansion),
                Err(error) => TaskRun::failed(error.to_string()),
            }
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
    let extraction_total_bytes = shard.total_extract_bytes;
    let mut on_extract_progress = |bytes| {
        let extracted = work
            .extracted_bytes
            .fetch_add(bytes, std::sync::atomic::Ordering::AcqRel)
            .saturating_add(bytes);
        let _ = event_tx.send(WorkerEvent::progress(
            crate::runtime::ProgressPhase::Extract,
            work.base_name.clone(),
            extracted.min(extraction_total_bytes),
            extraction_total_bytes,
            false,
        ));
    };
    let result = if let Some(commit) = shard.direct_commit.as_ref() {
        extractor.extract_entries_with_progress_and_file(
            &staging_dir,
            work.password.as_deref(),
            &archive_index,
            &entries,
            &work.expected_files,
            extraction_progress_buffer_bytes,
            &mut on_extract_progress,
            |source, normalized, digest| {
                let lookup = normalized.to_ascii_lowercase();
                if let Some(expected) = work.expected_files.get(&lookup) {
                    let expectation = ArtifactExpectation::from_game_file(expected);
                    let destination = work.dest.join(&expected.path);
                    let digest = digest.as_ref().ok_or_else(|| Error::Message {
                        context: "Extraction error: ",
                        detail: format!("Archive entry {normalized} has no verified digest"),
                    })?;
                    let proof = commit_observed_artifact(
                        source,
                        &destination,
                        &expectation,
                        ArtifactSource::Archive,
                        digest,
                    )?;
                    let _ = event_tx.send(WorkerEvent::committed(proof));
                } else {
                    let logical_path = std::path::PathBuf::from(normalized);
                    commit_file_job(&CommitFileJob {
                        source: source.to_path_buf(),
                        destination: work.dest.join(&logical_path),
                        logical_path,
                    })?;
                }

                let finished = commit.finish_file();
                let _ = event_tx.send(WorkerEvent::changed(normalized.to_string()));
                let _ = event_tx.send(WorkerEvent::progress(
                    crate::runtime::ProgressPhase::Commit,
                    normalized.to_string(),
                    finished as u64,
                    commit.total_files() as u64,
                    false,
                ));
                Ok(())
            },
        )
    } else {
        extractor.extract_entries_with_progress(
            &staging_dir,
            work.password.as_deref(),
            &archive_index,
            &entries,
            &work.expected_files,
            extraction_progress_buffer_bytes,
            &mut on_extract_progress,
        )
    };

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
        if extracted >= extraction_total_bytes {
            let _ = event_tx.send(WorkerEvent::progress(
                crate::runtime::ProgressPhase::Extract,
                work.base_name.clone(),
                extraction_total_bytes,
                extraction_total_bytes,
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

pub(crate) fn run_finish_archive(work: std::sync::Arc<ArchiveWork>) -> TaskRun {
    let prepared = match work.prepared.lock().unwrap().clone() {
        Some(prepared) => prepared,
        None => {
            return TaskRun::failed("archive finish started without prepared state");
        }
    };
    if let Some((plan, _report)) = prepared.patch_plan {
        return super::patch::schedule_patch_apply(work, PatchApplyWork::new(plan));
    }
    super::commit::finish_archive(work, prepared.staging_dir, prepared.deferred_commit_paths)
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
