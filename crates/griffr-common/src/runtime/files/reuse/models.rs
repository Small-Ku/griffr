//! Server game files reuse via hardlinks
//!
//! This module provides functionality to reuse game files across different server
//! installations by creating hardlinks for identical files (same relative path + MD5),
//! then downloading only the remaining files that couldn't be reused.

use std::path::PathBuf;

use crate::config::ServerId;
use crate::runtime::issues::FileIssue;

/// Information about a source server that can provide files for reuse
#[derive(Debug, Clone)]
pub struct SourceServer {
    /// Server ID of the source
    pub server_id: ServerId,
    /// Version of the source installation
    pub version: String,
    /// Install path of the source
    pub install_path: PathBuf,
    /// Number of reusable files from this source
    pub file_count: usize,
}

/// Explicit source installation input for reuse planning.
#[derive(Debug, Clone)]
pub struct SourceInstallInput {
    /// Server ID declared by the source installation metadata.
    pub server_id: ServerId,
    /// Installed version string of the source installation.
    pub version: String,
    /// Installation path used as the file source.
    pub install_path: PathBuf,
}

/// A file that can be reused from a source server
#[derive(Debug, Clone)]
pub struct ReusableFile {
    /// Relative path of the file
    pub path: String,
    /// MD5 hash of the file
    pub md5: String,
    /// File size in bytes
    pub size: u64,
    /// Source server providing this file
    pub source_server_id: ServerId,
    /// Source install path
    pub source_path: PathBuf,
}

/// A file that needs to be downloaded
#[derive(Debug, Clone)]
pub struct DownloadFile {
    /// Relative path of the file
    pub path: String,
    /// MD5 hash of the file
    pub md5: String,
    /// File size in bytes
    pub size: u64,
}

/// Plan for reusing files from other installed servers
#[derive(Debug, Clone)]
pub struct ReusePlan {
    /// Source servers that can provide files
    pub source_servers: Vec<SourceServer>,
    /// Files that can be reused
    pub reusable_files: Vec<ReusableFile>,
    /// Files that need to be downloaded
    pub download_files: Vec<DownloadFile>,
    /// Total size of reusable files in bytes
    pub reusable_size: u64,
    /// Total size of files to download in bytes
    pub download_size: u64,
    /// Whether copy fallback is required (files on different volumes)
    pub requires_copy_fallback: bool,
}

/// Configuration for the file reuse flow
#[derive(Debug, Clone)]
pub struct FileReuseConfig {
    /// Allow copying files when hardlink creation fails
    pub allow_copy_fallback: bool,
    /// Perform a dry run without making changes
    pub dry_run: bool,
    /// Explicit source installs to consider for reuse (may include same server as target)
    pub source_installs: Vec<SourceInstallInput>,
}

/// Options for executing a reuse plan
#[derive(Debug, Clone)]
pub struct ReuseOptions {
    /// Allow copying files when hardlink creation fails
    pub allow_copy_fallback: bool,
    /// Perform a dry run without making changes
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default)]
pub struct MaterializeSummary {
    pub reused_files: usize,
    pub downloaded_files: usize,
    pub issues: Vec<FileIssue>,
}

#[cfg(test)]
#[path = "test.rs"]
mod test;
