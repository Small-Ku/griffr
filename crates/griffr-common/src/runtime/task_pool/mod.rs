mod archive_plan;
mod blocking_buffer;
pub mod download;
pub(crate) mod download_write;
pub(crate) mod fs_ops;
pub mod graph;
mod runner;
pub mod scheduler;
pub mod types;
pub(crate) mod verify;

pub use archive_plan::{plan_archive_groups, ArchiveGroup};
pub use graph::{NodeId, NodeState, TaskGraph, TaskGraphBuilder, TaskGraphSummary};
pub use scheduler::{
    run_task_graph, run_task_graph_with_progress, run_tasks, run_tasks_with_progress,
};
/// Return the stable physical-volume identity used by scheduler admission.
pub fn storage_volume_key(path: impl AsRef<std::path::Path>) -> String {
    fs_ops::storage_volume_group_key(path.as_ref())
}
pub use types::{
    archive_expected_files, ArchivePart, ArchiveRangePriority, ArchiveRetention, ArchiveSource,
    DownloadResumeState, FileEnsureTask, FinalFileRef, Task, TaskOutcome, TaskPoolConfig,
    TaskPoolMetrics, TaskPoolResult, TaskPoolRunner, TaskProgress, TransferClass, VolumeIoPolicy,
    VolumeStreamingMode, VolumeTaskMetrics, DEFAULT_PROGRESS_BUFFER_BYTES,
    DEFAULT_REUSE_QUEUE_LIMIT, DEFAULT_VOLUME_METADATA_LIMIT, DEFAULT_VOLUME_READ_LIMIT,
    DEFAULT_VOLUME_STREAMING_MODE, DEFAULT_VOLUME_STREAMING_PRESSURE_LIMIT,
    DEFAULT_VOLUME_WRITE_LIMIT,
};

#[cfg(test)]
#[path = "test/mod.rs"]
mod test;
