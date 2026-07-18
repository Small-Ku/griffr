use super::fs_ops::{
    apply_delete_files_manifest, apply_extracted_vfs_patch_manifest, create_hardlink_async,
    resume_patch_transaction,
};
use super::types::{Task, WorkerEvent};

mod archive;
mod transfer;

pub(crate) fn execute_blocking_task(
    task: Task,
    max_retries: u32,
    extraction_progress_buffer_bytes: usize,
    extract_shards: usize,
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
        } => transfer::execute_copy_reuse_file(
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
            spawned,
            event_tx,
        ),
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
        Task::TransferDownload { .. }
        | Task::TransferArchivePart { .. }
        | Task::Hardlink { .. } => unreachable!("async I/O task routed to blocking executor"),
    }
}

pub(crate) async fn execute_async_task(
    task: Task,
    max_retries: u32,
    download_progress_buffer_bytes: usize,
    user_agent: &str,
    spawned: &mut Vec<Task>,
    event_tx: &flume::Sender<WorkerEvent>,
) {
    match task {
        Task::TransferArchivePart {
            part,
            group,
            retry_count,
            resume,
        } => {
            archive::execute_transfer_archive_part(
                part,
                group,
                retry_count,
                resume,
                max_retries,
                download_progress_buffer_bytes,
                user_agent,
                spawned,
                event_tx,
            )
            .await
        }
        Task::TransferDownload {
            url,
            dest,
            logical_path,
            expected_md5,
            expected_size,
            retry_count,
            transfer_class,
            resume,
        } => {
            transfer::execute_transfer_download(
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
                user_agent,
                spawned,
                event_tx,
            )
            .await
        }
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
        } => {
            transfer::execute_hardlink_reuse_file(
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
                spawned,
                event_tx,
            )
            .await
        }
        Task::Hardlink { src, dest } => match create_hardlink_async(&src, &dest).await {
            Ok(()) => {
                let _ = event_tx.send(WorkerEvent::Hardlinked { path: dest });
            }
            Err(error) => {
                let _ = event_tx.send(WorkerEvent::Failed {
                    path: dest.display().to_string(),
                    reason: error.to_string(),
                });
            }
        },
        _ => unreachable!("blocking task routed to async I/O executor"),
    }
}
