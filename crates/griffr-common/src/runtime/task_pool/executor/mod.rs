use compio::dispatcher::Dispatcher;

use super::fs_ops::{
    apply_delete_files_manifest, apply_extracted_vfs_patch_manifest, create_hardlink,
    resume_patch_transaction,
};
use super::types::{Task, WorkerEvent};

mod archive;
mod transfer;

pub(crate) fn execute_task(
    task: Task,
    max_retries: u32,
    extraction_progress_buffer_bytes: usize,
    download_progress_buffer_bytes: usize,
    extract_shards: usize,
    io_dispatcher: Option<&Dispatcher>,
    http_client: &cyper::Client,
    user_agent: &str,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<WorkerEvent>,
) {
    match task {
        Task::InstallArchive {
            base_name,
            dest,
            cleanup,
            password,
            patch_options,
            parts,
        } => archive::execute_install_archive(
            base_name,
            dest,
            cleanup,
            password,
            patch_options,
            parts,
            spawned,
            event_tx,
        ),
        Task::InstallArchivePart {
            part,
            group,
            retry_count,
        } => archive::execute_install_archive_part(
            part,
            group,
            retry_count,
            max_retries,
            io_dispatcher,
            spawned,
            event_tx,
        ),
        Task::TransferArchivePart {
            part,
            group,
            retry_count,
            resume,
        } => archive::execute_transfer_archive_part(
            part,
            group,
            retry_count,
            resume,
            max_retries,
            download_progress_buffer_bytes,
            io_dispatcher,
            http_client,
            user_agent,
            spawned,
            event_tx,
        ),
        Task::Verify {
            path,
            logical_path,
            expected_md5,
            expected_size,
            on_fail,
        } => super::verify::execute_verify(
            &path,
            &logical_path,
            &expected_md5,
            expected_size,
            on_fail,
            spawned,
            event_tx,
        ),
        Task::Download {
            url,
            dest,
            logical_path,
            expected_md5,
            expected_size,
            retry_count,
            transfer_class,
        } => transfer::execute_prepare_download(
            transfer::DownloadExecInput {
                url,
                dest,
                logical_path,
                expected_md5,
                expected_size,
                retry_count,
                max_retries,
                transfer_class,
            },
            io_dispatcher,
            spawned,
            event_tx,
        ),
        Task::TransferDownload {
            url,
            dest,
            logical_path,
            expected_md5,
            expected_size,
            retry_count,
            transfer_class,
            resume,
        } => transfer::execute_transfer_download(
            transfer::DownloadExecInput {
                url,
                dest,
                logical_path,
                expected_md5,
                expected_size,
                retry_count,
                max_retries,
                transfer_class,
            },
            resume,
            download_progress_buffer_bytes,
            io_dispatcher,
            http_client,
            user_agent,
            spawned,
            event_tx,
        ),
        Task::RepairFile {
            dest,
            logical_path,
            expected_md5,
            expected_size,
            source_candidates,
            download_url,
            allow_copy_fallback,
            retry_count,
            transfer_class,
        } => transfer::execute_repair_file(
            transfer::RepairFileInput {
                dest,
                logical_path,
                expected_md5,
                expected_size,
                source_candidates,
                download_url,
                allow_copy_fallback,
                retry_count,
                transfer_class,
            },
            spawned,
            event_tx,
        ),
        Task::VerifyReuseVolume {
            copy_only,
            candidates,
            logical_path,
            expected_md5,
            expected_size,
            group,
        } => transfer::execute_verify_reuse_volume(
            copy_only,
            candidates,
            logical_path,
            expected_md5,
            expected_size,
            group,
            spawned,
            event_tx,
        ),
        Task::ReuseFile {
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
        } => transfer::execute_reuse_file(
            transfer::ReuseFileInput {
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
            },
            io_dispatcher,
            spawned,
            event_tx,
        ),
        Task::Hardlink { src, dest } => match create_hardlink(io_dispatcher, &src, &dest) {
            Ok(()) => {
                let _ = event_tx.send(WorkerEvent::Hardlinked { path: dest });
            }
            Err(err) => {
                let _ = event_tx.send(WorkerEvent::Failed {
                    path: dest.display().to_string(),
                    reason: err.to_string(),
                });
            }
        },
        Task::Extract {
            base_name,
            volumes,
            dest,
            cleanup,
            password,
            patch_options,
        } => archive::execute_schedule_extract(
            base_name,
            volumes,
            dest,
            cleanup,
            password,
            patch_options,
            spawned,
        ),
        Task::PrepareArchive { work } => {
            archive::execute_prepare_archive(work, extract_shards, spawned, event_tx)
        }
        Task::ExtractArchiveShard { shard } => archive::execute_extract_archive_shard(
            shard,
            extraction_progress_buffer_bytes,
            spawned,
            event_tx,
        ),
        Task::CommitArchive { work } => archive::execute_commit_archive(work, spawned, event_tx),
        Task::CleanupArchive { work } => archive::execute_cleanup_archive(work, event_tx),
        Task::ApplyExtractedVfsPatchManifest { install_root } => {
            let plan_path = install_root
                .join(crate::runtime::PATCH_TRANSACTION_DIR)
                .join(crate::runtime::PATCH_PLAN_NAME);
            let has_transaction_plan = plan_path.is_file();
            let result = if has_transaction_plan {
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
                resume_patch_transaction(
                    &install_root,
                    Some(&mut on_commit),
                    Some(&mut on_patch),
                    Some(&mut on_delete),
                )
            } else {
                let mut on_progress = |path: &str, completed: usize, total: usize| {
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
                apply_extracted_vfs_patch_manifest(&install_root, Some(&mut on_progress))
            };
            match result {
                Ok(()) if !has_transaction_plan => {
                    spawned.push(Task::ApplyDeleteManifest { install_root });
                }
                Ok(()) => {}
                Err(err) => {
                    let _ = event_tx.send(WorkerEvent::Failed {
                        path: install_root.display().to_string(),
                        reason: err.to_string(),
                    });
                }
            }
        }
        Task::ApplyDeleteManifest { install_root } => {
            let result = {
                let mut on_progress = |path: &std::path::Path, completed: usize, total: usize| {
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
                apply_delete_files_manifest(&install_root, Some(&mut on_progress))
            };
            match result {
                Ok(()) => {}
                Err(err) => {
                    let _ = event_tx.send(WorkerEvent::Failed {
                        path: install_root.display().to_string(),
                        reason: err.to_string(),
                    });
                }
            }
        }
    }
}
