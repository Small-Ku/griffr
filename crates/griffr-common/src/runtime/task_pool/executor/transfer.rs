use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use super::super::fs_ops::{
    classify_reuse_mode, create_hardlink_async, reuse_verified_file, storage_volume_group_key,
    storage_volume_id, ReuseMethod, ReuseMode,
};
use super::super::graph::{GraphExpansion, TaskExecution};
use super::super::types::{
    destination_or_download_tasks, ReuseCandidateGroup, Task, TransferClass, WorkerEvent,
};

pub(super) struct DownloadExecInput {
    pub(super) url: String,
    pub(super) dest: PathBuf,
    pub(super) logical_path: String,
    pub(super) expected_md5: String,
    pub(super) expected_size: Option<u64>,
    pub(super) retry_count: u32,
    pub(super) max_retries: u32,
    pub(super) transfer_class: TransferClass,
}

pub(super) struct RepairFileInput {
    pub(super) dest: PathBuf,
    pub(super) logical_path: String,
    pub(super) expected_md5: String,
    pub(super) expected_size: u64,
    pub(super) source_candidates: Vec<PathBuf>,
    pub(super) download_url: Option<String>,
    pub(super) allow_copy_fallback: bool,
    pub(super) verify_destination_fallback: bool,
    pub(super) retry_count: u32,
    pub(super) transfer_class: TransferClass,
}

pub(super) struct ReuseFileInput {
    pub(super) source: PathBuf,
    pub(super) copy_only: bool,
    pub(super) remaining_source_candidates: Vec<PathBuf>,
    pub(super) dest: PathBuf,
    pub(super) logical_path: String,
    pub(super) expected_md5: String,
    pub(super) expected_size: u64,
    pub(super) download_url: Option<String>,
    pub(super) allow_copy_fallback: bool,
    pub(super) verify_destination_fallback: bool,
    pub(super) retry_count: u32,
    pub(super) transfer_class: TransferClass,
}

pub(super) fn execute_prepare_download(
    input: DownloadExecInput,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
    match super::super::download::prepare_download(
        &input.dest,
        &input.expected_md5,
        input.expected_size,
    ) {
        Ok(super::super::download::DownloadPreparation::Complete(bytes)) => {
            let _ = event_tx.send(WorkerEvent::Downloaded {
                path: input.logical_path.clone(),
                bytes,
            });
            let _ = event_tx.send(WorkerEvent::Verified {
                path: input.logical_path,
                ok: true,
                issue: None,
            });
            TaskExecution::succeeded()
        }
        Ok(super::super::download::DownloadPreparation::Ready(resume)) => {
            TaskExecution::then(Task::TransferDownload {
                url: input.url,
                dest: input.dest,
                logical_path: input.logical_path,
                expected_md5: input.expected_md5,
                expected_size: input.expected_size,
                retry_count: input.retry_count,
                transfer_class: input.transfer_class,
                resume,
            })
        }
        Err(error) => retry_or_fail_download(input, error, event_tx),
    }
}

pub(super) async fn execute_transfer_download(
    input: DownloadExecInput,
    resume: super::super::types::DownloadResumeState,
    download_progress_buffer_bytes: usize,
    user_agent: &str,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
    let event_tx_clone = event_tx.clone();
    let logical_path_clone = input.logical_path.clone();
    let expected_size_val = input.expected_size;
    let _ = event_tx.send(WorkerEvent::DownloadStarted {
        path: input.logical_path.clone(),
        total_bytes: expected_size_val.unwrap_or(0),
    });
    let result = super::super::download::do_prepared_download(
        user_agent,
        &input.url,
        &input.dest,
        &input.expected_md5,
        input.expected_size,
        resume,
        download_progress_buffer_bytes,
        Some(move |progress| match progress {
            super::super::download::DownloadProgress::Advanced(bytes) => {
                let _ = event_tx_clone.send(WorkerEvent::DownloadedBytes {
                    path: logical_path_clone.clone(),
                    bytes,
                    total_bytes: expected_size_val.unwrap_or(bytes),
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
    .await;
    match result {
        Ok(bytes) => {
            let _ = event_tx.send(WorkerEvent::Downloaded {
                path: input.logical_path.clone(),
                bytes,
            });
            let _ = event_tx.send(WorkerEvent::Verified {
                path: input.logical_path,
                ok: true,
                issue: None,
            });
            TaskExecution::succeeded()
        }
        Err(error) => retry_or_fail_download(input, error, event_tx),
    }
}

fn retry_or_fail_download(
    input: DownloadExecInput,
    error: crate::error::Error,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
    if input.retry_count < input.max_retries {
        let _ = event_tx.send(WorkerEvent::Retried {
            path: input.logical_path.clone(),
            reason: format!(
                "download attempt {} failed: {}",
                input.retry_count + 1,
                error
            ),
        });
        TaskExecution::then(Task::Download {
            url: input.url,
            dest: input.dest,
            logical_path: input.logical_path,
            expected_md5: input.expected_md5,
            expected_size: input.expected_size,
            retry_count: input.retry_count + 1,
            transfer_class: input.transfer_class,
        })
    } else {
        TaskExecution::failed(format!("download failed after retries: {error}"))
    }
}

pub(super) fn execute_repair_file(input: RepairFileInput) -> TaskExecution {
    let RepairFileInput {
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
    } = input;

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
        return task_list_execution(destination_or_download_tasks(
            dest,
            logical_path,
            expected_md5,
            expected_size,
            download_url,
            verify_destination_fallback,
            retry_count,
            transfer_class,
        ));
    }

    let (initial_groups, initial_copy_only, deferred_copy_groups) = if hardlink_groups.is_empty() {
        (copy_groups, true, Vec::new())
    } else {
        (hardlink_groups, false, copy_groups)
    };
    let group = ReuseCandidateGroup::new(
        initial_groups.len(),
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
    );
    TaskExecution::expand(GraphExpansion::parallel(initial_groups.into_iter().map(
        |candidates| Task::VerifyReuseVolume {
            copy_only: initial_copy_only,
            candidates,
            logical_path: logical_path.clone(),
            expected_md5: expected_md5.clone(),
            expected_size,
            group: group.clone(),
        },
    )))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn execute_verify_reuse_volume(
    copy_only: bool,
    candidates: Vec<PathBuf>,
    _logical_path: String,
    expected_md5: String,
    expected_size: u64,
    group: std::sync::Arc<ReuseCandidateGroup>,
) -> TaskExecution {
    if group.is_resolved() {
        return TaskExecution::succeeded();
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
    task_list_execution(group.finish_volume(copy_only, source))
}

pub(super) async fn execute_hardlink_reuse_file(
    input: ReuseFileInput,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
    assert!(!input.copy_only, "copy reuse routed to async executor");
    let result = create_hardlink_async(&input.source, &input.dest)
        .await
        .map(|()| ReuseMethod::Hardlink);
    finish_reuse_file(input, result, event_tx)
}

pub(super) fn execute_copy_reuse_file(
    input: ReuseFileInput,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
    assert!(
        input.copy_only,
        "hardlink reuse routed to blocking executor"
    );
    let result = reuse_verified_file(
        &input.source,
        &input.dest,
        &input.expected_md5,
        input.expected_size,
        ReuseMode::CopyOnly,
        true,
    );
    finish_reuse_file(input, result, event_tx)
}

fn finish_reuse_file(
    input: ReuseFileInput,
    result: crate::error::Result<ReuseMethod>,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
    let ReuseFileInput {
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
    } = input;
    match result {
        Ok(ReuseMethod::Hardlink) => {
            let _ = event_tx.send(WorkerEvent::Hardlinked { path: dest });
            let _ = event_tx.send(WorkerEvent::Verified {
                path: logical_path,
                ok: true,
                issue: None,
            });
            TaskExecution::succeeded()
        }
        Ok(ReuseMethod::Copy) => {
            let _ = event_tx.send(WorkerEvent::Copied { path: dest });
            let _ = event_tx.send(WorkerEvent::Verified {
                path: logical_path,
                ok: true,
                issue: None,
            });
            TaskExecution::succeeded()
        }
        Err(error) if !copy_only && allow_copy_fallback => {
            let _ = event_tx.send(WorkerEvent::Retried {
                path: logical_path.clone(),
                reason: format!(
                    "verified-source hardlink failed; scheduling copy fallback: {error}"
                ),
            });
            TaskExecution::then(Task::ReuseFile {
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
            })
        }
        Err(error) => {
            let _ = event_tx.send(WorkerEvent::Retried {
                path: logical_path.clone(),
                reason: format!("verified-source reuse failed: {error}"),
            });
            if !remaining_source_candidates.is_empty() {
                TaskExecution::then(Task::RepairFile {
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
                })
            } else {
                task_list_execution(destination_or_download_tasks(
                    dest,
                    logical_path,
                    expected_md5,
                    expected_size,
                    download_url,
                    verify_destination_fallback,
                    retry_count,
                    transfer_class,
                ))
            }
        }
    }
}

fn task_list_execution(tasks: crate::error::Result<Vec<Task>>) -> TaskExecution {
    match tasks {
        Ok(tasks) => TaskExecution::expand(GraphExpansion::parallel(tasks)),
        Err(error) => TaskExecution::failed(error.to_string()),
    }
}
