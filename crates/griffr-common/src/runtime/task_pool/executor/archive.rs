use std::path::PathBuf;

use crate::error::Error;
use compio::dispatcher::Dispatcher;

use super::super::fs_ops::{commit_staged_extract, make_extract_staging_dir};
use super::super::types::{ArchivePart, Task, WorkerEvent};

pub(super) fn execute_install_archive(
    base_name: String,
    dest: PathBuf,
    cleanup: bool,
    password: Option<String>,
    mut parts: Vec<ArchivePart>,
    max_retries: u32,
    download_progress_buffer_bytes: usize,
    io_dispatcher: Option<&Dispatcher>,
    user_agent: &str,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<WorkerEvent>,
) {
    parts.sort_by(|left, right| {
        left.sequence
            .cmp(&right.sequence)
            .then_with(|| left.logical_path.cmp(&right.logical_path))
    });
    let volumes = parts.iter().map(|part| part.dest.clone()).collect();
    for part in parts {
        let mut completed = false;
        for attempt in 0..=max_retries {
            if super::super::verify::build_issue(
                &part.dest,
                &part.logical_path,
                &part.expected_md5,
                Some(part.expected_size),
            )
            .is_none()
            {
                let _ = event_tx.send(WorkerEvent::Verified {
                    path: part.logical_path.clone(),
                    ok: true,
                    issue: None,
                });
                completed = true;
                break;
            }

            let event_tx_clone = event_tx.clone();
            let logical_path_clone = part.logical_path.clone();
            let expected_size_val = part.expected_size;
            let _ = event_tx.send(WorkerEvent::DownloadStarted {
                path: part.logical_path.clone(),
                total_bytes: expected_size_val,
            });
            match super::super::download::do_download(
                io_dispatcher,
                user_agent,
                &part.url,
                &part.dest,
                &part.expected_md5,
                Some(part.expected_size),
                download_progress_buffer_bytes,
                Some(move |bytes| {
                    let _ = event_tx_clone.send(WorkerEvent::DownloadedBytes {
                        path: logical_path_clone.clone(),
                        bytes,
                        total_bytes: expected_size_val,
                    });
                }),
            ) {
                Ok(bytes) => {
                    let _ = event_tx.send(WorkerEvent::Downloaded {
                        path: part.logical_path.clone(),
                        bytes,
                    });
                    let post_issue = super::super::verify::build_issue(
                        &part.dest,
                        &part.logical_path,
                        &part.expected_md5,
                        Some(part.expected_size),
                    );
                    if post_issue.is_none() {
                        let _ = event_tx.send(WorkerEvent::Verified {
                            path: part.logical_path.clone(),
                            ok: true,
                            issue: None,
                        });
                        completed = true;
                        break;
                    }
                    if attempt < max_retries {
                        let _ = event_tx.send(WorkerEvent::Retried {
                            path: part.logical_path.clone(),
                            reason: format!(
                                "install-archive verify attempt {} failed",
                                attempt + 1
                            ),
                        });
                        continue;
                    }
                    let _ = event_tx.send(WorkerEvent::Verified {
                        path: part.logical_path.clone(),
                        ok: false,
                        issue: post_issue,
                    });
                    let _ = event_tx.send(WorkerEvent::Failed {
                        path: part.logical_path.clone(),
                        reason: "install-archive verify failed after retries".to_string(),
                    });
                    return;
                }
                Err(err) => {
                    if attempt < max_retries {
                        let _ = event_tx.send(WorkerEvent::Retried {
                            path: part.logical_path.clone(),
                            reason: format!(
                                "install-archive download attempt {} failed: {}",
                                attempt + 1,
                                err
                            ),
                        });
                        continue;
                    }
                    let issue = super::super::verify::build_issue(
                        &part.dest,
                        &part.logical_path,
                        &part.expected_md5,
                        Some(part.expected_size),
                    );
                    let _ = event_tx.send(WorkerEvent::Verified {
                        path: part.logical_path.clone(),
                        ok: false,
                        issue,
                    });
                    let _ = event_tx.send(WorkerEvent::Failed {
                        path: part.logical_path.clone(),
                        reason: format!("install-archive download failed after retries: {}", err),
                    });
                    return;
                }
            }
        }
        if !completed {
            return;
        }
    }

    spawned.push(Task::Extract {
        base_name,
        volumes,
        dest,
        cleanup,
        password,
    });
}

pub(super) fn execute_extract_archive(
    base_name: String,
    volumes: Vec<PathBuf>,
    dest: PathBuf,
    cleanup: bool,
    password: Option<String>,
    extraction_progress_buffer_bytes: usize,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<WorkerEvent>,
) {
    let progress_path = base_name.clone();
    let event_tx_clone = event_tx.clone();
    let result =
        crate::download::extractor::MultiVolumeExtractor::new(volumes).and_then(|extractor| {
            let staging_dir = make_extract_staging_dir(&dest, &base_name)?;
            std::fs::create_dir_all(&staging_dir).map_err(|e| Error::CreateDirFailed {
                path: staging_dir.clone(),
                source: e,
            })?;
            if let Err(err) = extractor.extract_to_with_progress(
                &staging_dir,
                password.as_deref(),
                extraction_progress_buffer_bytes,
                Some(move |bytes, total_bytes| {
                    let _ = event_tx_clone.send(WorkerEvent::ExtractedBytes {
                        path: progress_path.clone(),
                        bytes,
                        total_bytes,
                    });
                }),
            ) {
                let _ = std::fs::remove_dir_all(&staging_dir);
                return Err(err);
            }
            let mut on_commit = |path: &std::path::Path, completed: usize, total: usize| {
                let _ = event_tx.send(WorkerEvent::ArchiveCommitProgress {
                    path: path.to_string_lossy().replace('\\', "/"),
                    completed,
                    total,
                });
            };
            if let Err(err) = commit_staged_extract(&staging_dir, &dest, Some(&mut on_commit)) {
                let _ = std::fs::remove_dir_all(&staging_dir);
                return Err(err);
            }
            spawned.push(Task::ApplyExtractedVfsPatchManifest {
                install_root: dest.clone(),
            });
            if cleanup {
                extractor.cleanup()?;
            }
            Ok(())
        });
    match result {
        Ok(()) => {
            let _ = event_tx.send(WorkerEvent::Extracted { path: dest });
        }
        Err(err) => {
            let _ = event_tx.send(WorkerEvent::Failed {
                path: base_name,
                reason: err.to_string(),
            });
        }
    }
}
