mod archive_plan;
pub mod download;
mod executor;
pub(crate) mod fs_ops;
pub mod scheduler;
pub mod types;
pub(crate) mod verify;

pub use archive_plan::{plan_archive_groups, ArchiveGroup};
pub use scheduler::{run_tasks, run_tasks_with_progress};
pub use types::{
    ArchivePart, DownloadResumeState, FileEnsureTask, Task, TaskOutcome, TaskPoolConfig,
    TaskPoolMetrics, TaskPoolResult, TaskPoolRunner, TaskProgress, TransferClass, VolumeIoPolicy,
    VolumeStreamingMode, VolumeTaskMetrics, DEFAULT_PROGRESS_BUFFER_BYTES,
    DEFAULT_REUSE_PIPELINE_WINDOW, DEFAULT_VOLUME_METADATA_LIMIT, DEFAULT_VOLUME_READ_LIMIT,
    DEFAULT_VOLUME_STREAMING_MODE, DEFAULT_VOLUME_STREAMING_PRESSURE_LIMIT,
    DEFAULT_VOLUME_WRITE_LIMIT,
};

#[cfg(test)]
#[path = "test/mod.rs"]
mod test;
