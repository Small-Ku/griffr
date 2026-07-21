use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use super::super::fs_ops::{
    classify_reuse_mode, copy_verified_file_async, create_hardlink_async, storage_volume_group_key,
    storage_volume_id, ReuseMode,
};
use super::super::graph::{GraphExpansion, TaskRun};
use super::super::types::{destination_or_repair_tasks, ReuseCandidateGroup, Task, WorkerEvent};
use crate::runtime::PathReuseMethod;

pub(super) fn run_prepare_download(
    task: Task,
    max_retries: u32,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    let Task::Download {
        url,
        dest,
        logical_path,
        expected_md5,
        expected_size,
        retry_count,
        transfer_class,
        archive_repair,
        resume: None,
    } = task
    else {
        unreachable!("download preparation requires an unprepared download task");
    };

    match super::super::download::prepare_download(&dest, &expected_md5, expected_size) {
        Ok(super::super::download::DownloadPreparation::Done(bytes)) => {
            let _ = event_tx.send(WorkerEvent::downloaded(logical_path.clone(), bytes));
            let _ = event_tx.send(WorkerEvent::verified(logical_path, true, None));
            TaskRun::succeeded()
        }
        Ok(super::super::download::DownloadPreparation::Resume(resume)) => {
            let transfer = Task::Download {
                url,
                dest,
                logical_path,
                expected_md5,
                expected_size,
                retry_count,
                transfer_class,
                archive_repair: archive_repair.clone(),
                resume: Some(resume),
            };
            let mut tasks = archive_repair
                .map(super::archive::start_archive_repair_index)
                .unwrap_or_default();
            if tasks.is_empty() {
                TaskRun::then(transfer)
            } else {
                tasks.push(transfer);
                TaskRun::expand(GraphExpansion::parallel(tasks))
            }
        }
        Err(error) => retry_or_fail_download(
            url,
            dest,
            logical_path,
            expected_md5,
            expected_size,
            retry_count,
            transfer_class,
            archive_repair,
            max_retries,
            error,
            event_tx,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_transfer_download(
    task: Task,
    max_retries: u32,
    download_progress_buffer_bytes: usize,
    user_agent: &str,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    let Task::Download {
        url,
        dest,
        logical_path,
        expected_md5,
        expected_size,
        retry_count,
        transfer_class,
        archive_repair,
        resume: Some(resume),
    } = task
    else {
        unreachable!("download transfer requires a prepared download task");
    };

    if let (Some(session), Some(expected_size)) = (archive_repair.as_ref(), expected_size) {
        let direct_bytes = resume.remaining_bytes(expected_size);
        if let Some(repair) = session.select_file(
            dest.clone(),
            logical_path.clone(),
            expected_md5.clone(),
            expected_size,
            direct_bytes,
            Some(url.clone()),
            retry_count,
            transfer_class,
        ) {
            return TaskRun::then(Task::FetchArchiveRepairFile { repair });
        }
    }

    let event_tx_clone = event_tx.clone();
    let logical_path_clone = logical_path.clone();
    let _ = event_tx.send(WorkerEvent::progress(
        crate::runtime::ProgressPhase::Download,
        logical_path.clone(),
        0,
        expected_size.unwrap_or(0),
        false,
    ));
    let result = super::super::download::do_prepared_download(
        user_agent,
        &url,
        &dest,
        &expected_md5,
        expected_size,
        resume,
        download_progress_buffer_bytes,
        Some(move |progress| match progress {
            super::super::download::DownloadProgress::Advanced(bytes) => {
                let _ = event_tx_clone.send(WorkerEvent::progress(
                    crate::runtime::ProgressPhase::Download,
                    logical_path_clone.clone(),
                    bytes,
                    expected_size.unwrap_or(bytes),
                    false,
                ));
            }
            super::super::download::DownloadProgress::Reset(bytes) => {
                let _ = event_tx_clone.send(WorkerEvent::progress(
                    crate::runtime::ProgressPhase::Download,
                    logical_path_clone.clone(),
                    bytes,
                    0,
                    true,
                ));
            }
        }),
    )
    .await;
    match result {
        Ok(bytes) => {
            let _ = event_tx.send(WorkerEvent::downloaded(logical_path.clone(), bytes));
            let _ = event_tx.send(WorkerEvent::verified(logical_path, true, None));
            TaskRun::succeeded()
        }
        Err(error) => retry_or_fail_download(
            url,
            dest,
            logical_path,
            expected_md5,
            expected_size,
            retry_count,
            transfer_class,
            None,
            max_retries,
            error,
            event_tx,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn retry_or_fail_download(
    url: String,
    dest: PathBuf,
    logical_path: String,
    expected_md5: String,
    expected_size: Option<u64>,
    retry_count: u32,
    transfer_class: super::super::types::TransferClass,
    archive_repair: Option<std::sync::Arc<super::super::types::ArchiveRepairSession>>,
    max_retries: u32,
    error: crate::error::Error,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    if retry_count < max_retries {
        let _ = event_tx.send(WorkerEvent::Retried {
            path: logical_path.clone(),
            reason: format!("download attempt {} failed: {}", retry_count + 1, error),
        });
        TaskRun::then(Task::Download {
            url,
            dest,
            logical_path,
            expected_md5,
            expected_size,
            retry_count: retry_count + 1,
            transfer_class,
            archive_repair,
            resume: None,
        })
    } else {
        TaskRun::failed(format!("download failed after retries: {error}"))
    }
}

pub(super) fn run_repair_file(task: Task) -> TaskRun {
    let Task::RepairFile {
        dest,
        logical_path,
        expected_md5,
        expected_size,
        source_candidates,
        download_url,
        allow_copy_fallback,
        verify_destination_fallback,
        retry_count,
        transfer_class,
        archive_repair,
    } = task
    else {
        unreachable!("repair runner requires a repair task");
    };

    // Normal verify has already proved the destination is bad, so metadata
    // discovery can overlap reuse and direct preparation. Explicit relink mode
    // starts the same discovery from the fallback Download only after its
    // delayed destination verification fails.
    let archive_prepare = if verify_destination_fallback {
        Vec::new()
    } else {
        archive_repair
            .clone()
            .map(super::archive::start_archive_repair_index)
            .unwrap_or_default()
    };

    let destination_volume = storage_volume_id(&dest);
    let mut seen = BTreeSet::new();
    let mut all_sources = Vec::new();
    let mut hardlink_groups = Vec::<Vec<PathBuf>>::new();
    let mut copy_groups = Vec::<Vec<PathBuf>>::new();
    let mut hardlink_indexes = HashMap::<String, usize>::new();
    let mut copy_indexes = HashMap::<String, usize>::new();

    for source in source_candidates {
        let normalized = std::fs::canonicalize(&source).unwrap_or(source);
        if !seen.insert(normalized.clone()) {
            continue;
        }
        let source_volume = storage_volume_id(&normalized);
        let copy_only =
            classify_reuse_mode(source_volume.as_deref(), destination_volume.as_deref())
                == ReuseMode::CopyOnly;
        if copy_only && !allow_copy_fallback {
            continue;
        }
        all_sources.push(normalized.clone());
        let key = storage_volume_group_key(&normalized);
        let (groups, indexes) = if copy_only {
            (&mut copy_groups, &mut copy_indexes)
        } else {
            (&mut hardlink_groups, &mut hardlink_indexes)
        };
        let index = *indexes.entry(key).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[index].push(normalized);
    }

    if hardlink_groups.is_empty() && copy_groups.is_empty() {
        let tasks = destination_or_repair_tasks(
            dest,
            logical_path,
            expected_md5,
            expected_size,
            download_url,
            verify_destination_fallback,
            retry_count,
            transfer_class,
            archive_repair,
        )
        .map(|mut tasks| {
            tasks.extend(archive_prepare);
            tasks
        });
        return task_list_run(tasks);
    }

    let (first_groups, first_copy_only, deferred_copy_groups) = if hardlink_groups.is_empty() {
        (copy_groups, true, Vec::new())
    } else {
        (hardlink_groups, false, copy_groups)
    };
    let group = ReuseCandidateGroup::new(
        first_groups.len(),
        deferred_copy_groups,
        all_sources,
        dest,
        logical_path.clone(),
        expected_md5.clone(),
        expected_size,
        download_url,
        allow_copy_fallback,
        verify_destination_fallback,
        retry_count,
        transfer_class,
        archive_repair,
    );
    let mut tasks = first_groups
        .into_iter()
        .map(|candidates| Task::VerifyReuseVolume {
            copy_only: first_copy_only,
            candidates,
            logical_path: logical_path.clone(),
            expected_md5: expected_md5.clone(),
            expected_size,
            group: group.clone(),
        })
        .collect::<Vec<_>>();
    tasks.extend(archive_prepare);
    TaskRun::expand(GraphExpansion::parallel(tasks))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_verify_reuse_volume(
    copy_only: bool,
    candidates: Vec<PathBuf>,
    _logical_path: String,
    expected_md5: String,
    expected_size: u64,
    group: std::sync::Arc<ReuseCandidateGroup>,
) -> TaskRun {
    if group.is_resolved() {
        return TaskRun::succeeded();
    }
    let source = candidates.into_iter().find(|source| {
        if group.is_resolved() {
            return false;
        }
        matches!(
            super::super::verify::verify_candidate_cancellable(
                source,
                &expected_md5,
                expected_size,
                || group.is_resolved(),
            ),
            super::super::verify::CandidateVerification::Valid
        )
    });
    task_list_run(group.finish_volume(copy_only, source))
}

pub(super) async fn run_hardlink_reuse_file(
    task: Task,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    let Task::ReuseFile {
        source,
        copy_only,
        dest,
        ..
    } = &task
    else {
        unreachable!("hardlink reuse runner requires a reuse task");
    };
    assert!(!copy_only, "copy reuse routed to hardlink runner");
    let result = create_hardlink_async(source, dest)
        .await
        .map(|()| PathReuseMethod::Hardlink);
    finish_reuse_file(task, result, event_tx)
}

pub(super) async fn run_copy_reuse_file(
    task: Task,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    let Task::ReuseFile {
        source,
        copy_only,
        dest,
        expected_md5,
        expected_size,
        ..
    } = &task
    else {
        unreachable!("copy reuse runner requires a reuse task");
    };
    assert!(*copy_only, "hardlink reuse routed to copy runner");
    let result = copy_verified_file_async(source, dest, expected_md5, *expected_size).await;
    finish_reuse_file(task, result, event_tx)
}

fn finish_reuse_file(
    task: Task,
    result: crate::error::Result<PathReuseMethod>,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    let Task::ReuseFile {
        source,
        copy_only,
        remaining_source_candidates,
        dest,
        logical_path,
        expected_md5,
        expected_size,
        download_url,
        allow_copy_fallback,
        verify_destination_fallback,
        retry_count,
        transfer_class,
        archive_repair,
    } = task
    else {
        unreachable!("reuse finish requires a reuse task");
    };
    match result {
        Ok(PathReuseMethod::Hardlink) => {
            let _ = event_tx.send(WorkerEvent::hardlinked(dest));
            let _ = event_tx.send(WorkerEvent::verified(logical_path, true, None));
            TaskRun::succeeded()
        }
        Ok(PathReuseMethod::Copy) => {
            let _ = event_tx.send(WorkerEvent::copied(dest));
            let _ = event_tx.send(WorkerEvent::verified(logical_path, true, None));
            TaskRun::succeeded()
        }
        Err(error) if !copy_only && allow_copy_fallback => {
            let _ = event_tx.send(WorkerEvent::Retried {
                path: logical_path.clone(),
                reason: format!(
                    "verified-source hardlink failed; scheduling copy fallback: {error}"
                ),
            });
            TaskRun::then(Task::ReuseFile {
                source,
                copy_only: true,
                remaining_source_candidates,
                dest,
                logical_path,
                expected_md5,
                expected_size,
                download_url,
                allow_copy_fallback,
                verify_destination_fallback,
                retry_count,
                transfer_class,
                archive_repair,
            })
        }
        Err(error) => {
            let _ = event_tx.send(WorkerEvent::Retried {
                path: logical_path.clone(),
                reason: format!("verified-source reuse failed: {error}"),
            });
            if !remaining_source_candidates.is_empty() {
                TaskRun::then(Task::RepairFile {
                    dest,
                    logical_path,
                    expected_md5,
                    expected_size,
                    source_candidates: remaining_source_candidates,
                    download_url,
                    allow_copy_fallback,
                    verify_destination_fallback,
                    retry_count,
                    transfer_class,
                    archive_repair,
                })
            } else {
                task_list_run(destination_or_repair_tasks(
                    dest,
                    logical_path,
                    expected_md5,
                    expected_size,
                    download_url,
                    verify_destination_fallback,
                    retry_count,
                    transfer_class,
                    archive_repair,
                ))
            }
        }
    }
}

fn task_list_run(tasks: crate::error::Result<Vec<Task>>) -> TaskRun {
    match tasks {
        Ok(tasks) => TaskRun::expand(GraphExpansion::parallel(tasks)),
        Err(error) => TaskRun::failed(error.to_string()),
    }
}
