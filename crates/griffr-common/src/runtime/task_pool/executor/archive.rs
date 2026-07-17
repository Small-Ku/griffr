use std::path::PathBuf;

use crate::error::Error;
use crate::runtime::{build_patch_execution_plan, PatchApplyOptions};
use compio::dispatcher::Dispatcher;

use super::super::fs_ops::{
    commit_staged_extract, execute_patch_transaction, make_extract_staging_dir,
};
use super::super::types::{ArchiveInstallGroup, ArchivePart, Task, WorkerEvent};

pub(super) fn execute_install_archive(
    base_name: String,
    dest: PathBuf,
    cleanup: bool,
    password: Option<String>,
    patch_options: PatchApplyOptions,
    mut parts: Vec<ArchivePart>,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<WorkerEvent>,
) {
    parts.sort_by(|left, right| {
        left.sequence
            .cmp(&right.sequence)
            .then_with(|| left.logical_path.cmp(&right.logical_path))
    });
    if parts.is_empty() {
        let _ = event_tx.send(WorkerEvent::Failed {
            path: base_name,
            reason: "install archive has no parts".to_string(),
        });
        return;
    }

    let volumes = parts.iter().map(|part| part.dest.clone()).collect();
    let continuation = Task::Extract {
        base_name,
        volumes,
        dest,
        cleanup,
        password,
        patch_options,
    };
    let group = ArchiveInstallGroup::new(parts.len(), continuation);
    spawned.extend(parts.into_iter().map(|part| Task::InstallArchivePart {
        part,
        group: group.clone(),
    }));
}

pub(super) fn execute_install_archive_part(
    part: ArchivePart,
    group: std::sync::Arc<ArchiveInstallGroup>,
    max_retries: u32,
    download_progress_buffer_bytes: usize,
    io_dispatcher: Option<&Dispatcher>,
    user_agent: &str,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<WorkerEvent>,
) {
    let mut succeeded = false;
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
            succeeded = true;
            break;
        }

        let event_tx_clone = event_tx.clone();
        let logical_path_clone = part.logical_path.clone();
        let expected_size = part.expected_size;
        let _ = event_tx.send(WorkerEvent::DownloadStarted {
            path: part.logical_path.clone(),
            total_bytes: expected_size,
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
                    total_bytes: expected_size,
                });
            }),
        ) {
            Ok(bytes) => {
                let _ = event_tx.send(WorkerEvent::Downloaded {
                    path: part.logical_path.clone(),
                    bytes,
                });
                let _ = event_tx.send(WorkerEvent::Verified {
                    path: part.logical_path.clone(),
                    ok: true,
                    issue: None,
                });
                succeeded = true;
                break;
            }
            Err(error) if attempt < max_retries => {
                let _ = event_tx.send(WorkerEvent::Retried {
                    path: part.logical_path.clone(),
                    reason: format!(
                        "install-archive download attempt {} failed: {}",
                        attempt + 1,
                        error
                    ),
                });
            }
            Err(error) => {
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
                    reason: format!(
                        "install-archive download failed after retries: {}",
                        error
                    ),
                });
                break;
            }
        }
    }
    group.finish_part(succeeded, spawned);
}

pub(super) fn execute_extract_archive(
    base_name: String,
    volumes: Vec<PathBuf>,
    dest: PathBuf,
    cleanup: bool,
    password: Option<String>,
    patch_options: PatchApplyOptions,
    extraction_progress_buffer_bytes: usize,
    patch_slots: usize,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<WorkerEvent>,
) {
    let progress_path = base_name.clone();
    let event_tx_clone = event_tx.clone();
    let result =
        crate::download::extractor::MultiVolumeExtractor::new(volumes).and_then(|extractor| {
            let patch_options = patch_options.resolved_for_install(&dest)?;
            let staging_dir =
                make_extract_staging_dir(&dest, &base_name, patch_options.work_dir.as_deref())?;
            std::fs::create_dir_all(&staging_dir).map_err(|source| Error::CreateDirFailed {
                path: staging_dir.clone(),
                source,
            })?;

            let inspection = extractor.inspect_patch_payload(password.as_deref())?;
            let patch_plan = if inspection.patch_manifest.is_some() {
                Some(build_patch_execution_plan(
                    &dest,
                    &staging_dir,
                    &inspection,
                    &patch_options,
                )?)
            } else {
                None
            };
            if let Some((_, report)) = patch_plan.as_ref() {
                let _ = event_tx.send(WorkerEvent::ArchivePreflight {
                    path: base_name.clone(),
                    report: report.clone(),
                });
            }

            if let Err(error) = extractor.extract_to_with_progress(
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
                return Err(error);
            }

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

            if let Some((plan, report)) = patch_plan {
                execute_patch_transaction(
                    &plan,
                    Some(&report),
                    Some(&mut on_commit),
                    Some(&mut on_patch),
                    Some(&mut on_delete),
                    patch_slots,
                )?;
                if staging_dir.exists() {
                    std::fs::remove_dir_all(&staging_dir).map_err(|source| {
                        Error::RemoveFailed {
                            path: staging_dir.clone(),
                            source,
                        }
                    })?;
                }
            } else {
                if let Err(error) = commit_staged_extract(&staging_dir, &dest, Some(&mut on_commit))
                {
                    let _ = std::fs::remove_dir_all(&staging_dir);
                    return Err(error);
                }
                spawned.push(Task::ApplyExtractedVfsPatchManifest {
                    install_root: dest.clone(),
                });
            }

            if cleanup {
                extractor.cleanup()?;
            }
            Ok(())
        });
    match result {
        Ok(()) => {
            let _ = event_tx.send(WorkerEvent::Extracted { path: dest });
        }
        Err(error) => {
            let _ = event_tx.send(WorkerEvent::Failed {
                path: base_name,
                reason: error.to_string(),
            });
        }
    }
}
