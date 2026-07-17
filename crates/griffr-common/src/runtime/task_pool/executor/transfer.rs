use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use compio::dispatcher::Dispatcher;

use super::super::fs_ops::{
    classify_reuse_mode, reuse_verified_file, storage_volume_group_key, storage_volume_id,
    ReuseMethod, ReuseMode,
};
use super::super::types::{ReuseCandidateGroup, Task, TransferClass, WorkerEvent};

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
    pub(super) retry_count: u32,
    pub(super) transfer_class: TransferClass,
}

pub(super) fn execute_prepare_download(
    input: DownloadExecInput,
    io_dispatcher: Option<&Dispatcher>,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<WorkerEvent>,
) {
    match super::super::download::prepare_download(
        io_dispatcher,
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
        }
        Ok(super::super::download::DownloadPreparation::Ready(resume)) => {
            spawned.push(Task::TransferDownload {
                url: input.url,
                dest: input.dest,
                logical_path: input.logical_path,
                expected_md5: input.expected_md5,
                expected_size: input.expected_size,
                retry_count: input.retry_count,
                transfer_class: input.transfer_class,
                resume,
            });
        }
        Err(error) => retry_or_fail_download(input, error, spawned, event_tx),
    }
}

pub(super) fn execute_transfer_download(
    input: DownloadExecInput,
    resume: super::super::types::DownloadResumeState,
    download_progress_buffer_bytes: usize,
    io_dispatcher: Option<&Dispatcher>,
    user_agent: &str,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<WorkerEvent>,
) {
    let event_tx_clone = event_tx.clone();
    let logical_path_clone = input.logical_path.clone();
    let expected_size_val = input.expected_size;
    let _ = event_tx.send(WorkerEvent::DownloadStarted {
        path: input.logical_path.clone(),
        total_bytes: expected_size_val.unwrap_or(0),
    });
    let result = super::super::download::do_prepared_download(
        io_dispatcher,
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
    );
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
        }
        Err(error) => retry_or_fail_download(input, error, spawned, event_tx),
    }
}

fn retry_or_fail_download(
    input: DownloadExecInput,
    error: crate::error::Error,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<WorkerEvent>,
) {
    if input.retry_count < input.max_retries {
        let _ = event_tx.send(WorkerEvent::Retried {
            path: input.logical_path.clone(),
            reason: format!(
                "download attempt {} failed: {}",
                input.retry_count + 1,
                error
            ),
        });
        spawned.push(Task::Download {
            url: input.url,
            dest: input.dest,
            logical_path: input.logical_path,
            expected_md5: input.expected_md5,
            expected_size: input.expected_size,
            retry_count: input.retry_count + 1,
            transfer_class: input.transfer_class,
        });
    } else {
        let _ = event_tx.send(WorkerEvent::Failed {
            path: input.logical_path,
            reason: format!("download failed after retries: {}", error),
        });
    }
}

pub(super) fn execute_repair_file(
    input: RepairFileInput,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<WorkerEvent>,
) {
    let RepairFileInput {
        dest,
        logical_path,
        expected_md5,
        expected_size,
        source_candidates,
        download_url,
        allow_copy_fallback,
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
        enqueue_download_or_failure(
            dest,
            logical_path,
            expected_md5,
            expected_size,
            download_url,
            retry_count,
            transfer_class,
            spawned,
            event_tx,
        );
        return;
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
        retry_count,
        transfer_class,
    );
    spawned.extend(initial_groups.into_iter().map(|candidates| Task::VerifyReuseVolume {
        copy_only: initial_copy_only,
        candidates,
        logical_path: logical_path.clone(),
        expected_md5: expected_md5.clone(),
        expected_size,
        group: group.clone(),
    }));
}

#[allow(clippy::too_many_arguments)]
fn enqueue_download_or_failure(
    dest: PathBuf,
    logical_path: String,
    expected_md5: String,
    expected_size: u64,
    download_url: Option<String>,
    retry_count: u32,
    transfer_class: TransferClass,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<WorkerEvent>,
) {
    if let Some(url) = download_url {
        spawned.push(Task::Download {
            url,
            dest,
            logical_path,
            expected_md5,
            expected_size: Some(expected_size),
            retry_count,
            transfer_class,
        });
    } else {
        let _ = event_tx.send(WorkerEvent::Failed {
            path: logical_path,
            reason: "no usable source candidates".to_string(),
        });
    }
}

pub(super) fn execute_verify_reuse_volume(
    copy_only: bool,
    candidates: Vec<PathBuf>,
    logical_path: String,
    expected_md5: String,
    expected_size: u64,
    group: std::sync::Arc<ReuseCandidateGroup>,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<WorkerEvent>,
) {
    if group.is_resolved() {
        return;
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
    group.finish_volume(copy_only, source, spawned, event_tx);
}

pub(super) fn execute_reuse_file(
    input: ReuseFileInput,
    io_dispatcher: Option<&Dispatcher>,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<WorkerEvent>,
) {
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
        retry_count,
        transfer_class,
    } = input;
    match reuse_verified_file(
        io_dispatcher,
        &source,
        &dest,
        &expected_md5,
        expected_size,
        if copy_only {
            ReuseMode::CopyOnly
        } else {
            ReuseMode::HardlinkPreferred
        },
        allow_copy_fallback,
    ) {
        Ok(ReuseMethod::Hardlink) => {
            let _ = event_tx.send(WorkerEvent::Hardlinked { path: dest });
            let _ = event_tx.send(WorkerEvent::Verified {
                path: logical_path,
                ok: true,
                issue: None,
            });
        }
        Ok(ReuseMethod::Copy) => {
            let _ = event_tx.send(WorkerEvent::Copied { path: dest });
            let _ = event_tx.send(WorkerEvent::Verified {
                path: logical_path,
                ok: true,
                issue: None,
            });
        }
        Err(error) => {
            let reason = error.to_string();
            let _ = event_tx.send(WorkerEvent::Retried {
                path: logical_path.clone(),
                reason: format!("verified-source reuse failed: {reason}"),
            });
            if !remaining_source_candidates.is_empty() {
                spawned.push(Task::RepairFile {
                    dest,
                    logical_path,
                    expected_md5,
                    expected_size,
                    source_candidates: remaining_source_candidates,
                    download_url,
                    allow_copy_fallback,
                    retry_count,
                    transfer_class,
                });
            } else if let Some(url) = download_url {
                spawned.push(Task::Download {
                    url,
                    dest,
                    logical_path,
                    expected_md5,
                    expected_size: Some(expected_size),
                    retry_count,
                    transfer_class,
                });
            } else {
                let _ = event_tx.send(WorkerEvent::Failed {
                    path: logical_path,
                    reason,
                });
            }
        }
    }
}
