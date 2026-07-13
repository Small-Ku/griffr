use std::path::PathBuf;

use compio::dispatcher::Dispatcher;

use super::super::fs_ops::{reuse_file, ReuseMethod};
use super::super::types::{ProgressEvent, Task};

pub(super) struct DownloadExecInput {
    pub(super) url: String,
    pub(super) dest: PathBuf,
    pub(super) logical_path: String,
    pub(super) expected_md5: String,
    pub(super) expected_size: Option<u64>,
    pub(super) retry_count: u32,
    pub(super) max_retries: u32,
}

pub(super) struct EnsureFileInput {
    pub(super) dest: PathBuf,
    pub(super) logical_path: String,
    pub(super) expected_md5: String,
    pub(super) expected_size: u64,
    pub(super) source_candidates: Vec<PathBuf>,
    pub(super) download_url: Option<String>,
    pub(super) allow_copy_fallback: bool,
    pub(super) prefer_reuse: bool,
    pub(super) retry_count: u32,
    pub(super) max_retries: u32,
}

pub(super) fn execute_download(
    input: DownloadExecInput,
    download_progress_buffer_bytes: usize,
    io_dispatcher: Option<&Dispatcher>,
    user_agent: &str,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<ProgressEvent>,
) {
    let event_tx_clone = event_tx.clone();
    let logical_path_clone = input.logical_path.clone();
    let expected_size_val = input.expected_size;
    let _ = event_tx.send(ProgressEvent::DownloadStarted {
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
            let _ = event_tx_clone.send(ProgressEvent::DownloadedBytes {
                path: logical_path_clone.clone(),
                bytes,
                total_bytes: expected_size_val.unwrap_or(bytes),
            });
        }),
    );
    match result {
        Ok(bytes) => {
            let _ = event_tx.send(ProgressEvent::Downloaded {
                path: input.logical_path.clone(),
                bytes,
            });
            let on_fail = if input.retry_count < input.max_retries {
                Some(Box::new(Task::Download {
                    url: input.url.clone(),
                    dest: input.dest.clone(),
                    logical_path: input.logical_path.clone(),
                    expected_md5: input.expected_md5.clone(),
                    expected_size: input.expected_size,
                    retry_count: input.retry_count + 1,
                }))
            } else {
                None
            };
            spawned.push(Task::Verify {
                path: input.dest,
                logical_path: input.logical_path,
                expected_md5: input.expected_md5,
                expected_size: input.expected_size,
                on_fail,
            });
        }
        Err(err) => {
            if input.retry_count < input.max_retries {
                let _ = event_tx.send(ProgressEvent::Retried {
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
                });
            } else {
                let _ = event_tx.send(ProgressEvent::Failed {
                    path: input.logical_path.clone(),
                    reason: format!("download failed after retries: {}", err),
                });
                spawned.push(Task::Verify {
                    path: input.dest,
                    logical_path: input.logical_path,
                    expected_md5: input.expected_md5,
                    expected_size: input.expected_size,
                    on_fail: None,
                });
            }
        }
    }
}

pub(super) fn execute_ensure_file(
    input: EnsureFileInput,
    download_progress_buffer_bytes: usize,
    io_dispatcher: Option<&Dispatcher>,
    user_agent: &str,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<ProgressEvent>,
) {
    let existing_ok = super::super::verify::build_issue(
        &input.dest,
        &input.logical_path,
        &input.expected_md5,
        Some(input.expected_size),
    )
    .is_none();
    if existing_ok && !input.prefer_reuse {
        let _ = event_tx.send(ProgressEvent::Verified {
            path: input.logical_path,
            ok: true,
            issue: None,
        });
        return;
    }
    let mut reuse_error = None;
    for source in &input.source_candidates {
        if super::super::verify::build_issue(
            source,
            &input.logical_path,
            &input.expected_md5,
            Some(input.expected_size),
        )
        .is_some()
        {
            continue;
        }
        match reuse_file(
            io_dispatcher,
            source,
            &input.dest,
            input.allow_copy_fallback,
        ) {
            Ok(ReuseMethod::Hardlink) => {
                let _ = event_tx.send(ProgressEvent::Hardlinked {
                    path: input.dest.clone(),
                });
                if super::super::verify::build_issue(
                    &input.dest,
                    &input.logical_path,
                    &input.expected_md5,
                    Some(input.expected_size),
                )
                .is_none()
                {
                    let _ = event_tx.send(ProgressEvent::Verified {
                        path: input.logical_path,
                        ok: true,
                        issue: None,
                    });
                    return;
                }
            }
            Ok(ReuseMethod::Copy) => {
                let _ = event_tx.send(ProgressEvent::Copied {
                    path: input.dest.clone(),
                });
                if super::super::verify::build_issue(
                    &input.dest,
                    &input.logical_path,
                    &input.expected_md5,
                    Some(input.expected_size),
                )
                .is_none()
                {
                    let _ = event_tx.send(ProgressEvent::Verified {
                        path: input.logical_path,
                        ok: true,
                        issue: None,
                    });
                    return;
                }
            }
            Err(err) => {
                reuse_error = Some(err.to_string());
            }
        }
    }

    if let Some(url) = input.download_url {
        execute_download(
            DownloadExecInput {
                url,
                dest: input.dest,
                logical_path: input.logical_path,
                expected_md5: input.expected_md5,
                expected_size: Some(input.expected_size),
                retry_count: input.retry_count,
                max_retries: input.max_retries,
            },
            download_progress_buffer_bytes,
            io_dispatcher,
            user_agent,
            spawned,
            event_tx,
        );
        return;
    }

    let issue = super::super::verify::build_issue(
        &input.dest,
        &input.logical_path,
        &input.expected_md5,
        Some(input.expected_size),
    );
    let _ = event_tx.send(ProgressEvent::Verified {
        path: input.logical_path.clone(),
        ok: false,
        issue,
    });
    let _ = event_tx.send(ProgressEvent::Failed {
        path: input.logical_path,
        reason: reuse_error.unwrap_or_else(|| "no usable source candidates".to_string()),
    });
}
