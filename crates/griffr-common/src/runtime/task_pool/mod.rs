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
    TaskPoolMetrics, TaskPoolResult, TaskPoolRunner, TaskProgress, TransferClass,
    VolumeConcurrency, VolumeTaskMetrics, DEFAULT_PROGRESS_BUFFER_BYTES,
};

#[cfg(test)]
#[path = "test/mod.rs"]
mod test;
