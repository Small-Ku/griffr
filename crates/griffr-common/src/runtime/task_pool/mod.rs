pub mod download;
mod executor;
pub mod fs_ops;
pub mod scheduler;
pub mod types;
pub mod verify;

pub use scheduler::{extract_archives_pooled, run_tasks, run_tasks_with_progress};
pub use types::{ArchivePart, ProgressEvent, Task, TaskPoolConfig, TaskPoolResult, TaskPoolRunner};

#[cfg(test)]
#[path = "test.rs"]
mod test;
