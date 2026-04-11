//! Game management module

pub mod admin;
pub mod files_reuse;
pub mod launcher;
pub mod manager;
pub mod server;
pub mod vfs;

pub use admin::{ensure_admin, is_running_as_admin, restart_as_admin};
pub use files_reuse::{
    apply_file_reuse_flow, derive_files_base_url, download_remaining_files, execute_reuse_plan,
    plan_file_reuse, print_reuse_plan_summary, FileReuseConfig, ReuseOptions, ReusePlan,
    SourceInstallInput,
};
pub use launcher::{GameProcess, Launcher};
pub use manager::GameManager;
pub use server::Server;
pub use vfs::{download_vfs_resources, get_vfs_resource_info, VfsUpdateResult};

use serde::{Deserialize, Serialize};

/// Type of issue found with a game file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileIssueKind {
    /// File is missing
    Missing,
    /// File size mismatch
    SizeMismatch,
    /// File MD5 mismatch
    Md5Mismatch,
}

/// Information about a problematic game file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileIssue {
    /// Relative path from game root
    pub path: String,
    /// Expected MD5 hash
    pub expected_md5: String,
    /// Expected size in bytes
    pub expected_size: u64,
    /// Actual size in bytes (if file exists)
    pub actual_size: Option<u64>,
    /// Actual MD5 hash (if file exists)
    pub actual_md5: Option<String>,
    /// Type of issue
    pub kind: FileIssueKind,
}
