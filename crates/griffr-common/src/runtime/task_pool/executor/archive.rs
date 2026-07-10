use std::path::PathBuf;

use anyhow::Context;
use compio::dispatcher::Dispatcher;

use super::super::fs_ops::{commit_staged_extract, make_extract_staging_dir};
use super::super::types::{ArchivePart, ProgressEvent, Task};

pub(super) fn execute_install_archive(
    source_dir: PathBuf,
    base_name: String,
    dest: PathBuf,
    cleanup: bool,
    password: Option<String>,
    parts: Vec<ArchivePart>,
    max_retries: u32,
    download_progress_buffer_bytes: usize,
    io_dispatcher: Option<&Dispatcher>,
    user_agent: &str,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<ProgressEvent>,
) {
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
                let _ = event_tx.send(ProgressEvent::Verified {
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
            match super::super::download::do_download(
                io_dispatcher,
                user_agent,
                &part.url,
                &part.dest,
                &part.expected_md5,
                Some(part.expected_size),
                download_progress_buffer_bytes,
                Some(move |bytes| {
                    let _ = event_tx_clone.send(ProgressEvent::DownloadedBytes {
                        path: logical_path_clone.clone(),
                        bytes,
                        total_bytes: expected_size_val,
                    });
                }),
            ) {
                Ok(bytes) => {
                    let _ = event_tx.send(ProgressEvent::Downloaded {
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
                        let _ = event_tx.send(ProgressEvent::Verified {
                            path: part.logical_path.clone(),
                            ok: true,
                            issue: None,
                        });
                        completed = true;
                        break;
                    }
                    if attempt < max_retries {
                        let _ = event_tx.send(ProgressEvent::Retried {
                            path: part.logical_path.clone(),
                            reason: format!(
                                "install-archive verify attempt {} failed",
                                attempt + 1
                            ),
                        });
                        continue;
                    }
                    let _ = event_tx.send(ProgressEvent::Verified {
                        path: part.logical_path.clone(),
                        ok: false,
                        issue: post_issue,
                    });
                    let _ = event_tx.send(ProgressEvent::Failed {
                        path: part.logical_path.clone(),
                        reason: "install-archive verify failed after retries".to_string(),
                    });
                    return;
                }
                Err(err) => {
                    if attempt < max_retries {
                        let _ = event_tx.send(ProgressEvent::Retried {
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
                    let _ = event_tx.send(ProgressEvent::Verified {
                        path: part.logical_path.clone(),
                        ok: false,
                        issue,
                    });
                    let _ = event_tx.send(ProgressEvent::Failed {
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
        source_dir,
        base_name,
        dest,
        cleanup,
        password,
    });
}

pub(super) fn execute_extract_archive(
    source_dir: PathBuf,
    base_name: String,
    dest: PathBuf,
    cleanup: bool,
    password: Option<String>,
    extraction_progress_buffer_bytes: usize,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<ProgressEvent>,
) {
    let progress_path = base_name.clone();
    let event_tx_clone = event_tx.clone();
    let result =
        crate::download::extractor::MultiVolumeExtractor::from_directory(&source_dir, &base_name)
            .and_then(|extractor| {
                let staging_dir = make_extract_staging_dir(&dest, &base_name)?;
                std::fs::create_dir_all(&staging_dir).with_context(|| {
                    format!(
                        "Failed to create extraction staging dir {}",
                        staging_dir.display()
                    )
                })?;
                if let Err(err) = extractor.extract_to_with_progress(
                    &staging_dir,
                    password.as_deref(),
                    extraction_progress_buffer_bytes,
                    Some(move |bytes, total_bytes| {
                        let _ = event_tx_clone.send(ProgressEvent::ExtractedBytes {
                            path: progress_path.clone(),
                            bytes,
                            total_bytes,
                        });
                    }),
                ) {
                    let _ = std::fs::remove_dir_all(&staging_dir);
                    return Err(err);
                }
                if let Err(err) = commit_staged_extract(&staging_dir, &dest) {
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
            let _ = event_tx.send(ProgressEvent::Extracted { path: dest });
        }
        Err(err) => {
            let _ = event_tx.send(ProgressEvent::Failed {
                path: format!("{}/{}", source_dir.display(), base_name),
                reason: err.to_string(),
            });
        }
    }
}
