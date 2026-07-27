use std::path::PathBuf;

use crate::runtime::issues::FileIssue;

/// Configuration for manifest-driven game-file ensure work.
#[derive(Debug, Clone)]
pub struct FileReuseConfig {
    /// Allow copying files when hardlink creation fails or crosses volumes.
    pub allow_copy_fallback: bool,
    /// Plan without changing files.
    pub dry_run: bool,
    /// Compatible install roots to probe for files that match the target manifest.
    pub source_roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct FileEnsureSummary {
    pub reused_files: usize,
    pub downloaded_files: usize,
    pub issues: Vec<FileIssue>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ObsoleteFileCleanupSummary {
    pub removed_files: usize,
    pub retained_modified_files: usize,
}
