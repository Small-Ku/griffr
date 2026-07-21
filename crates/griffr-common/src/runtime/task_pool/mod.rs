mod archive_plan;
pub mod download;
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
pub use types::{
    archive_expected_files, ArchivePart, ArchiveRangePriority, ArchiveRetention,
    DownloadResumeState, FileEnsureTask, Task, TaskOutcome, TaskPoolConfig, TaskPoolMetrics,
    TaskPoolResult, TaskPoolRunner, TaskProgress, TransferClass, VolumeIoPolicy,
    VolumeStreamingMode, VolumeTaskMetrics, DEFAULT_PROGRESS_BUFFER_BYTES,
    DEFAULT_REUSE_QUEUE_LIMIT, DEFAULT_VOLUME_METADATA_LIMIT, DEFAULT_VOLUME_READ_LIMIT,
    DEFAULT_VOLUME_STREAMING_MODE, DEFAULT_VOLUME_STREAMING_PRESSURE_LIMIT,
    DEFAULT_VOLUME_WRITE_LIMIT,
};

#[cfg(test)]
#[path = "test/mod.rs"]
mod test;
