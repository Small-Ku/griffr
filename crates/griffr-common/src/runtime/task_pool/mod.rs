pub mod download;
mod executor;
pub mod fs_ops;
pub mod scheduler;
pub mod types;
pub mod verify;

pub use scheduler::{extract_archives_pooled, run_tasks, run_tasks_with_progress};
pub use types::{
    ArchivePart, ProgressEvent, Task, TaskPoolConfig, TaskPoolResult, TaskPoolRunner,
    DEFAULT_PROGRESS_BUFFER_BYTES,
};

#[cfg(test)]
#[path = "test/mod.rs"]
mod test;
