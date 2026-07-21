use std::path::PathBuf;

use crate::config::{ChannelPair, RegionId};
use crate::runtime::issues::FileIssue;

/// Explicit source installation input for file reuse.
#[derive(Debug, Clone)]
pub struct SourceInstallInput {
    /// Region declared by the source installation metadata.
    pub region_id: RegionId,
    /// Channel values declared by the source installation metadata.
    pub channel_id: ChannelPair,
    /// Installed version string of the source installation.
    pub version: String,
    /// Installation path used as the file source.
    pub install_path: PathBuf,
}

/// Configuration for the manifest-driven game-file ensure work.
#[derive(Debug, Clone)]
pub struct FileReuseConfig {
    /// Allow copying files when hardlink creation fails.
    pub allow_copy_fallback: bool,
    /// Plan without changing files.
    pub dry_run: bool,
    /// Explicit source installs to consider for reuse.
    pub source_installs: Vec<SourceInstallInput>,
}

#[derive(Debug, Clone, Default)]
pub struct FileEnsureSummary {
    pub reused_files: usize,
    pub downloaded_files: usize,
    pub issues: Vec<FileIssue>,
}
