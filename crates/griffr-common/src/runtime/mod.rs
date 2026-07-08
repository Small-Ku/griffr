pub mod admin;
mod compat_fs;
pub mod files;
pub mod issues;
pub mod launcher;
pub mod manager;
mod paths;
mod progress;
pub mod server;
pub mod task_pool;

pub use admin::{ensure_admin, is_running_as_admin, restart_as_admin};
pub use compat_fs::{
    collect_files_recursive, copy_dir_recursive, dir_size, directory_has_entries,
    list_files_with_extension, read_link, remove_dir_all, remove_empty_dirs_recursive, CopyStats,
};
pub use files::reuse::{
    apply_file_reuse_flow, derive_files_base_url, materialize_game_files_with_pool,
    FileReuseConfig, MaterializeSummary, SourceInstallInput,
};
pub use files::vfs::{
    bootstrap_persistent_vfs_with_runner, download_vfs_resources, get_vfs_resource_info,
    plan_persistent_bootstrap_tasks, plan_vfs_tasks, VfsBootstrapConfig, VfsBootstrapPlan,
    VfsBootstrapResult, VfsBootstrapScope, VfsMaterializeConfig, VfsTaskPlan, VfsUpdateResult,
};
pub use issues::{FileIssue, FileIssueKind};
pub use launcher::{GameProcess, Launcher};
pub use manager::GameManager;
pub use paths::{is_launcher_metadata_path, logical_path_from_root, normalize_logical_path};
pub use progress::{
    PathAttemptKind, PathOutcome, PathOutcomeSummary, PathOutcomeTracker, PathReuseMethod,
    RunningByteProgress,
};
pub use server::Server;
