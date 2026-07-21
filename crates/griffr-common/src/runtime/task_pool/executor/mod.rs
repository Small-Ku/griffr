use super::fs_ops::{
    apply_delete_files_manifest_async, apply_extracted_vfs_patch_manifest, create_hardlink_async,
    resume_patch_transaction,
};
use super::graph::TaskExecution;
use super::types::{Task, WorkerEvent};

mod archive;
mod transfer;

pub(crate) fn execute_blocking_task(
    task: Task,
    max_retries: u32,
    extraction_progress_buffer_bytes: usize,
    extract_shards: usize,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
    match task {
        Task::InstallArchive {
            base_name,
            dest,
            retention,
            password,
            patch_options,
            expected_files,
            excluded_commit_paths,
            parts,
        } => archive::execute_install_archive(
            base_name,
            dest,
            retention,
            password,
            patch_options,
            expected_files,
            excluded_commit_paths,
            parts,
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
            verify_destination_fallback,
            retry_count,
            transfer_class,
        } => transfer::execute_repair_file(transfer::RepairFileInput {
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
        }),
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
        ),
        Task::Extract {
            base_name,
            volumes,
            dest,
            retention,
            password,
            patch_options,
            expected_files,
            excluded_commit_paths,
        } => archive::execute_schedule_extract(
            base_name,
            volumes,
            dest,
            retention,
            password,
            patch_options,
            expected_files,
            excluded_commit_paths,
        ),
        Task::DiscoverArchiveDirectory {
            work,
            required_range,
        } => archive::execute_discover_archive_directory(work, required_range),
        Task::InspectArchiveIndex { work, directory } => {
            archive::execute_read_archive_index(work, directory)
        }
        Task::ReadArchiveControls {
            work,
            archive_index,
        } => archive::execute_read_archive_controls(work, archive_index),
        Task::PlanArchiveExtraction {
            work,
            archive_index,
        } => {
            archive::execute_plan_archive_extraction(work, archive_index, extract_shards, event_tx)
        }
        Task::ProbePatchArtifact {
            patch_check,
            probe_index,
        } => archive::execute_probe_patch_artifact(patch_check, probe_index),
        Task::MeasurePatchRelocation { patch_check } => {
            archive::execute_measure_patch_relocation(patch_check)
        }
        Task::FinalizePatchPlan {
            work,
            archive_index,
            patch_check,
        } => archive::execute_finalize_patch_plan(work, archive_index, patch_check, event_tx),
        Task::ExtractArchiveShard { shard } => archive::execute_extract_archive_shard(
            shard,
            extraction_progress_buffer_bytes,
            event_tx,
        ),
        Task::FillArchiveVolumeGaps { work, volume_index } => {
            archive::execute_fill_archive_volume_gaps(work, volume_index)
        }
        Task::FinalizeArchiveVolume { work, volume_index } => {
            archive::execute_finalize_archive_volume(work, volume_index, event_tx)
        }
        Task::ArchiveVolumesComplete { work } => archive::execute_archive_volumes_complete(work),
        Task::CommitArchive { work } => archive::execute_commit_archive(work, event_tx),
        Task::CommitArchiveBatch {
            commit,
            batch_index,
        } => archive::execute_commit_archive_batch(commit, batch_index, event_tx),
        Task::VerifyCommittedBatch {
            commit,
            batch_index,
        } => archive::execute_verify_committed_batch(commit, batch_index, event_tx),
        Task::FinishArchiveCommit { commit } => archive::execute_finish_archive_commit(commit),
        Task::PreparePatchTransaction { transaction } => {
            archive::execute_prepare_patch_transaction(transaction, event_tx)
        }
        Task::ApplyPatchEntry {
            transaction,
            entry_index,
        } => archive::execute_apply_patch_entry(transaction, entry_index, event_tx),
        Task::ReleasePatchBase { transaction, base } => {
            archive::execute_release_patch_base(transaction, base)
        }
        Task::ApplyPatchDeletes { transaction } => {
            archive::execute_apply_patch_deletes(transaction, event_tx)
        }
        Task::CommitPatchDeferred { transaction } => {
            archive::execute_commit_patch_deferred(transaction)
        }
        Task::CleanupPatchTransaction {
            transaction,
            archive: archive_work,
        } => archive::execute_cleanup_patch_transaction(transaction, archive_work),
        Task::CleanupArchive { work } => archive::execute_cleanup_archive(work, event_tx),
        Task::ApplyExtractedVfsPatchManifest { install_root } => {
            execute_apply_patch_manifest(install_root, event_tx)
        }
        Task::TransferDownload { .. }
        | Task::FetchArchiveRange { .. }
        | Task::ReuseFile { .. }
        | Task::ApplyDeleteManifest { .. }
        | Task::Hardlink { .. } => unreachable!("async I/O task routed to blocking executor"),
    }
}

pub(crate) async fn execute_async_task(
    task: Task,
    max_retries: u32,
    download_progress_buffer_bytes: usize,
    user_agent: &str,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
    match task {
        Task::FetchArchiveRange {
            work,
            request,
            retry_count,
            priority,
        } => {
            archive::execute_fetch_archive_range(
                work,
                request,
                retry_count,
                priority,
                max_retries,
                download_progress_buffer_bytes,
                user_agent,
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
            verify_destination_fallback,
            retry_count,
            transfer_class,
        } => {
            let input = transfer::ReuseFileInput {
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
            };
            if copy_only {
                transfer::execute_copy_reuse_file(input, event_tx).await
            } else {
                transfer::execute_hardlink_reuse_file(input, event_tx).await
            }
        }
        Task::ApplyDeleteManifest { install_root } => {
            execute_apply_delete_manifest(install_root, event_tx).await
        }
        Task::Hardlink { src, dest } => match create_hardlink_async(&src, &dest).await {
            Ok(()) => {
                let _ = event_tx.send(WorkerEvent::Hardlinked { path: dest });
                TaskExecution::succeeded()
            }
            Err(error) => TaskExecution::failed(error.to_string()),
        },
        _ => unreachable!("blocking task routed to async I/O executor"),
    }
}

fn execute_apply_patch_manifest(
    install_root: std::path::PathBuf,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
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
            TaskExecution::then(Task::ApplyDeleteManifest { install_root })
        }
        Ok(()) => TaskExecution::succeeded(),
        Err(error) => TaskExecution::failed(error.to_string()),
    }
}

async fn execute_apply_delete_manifest(
    install_root: std::path::PathBuf,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
    let result = apply_delete_files_manifest_async(
        &install_root,
        Some(|path: &std::path::Path, completed: usize, total: usize| {
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
        }),
    )
    .await;
    match result {
        Ok(()) => TaskExecution::succeeded(),
        Err(error) => TaskExecution::failed(error.to_string()),
    }
}
