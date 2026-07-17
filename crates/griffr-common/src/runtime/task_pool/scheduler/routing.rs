use super::WorkerKind;
use crate::runtime::task_pool::{Task, TaskPoolConfig, TransferClass};

pub(super) fn dispatcher_thread_count(config: &TaskPoolConfig) -> usize {
    config.dispatcher_threads.clamp(2, 4)
}

pub(super) fn worker_kind_for_task(task: &Task) -> WorkerKind {
    match task {
        Task::TransferDownload {
            transfer_class: TransferClass::Vfs,
            ..
        } => WorkerKind::VfsIo,
        Task::TransferArchivePart { .. } => WorkerKind::ArchiveIo,
        Task::InstallArchive { .. }
        | Task::ReuseFile { .. }
        | Task::Hardlink { .. }
        | Task::ApplyExtractedVfsPatchManifest { .. }
        | Task::ApplyDeleteManifest { .. }
        | Task::TransferDownload { .. } => WorkerKind::Io,
        Task::InstallArchivePart { .. }
        | Task::Download { .. }
        | Task::Verify { .. }
        | Task::RepairFile { .. }
        | Task::VerifyReuseVolume { .. } => WorkerKind::Cpu,
        Task::Extract { .. } => WorkerKind::Extract,
    }
}

pub(super) fn task_path(task: &Task) -> String {
    match task {
        Task::InstallArchive { base_name, .. } | Task::Extract { base_name, .. } => {
            base_name.clone()
        }
        Task::InstallArchivePart { part, .. } | Task::TransferArchivePart { part, .. } => {
            part.logical_path.clone()
        }
        Task::Download { logical_path, .. }
        | Task::TransferDownload { logical_path, .. }
        | Task::Verify { logical_path, .. }
        | Task::RepairFile { logical_path, .. }
        | Task::VerifyReuseVolume { logical_path, .. }
        | Task::ReuseFile { logical_path, .. } => logical_path.clone(),
        Task::ApplyExtractedVfsPatchManifest { install_root }
        | Task::ApplyDeleteManifest { install_root } => install_root.display().to_string(),
        Task::Hardlink { dest, .. } => dest.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::worker_kind_for_task;
    use crate::runtime::task_pool::scheduler::WorkerKind;
    use crate::runtime::task_pool::{DownloadResumeState, Task, TransferClass};
    use md5::{Digest, Md5};
    use std::path::PathBuf;

    fn transfer_download(transfer_class: TransferClass) -> Task {
        Task::TransferDownload {
            url: "https://example.invalid/file.bin".to_string(),
            dest: PathBuf::from("file.bin"),
            logical_path: "file.bin".to_string(),
            expected_md5: "00".repeat(16),
            expected_size: Some(4),
            retry_count: 0,
            transfer_class,
            resume: DownloadResumeState::new(0, Md5::new()),
        }
    }

    #[test]
    fn only_vfs_downloads_use_the_limited_vfs_queue() {
        assert_eq!(
            worker_kind_for_task(&transfer_download(TransferClass::Vfs)),
            WorkerKind::VfsIo
        );
        assert_eq!(
            worker_kind_for_task(&transfer_download(TransferClass::General)),
            WorkerKind::Io
        );

        let reuse = Task::ReuseFile {
            source: PathBuf::from("source.bin"),
            copy_only: false,
            remaining_source_candidates: Vec::new(),
            dest: PathBuf::from("dest.bin"),
            logical_path: "dest.bin".to_string(),
            expected_md5: "00".repeat(16),
            expected_size: 4,
            download_url: None,
            allow_copy_fallback: false,
            retry_count: 0,
            transfer_class: TransferClass::Vfs,
        };
        assert_eq!(worker_kind_for_task(&reuse), WorkerKind::Io);
    }

    #[test]
    fn verification_and_source_validation_use_cpu_workers() {
        let prepare = Task::Download {
            url: "https://example.invalid/file.bin".to_string(),
            dest: PathBuf::from("file.bin"),
            logical_path: "file.bin".to_string(),
            expected_md5: "00".repeat(16),
            expected_size: Some(4),
            retry_count: 0,
            transfer_class: TransferClass::General,
        };
        let verify = Task::Verify {
            path: PathBuf::from("file.bin"),
            logical_path: "file.bin".to_string(),
            expected_md5: "00".repeat(16),
            expected_size: Some(4),
            on_fail: None,
        };
        let repair = Task::RepairFile {
            dest: PathBuf::from("file.bin"),
            logical_path: "file.bin".to_string(),
            expected_md5: "00".repeat(16),
            expected_size: 4,
            source_candidates: Vec::new(),
            download_url: None,
            allow_copy_fallback: false,
            retry_count: 0,
            transfer_class: TransferClass::General,
        };
        assert_eq!(worker_kind_for_task(&prepare), WorkerKind::Cpu);
        assert_eq!(worker_kind_for_task(&verify), WorkerKind::Cpu);
        assert_eq!(worker_kind_for_task(&repair), WorkerKind::Cpu);
    }
}
