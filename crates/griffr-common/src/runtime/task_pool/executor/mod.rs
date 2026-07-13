use compio::dispatcher::Dispatcher;

use super::fs_ops::{
    apply_delete_files_manifest, apply_extracted_vfs_patch_manifest, create_hardlink,
};
use super::types::{Task, WorkerEvent};

mod archive;
mod transfer;

pub(crate) fn execute_task(
    task: Task,
    max_retries: u32,
    extraction_progress_buffer_bytes: usize,
    download_progress_buffer_bytes: usize,
    io_dispatcher: Option<&Dispatcher>,
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
            parts,
        } => archive::execute_install_archive(
            base_name,
            dest,
            cleanup,
            password,
            parts,
            max_retries,
            download_progress_buffer_bytes,
            io_dispatcher,
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
        } => transfer::execute_download(
            transfer::DownloadExecInput {
                url,
                dest,
                logical_path,
                expected_md5,
                expected_size,
                retry_count,
                max_retries,
            },
            download_progress_buffer_bytes,
            io_dispatcher,
            user_agent,
            spawned,
            event_tx,
        ),
        Task::EnsureFile {
            dest,
            logical_path,
            expected_md5,
            expected_size,
            source_candidates,
            download_url,
            allow_copy_fallback,
            prefer_reuse,
            retry_count,
        } => transfer::execute_ensure_file(
            transfer::EnsureFileInput {
                dest,
                logical_path,
                expected_md5,
                expected_size,
                source_candidates,
                download_url,
                allow_copy_fallback,
                prefer_reuse,
                retry_count,
                max_retries,
            },
            download_progress_buffer_bytes,
            io_dispatcher,
            user_agent,
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
        } => archive::execute_extract_archive(
            base_name,
            volumes,
            dest,
            cleanup,
            password,
            extraction_progress_buffer_bytes,
            spawned,
            event_tx,
        ),
        Task::ApplyExtractedVfsPatchManifest { install_root } => {
            let result = {
                let mut on_progress = |path: &str, completed: usize, total: usize| {
                    let _ = event_tx.send(WorkerEvent::PatchProgress {
                        path: path.to_string(),
                        completed,
                        total,
                    });
                };
                apply_extracted_vfs_patch_manifest(&install_root, Some(&mut on_progress))
            };
            match result {
                Ok(()) => {
                    spawned.push(Task::ApplyDeleteManifest { install_root });
                }
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
                    let _ = event_tx.send(WorkerEvent::DeleteProgress {
                        path: path.to_string_lossy().replace('\\', "/"),
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
