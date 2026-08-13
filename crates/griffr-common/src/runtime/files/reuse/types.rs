use std::path::PathBuf;

use crate::api::types::PackFile;
use crate::runtime::issues::FileIssue;
use crate::runtime::ArtifactProof;

/// Configuration for manifest-driven game-file ensure work.
#[derive(Debug, Clone)]
pub struct FileMaterializationConfig {
    /// Allow copying files when hardlink creation fails or crosses volumes.
    pub allow_copy_fallback: bool,
    /// Plan without changing files.
    pub dry_run: bool,
    /// Compatible install roots to probe for files that match the target manifest.
    pub source_roots: Vec<PathBuf>,
    /// Full-package archives available as a compressed-range materialization provider.
    pub archive_packs: Vec<PackFile>,
    /// Materialize immediately instead of verifying a destination known to be empty.
    pub skip_destination_check: bool,
}

#[derive(Debug, Clone, Default)]
pub struct FileEnsureSummary {
    pub reused_files: usize,
    pub downloaded_files: usize,
    pub issues: Vec<FileIssue>,
    pub verified_artifacts: Vec<ArtifactProof>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ObsoleteFileCleanupSummary {
    pub removed_files: usize,
    pub retained_modified_files: usize,
}
