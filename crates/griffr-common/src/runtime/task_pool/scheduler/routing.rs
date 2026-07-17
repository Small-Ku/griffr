use super::WorkerKind;
use crate::runtime::task_pool::{Task, TaskPoolConfig, TransferClass};

pub(super) fn dispatcher_thread_count(config: &TaskPoolConfig) -> usize {
    let worker_loops =
        config.io_slots + config.vfs_io_slots + config.cpu_slots + config.extract_slots;
    let extra_io_lanes = (config.io_slots + config.vfs_io_slots).max(1);
    (worker_loops + extra_io_lanes).clamp(2, 64)
}

pub(super) fn worker_kind_for_task(task: &Task) -> WorkerKind {
    match task {
        Task::Download {
            transfer_class: TransferClass::Vfs,
            ..
        } => WorkerKind::VfsIo,
        Task::InstallArchive { .. }
        | Task::Download { .. }
        | Task::ReuseFile { .. }
        | Task::Hardlink { .. }
        | Task::ApplyExtractedVfsPatchManifest { .. }
        | Task::ApplyDeleteManifest { .. } => WorkerKind::Io,
        Task::Verify { .. } | Task::RepairFile { .. } => WorkerKind::Cpu,
        Task::Extract { .. } => WorkerKind::Extract,
    }
}

#[cfg(test)]
mod tests {
    use super::worker_kind_for_task;
    use crate::runtime::task_pool::scheduler::WorkerKind;
    use crate::runtime::task_pool::{Task, TransferClass};
    use std::path::PathBuf;

    fn download(transfer_class: TransferClass) -> Task {
        Task::Download {
            url: "https://example.invalid/file.bin".to_string(),
            dest: PathBuf::from("file.bin"),
            logical_path: "file.bin".to_string(),
            expected_md5: "00".repeat(16),
            expected_size: Some(4),
            retry_count: 0,
            transfer_class,
        }
    }

    #[test]
    fn only_vfs_downloads_use_the_limited_vfs_queue() {
        assert_eq!(
            worker_kind_for_task(&download(TransferClass::Vfs)),
            WorkerKind::VfsIo
        );
        assert_eq!(
            worker_kind_for_task(&download(TransferClass::General)),
            WorkerKind::Io
        );

        let reuse = Task::ReuseFile {
            source: PathBuf::from("source.bin"),
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
        assert_eq!(worker_kind_for_task(&verify), WorkerKind::Cpu);
        assert_eq!(worker_kind_for_task(&repair), WorkerKind::Cpu);
    }
}
