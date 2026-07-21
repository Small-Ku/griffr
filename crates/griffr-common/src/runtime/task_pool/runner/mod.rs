use super::fs_ops::{
    apply_delete_files_manifest_async, apply_extracted_vfs_patch_manifest, create_hardlink_async,
    resume_patch_apply,
};
use super::graph::TaskRun;
use super::types::{Task, WorkerEvent};

mod archive;
mod transfer;

pub(crate) fn run_blocking_task(
    task: Task,
    max_retries: u32,
    extraction_progress_buffer_bytes: usize,
    extract_shards: usize,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    match task {
        Task::OpenArchive {
            base_name,
            source,
            dest,
            retention,
            password,
            patch_options,
            expected_files,
            excluded_commit_paths,
        } => archive::run_open_archive(
            base_name,
            source,
            dest,
            retention,
            password,
            patch_options,
            expected_files,
            excluded_commit_paths,
        ),
        Task::Verify {
            path,
            logical_path,
            expected_md5,
            expected_size,
            on_fail,
        } => super::verify::run_verify(
            &path,
            &logical_path,
            &expected_md5,
            expected_size,
            on_fail,
            event_tx,
        ),
        task @ Task::Download { resume: None, .. } => {
            transfer::run_prepare_download(task, max_retries, event_tx)
        }
        task @ Task::RepairFile { .. } => transfer::run_repair_file(task),
        Task::VerifyReuseVolume {
            copy_only,
            candidates,
            logical_path,
            expected_md5,
            expected_size,
            group,
        } => transfer::run_verify_reuse_volume(
            copy_only,
            candidates,
            logical_path,
            expected_md5,
            expected_size,
            group,
        ),
        Task::DiscoverArchiveDirectory {
            work,
            required_range,
        } => archive::run_discover_archive_directory(work, required_range),
        Task::InspectArchiveIndex { work, directory } => {
            archive::run_read_archive_index(work, directory)
        }
        Task::ReadArchiveControls {
            work,
            archive_index,
        } => archive::run_read_archive_controls(work, archive_index, extract_shards, event_tx),
        Task::ProbePatchArtifact {
            patch_check,
            probe_index,
        } => archive::run_probe_patch_artifact(patch_check, probe_index),
        Task::MeasurePatchRelocation { patch_check } => {
            archive::run_measure_patch_relocation(patch_check)
        }
        Task::SavePatchPlan {
            work,
            archive_index,
            patch_check,
        } => archive::run_save_patch_plan(work, archive_index, patch_check, event_tx),
        Task::ExtractArchiveShard { shard } => {
            archive::run_extract_archive_shard(shard, extraction_progress_buffer_bytes, event_tx)
        }
        Task::RetainArchiveVolume { work, volume_index } => {
            archive::run_retain_archive_volume(work, volume_index, event_tx)
        }
        Task::CommitArchive { work } => archive::run_commit_archive(work, event_tx),
        Task::CommitArchiveBatch {
            commit,
            batch_index,
        } => archive::run_commit_archive_batch(commit, batch_index, event_tx),
        Task::FinishArchiveCommit { commit } => archive::run_finish_archive_commit(commit),
        Task::PreparePatchApply { patch } => archive::run_prepare_patch_apply(patch, event_tx),
        Task::ApplyPatchEntry { patch, entry_index } => {
            archive::run_apply_patch_entry(patch, entry_index, event_tx)
        }
        Task::ReleasePatchBase { patch, base } => archive::run_release_patch_base(patch, base),
        Task::ApplyPatchDeletes { patch } => archive::run_apply_patch_deletes(patch, event_tx),
        Task::CommitPatchDeferred { patch } => archive::run_commit_patch_deferred(patch),
        Task::CleanPatchApply {
            patch,
            archive: archive_work,
        } => archive::run_clean_patch_apply(patch, archive_work),
        Task::CleanupArchive { work } => archive::run_clean_archive(work, event_tx),
        Task::ApplyExtractedVfsPatchManifest { install_root } => {
            run_apply_patch_manifest(install_root, event_tx)
        }
        Task::Download {
            resume: Some(_), ..
        }
        | Task::FetchArchiveRange { .. }
        | Task::ReuseFile { .. }
        | Task::ApplyDeleteManifest { .. }
        | Task::Hardlink { .. } => unreachable!("async I/O task routed to blocking runner"),
    }
}

pub(crate) async fn run_async_task(
    task: Task,
    max_retries: u32,
    download_progress_buffer_bytes: usize,
    user_agent: &str,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    match task {
        Task::FetchArchiveRange {
            work,
            request,
            retry_count,
            priority,
        } => {
            archive::run_fetch_archive_range(
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
        task @ Task::Download {
            resume: Some(_), ..
        } => {
            transfer::run_transfer_download(
                task,
                max_retries,
                download_progress_buffer_bytes,
                user_agent,
                event_tx,
            )
            .await
        }
        task @ Task::ReuseFile { copy_only, .. } => {
            if copy_only {
                transfer::run_copy_reuse_file(task, event_tx).await
            } else {
                transfer::run_hardlink_reuse_file(task, event_tx).await
            }
        }
        Task::ApplyDeleteManifest { install_root } => {
            run_apply_delete_manifest(install_root, event_tx).await
        }
        Task::Hardlink { src, dest } => match create_hardlink_async(&src, &dest).await {
            Ok(()) => {
                let _ = event_tx.send(WorkerEvent::hardlinked(dest));
                TaskRun::succeeded()
            }
            Err(error) => TaskRun::failed(error.to_string()),
        },
        _ => unreachable!("blocking task routed to async I/O runner"),
    }
}

fn run_apply_patch_manifest(
    install_root: std::path::PathBuf,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    let plan_path = install_root
        .join(crate::runtime::PATCH_WORK_DIR)
        .join(crate::runtime::PATCH_PLAN_NAME);
    let has_patch_plan = plan_path.is_file();
    let result = if has_patch_plan {
        let mut on_commit = |path: &std::path::Path, finished: usize, total: usize| {
            let normalized = path.to_string_lossy().replace('\\', "/");
            if finished > 0 {
                let _ = event_tx.send(WorkerEvent::changed(normalized.clone()));
            }
            let _ = event_tx.send(WorkerEvent::progress(
                crate::runtime::ProgressPhase::Commit,
                normalized,
                finished as u64,
                total as u64,
                false,
            ));
        };
        let mut on_patch = |path: &str, finished: usize, total: usize| {
            if finished > 0 {
                let _ = event_tx.send(WorkerEvent::changed(path.replace('\\', "/")));
            }
            let _ = event_tx.send(WorkerEvent::progress(
                crate::runtime::ProgressPhase::Patch,
                path.to_string(),
                finished as u64,
                total as u64,
                false,
            ));
        };
        let mut on_delete = |path: &std::path::Path, finished: usize, total: usize| {
            let normalized = path.to_string_lossy().replace('\\', "/");
            if finished > 0 {
                let _ = event_tx.send(WorkerEvent::changed(normalized.clone()));
            }
            let _ = event_tx.send(WorkerEvent::progress(
                crate::runtime::ProgressPhase::Delete,
                normalized,
                finished as u64,
                total as u64,
                false,
            ));
        };
        resume_patch_apply(
            &install_root,
            Some(&mut on_commit),
            Some(&mut on_patch),
            Some(&mut on_delete),
        )
    } else {
        let mut on_progress = |path: &str, finished: usize, total: usize| {
            if finished > 0 {
                let _ = event_tx.send(WorkerEvent::changed(path.replace('\\', "/")));
            }
            let _ = event_tx.send(WorkerEvent::progress(
                crate::runtime::ProgressPhase::Patch,
                path.to_string(),
                finished as u64,
                total as u64,
                false,
            ));
        };
        apply_extracted_vfs_patch_manifest(&install_root, Some(&mut on_progress))
    };

    match result {
        Ok(()) if !has_patch_plan => TaskRun::then(Task::ApplyDeleteManifest { install_root }),
        Ok(()) => TaskRun::succeeded(),
        Err(error) => TaskRun::failed(error.to_string()),
    }
}

async fn run_apply_delete_manifest(
    install_root: std::path::PathBuf,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    let result = apply_delete_files_manifest_async(
        &install_root,
        Some(|path: &std::path::Path, finished: usize, total: usize| {
            let normalized = path.to_string_lossy().replace('\\', "/");
            if finished > 0 {
                let _ = event_tx.send(WorkerEvent::changed(normalized.clone()));
            }
            let _ = event_tx.send(WorkerEvent::progress(
                crate::runtime::ProgressPhase::Delete,
                normalized,
                finished as u64,
                total as u64,
                false,
            ));
        }),
    )
    .await;
    match result {
        Ok(()) => TaskRun::succeeded(),
        Err(error) => TaskRun::failed(error.to_string()),
    }
}
