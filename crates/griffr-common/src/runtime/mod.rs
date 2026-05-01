pub mod admin;
pub mod files;
pub mod issues;
pub mod launcher;
pub mod manager;
pub mod server;
pub mod task_pool;

pub use admin::{ensure_admin, is_running_as_admin, restart_as_admin};
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
pub use server::Server;
