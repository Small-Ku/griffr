use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use compio::dispatcher::Dispatcher;

use super::super::fs_ops::{reuse_verified_file, ReuseMethod};
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

pub(super) fn execute_download(
    input: DownloadExecInput,
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
    let result = super::super::download::do_download(
        io_dispatcher,
        user_agent,
        &input.url,
        &input.dest,
        &input.expected_md5,
        input.expected_size,
        download_progress_buffer_bytes,
        Some(move |bytes| {
            let _ = event_tx_clone.send(WorkerEvent::DownloadedBytes {
                path: logical_path_clone.clone(),
                bytes,
                total_bytes: expected_size_val.unwrap_or(bytes),
            });
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
        Err(err) if input.retry_count < input.max_retries => {
            let _ = event_tx.send(WorkerEvent::Retried {
                path: input.logical_path.clone(),
                reason: format!("download attempt {} failed: {}", input.retry_count + 1, err),
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
        }
        Err(err) => {
            let _ = event_tx.send(WorkerEvent::Failed {
                path: input.logical_path,
                reason: format!("download failed after retries: {}", err),
            });
        }
    }
}

fn source_volume_key(path: &Path) -> PathBuf {
    match path.components().next() {
        Some(Component::Prefix(prefix)) => PathBuf::from(prefix.as_os_str()),
        Some(Component::RootDir) => PathBuf::from(std::path::MAIN_SEPARATOR.to_string()),
        Some(component) => PathBuf::from(component.as_os_str()),
        None => PathBuf::new(),
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

    let mut seen = BTreeSet::new();
    let mut by_volume = BTreeMap::<PathBuf, Vec<PathBuf>>::new();
    for source in source_candidates {
        let normalized = std::fs::canonicalize(&source).unwrap_or(source);
        if seen.insert(normalized.clone()) {
            by_volume
                .entry(source_volume_key(&normalized))
                .or_default()
                .push(normalized);
        }
    }
    if by_volume.is_empty() {
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
        return;
    }

    let group = ReuseCandidateGroup::new(
        by_volume.len(),
        dest,
        logical_path.clone(),
        expected_md5.clone(),
        expected_size,
        download_url,
        allow_copy_fallback,
        retry_count,
        transfer_class,
    );
    spawned.extend(
        by_volume
            .into_values()
            .enumerate()
            .map(|(group_index, candidates)| Task::VerifyReuseVolume {
                group_index,
                candidates,
                logical_path: logical_path.clone(),
                expected_md5: expected_md5.clone(),
                expected_size,
                group: group.clone(),
            }),
    );
}

pub(super) fn execute_verify_reuse_volume(
    group_index: usize,
    candidates: Vec<PathBuf>,
    logical_path: String,
    expected_md5: String,
    expected_size: u64,
    group: std::sync::Arc<ReuseCandidateGroup>,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<WorkerEvent>,
) {
    let source = candidates.into_iter().find(|source| {
        super::super::verify::build_issue(
            source,
            &logical_path,
            &expected_md5,
            Some(expected_size),
        )
        .is_none()
    });
    group.finish_volume(group_index, source, spawned, event_tx);
}

pub(super) fn execute_reuse_file(
    input: ReuseFileInput,
    io_dispatcher: Option<&Dispatcher>,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<WorkerEvent>,
) {
    let ReuseFileInput {
        source,
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
                let mut remaining_source_candidates = remaining_source_candidates;
                let source = remaining_source_candidates.remove(0);
                spawned.push(Task::ReuseFile {
                    source,
                    remaining_source_candidates,
                    dest,
                    logical_path,
                    expected_md5,
                    expected_size,
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
