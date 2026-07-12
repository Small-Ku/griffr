mod archive_plan;
pub mod download;
mod executor;
pub mod fs_ops;
pub mod scheduler;
pub mod types;
pub mod verify;

pub use archive_plan::{plan_archive_groups, ArchiveGroup};
pub use scheduler::{run_tasks, run_tasks_with_progress};
pub use types::{
    ArchivePart, ProgressEvent, Task, TaskPoolConfig, TaskPoolResult, TaskPoolRunner,
    DEFAULT_PROGRESS_BUFFER_BYTES,
};

#[cfg(test)]
#[path = "test/mod.rs"]
mod test;
