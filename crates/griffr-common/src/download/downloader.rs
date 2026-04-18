//! Parallel download with resume support

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::api::types::PackFile;
use crate::game::task_pool::{run_tasks, ProgressEvent, Task, TaskPoolConfig};

/// Progress callback trait for download updates
pub trait ProgressCallback: Send + Sync {
    /// Called when a download starts
    fn on_start(&self, filename: &str, total_bytes: u64);

    /// Called with progress updates
    fn on_progress(&self, filename: &str, downloaded_bytes: u64, total_bytes: u64);

    /// Called when a download completes
    fn on_complete(&self, filename: &str, success: bool);
}

/// Simple progress callback that emits tracing events
pub struct ConsoleProgress;

impl ProgressCallback for ConsoleProgress {
    fn on_start(&self, filename: &str, total_bytes: u64) {
        let mb = total_bytes as f64 / 1024.0 / 1024.0;
        info!("Downloading {} ({:.1} MB)...", filename, mb);
    }

    fn on_progress(&self, _filename: &str, _downloaded_bytes: u64, _total_bytes: u64) {
        // No-op for console to avoid spam
    }

    fn on_complete(&self, filename: &str, success: bool) {
        if success {
            info!("Downloaded {}", filename);
        } else {
            warn!("Failed to download {}", filename);
        }
    }
}

/// Download options
#[derive(Debug, Clone)]
pub struct DownloadOptions {
    /// Number of concurrent connections per file
    pub concurrent_connections: u32,

    /// Number of retry attempts
    pub retry_attempts: u32,

    /// Whether to resume partial downloads
    pub resume: bool,

    /// Verify MD5 after download
    pub verify_md5: bool,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            concurrent_connections: 4,
            retry_attempts: 3,
            resume: true,
            verify_md5: true,
        }
    }
}

/// Download manager
#[derive(Debug, Clone)]
pub struct Downloader {
    options: DownloadOptions,
}

impl Downloader {
    /// Create a new downloader with default options
    pub fn new() -> Result<Self> {
        Self::with_options(DownloadOptions::default())
    }

    /// Create a new downloader with custom options
    pub fn with_options(options: DownloadOptions) -> Result<Self> {
        Ok(Self { options })
    }

    /// Download multiple pack files in parallel
    pub async fn download_packs(
        &self,
        packs: &[PackFile],
        output_dir: &Path,
        progress: Option<std::sync::Arc<dyn ProgressCallback>>,
    ) -> Result<Vec<PathBuf>> {
        if packs.is_empty() {
            return Ok(Vec::new());
        }

        compio::fs::create_dir_all(output_dir)
            .await
            .with_context(|| format!("Failed to create {}", output_dir.display()))?;

        let mut expected = Vec::with_capacity(packs.len());
        let mut tasks = Vec::with_capacity(packs.len());
        for pack in packs {
            let filename = pack
                .filename()
                .context("Failed to extract filename from URL")?
                .split('?')
                .next()
                .unwrap_or_default()
                .to_string();
            let output_path = output_dir.join(&filename);
            expected.push((filename.clone(), output_path.clone(), pack.size()));
            tasks.push(Task::Download {
                url: pack.url.clone(),
                dest: output_path,
                logical_path: filename,
                expected_md5: pack.md5.clone(),
                expected_size: Some(pack.size()),
                retry_count: 0,
            });
        }

        if let Some(p) = progress.as_deref() {
            for (filename, _, size) in &expected {
                p.on_start(filename, *size);
            }
        }

        let mut config = TaskPoolConfig::default();
        config.io_slots = self.options.concurrent_connections.max(1) as usize;
        config.max_retries = self.options.retry_attempts;

        let result = run_tasks(tasks, config)?;
        let mut failed = Vec::new();
        for event in result.events {
            match event {
                ProgressEvent::Downloaded { path, bytes } => {
                    if let Some(p) = progress.as_deref() {
                        p.on_progress(&path, bytes, bytes);
                    }
                }
                ProgressEvent::Verified { path, ok, .. } => {
                    if !ok {
                        failed.push(path.clone());
                    }
                    if let Some(p) = progress.as_deref() {
                        p.on_complete(&path, ok);
                    }
                }
                ProgressEvent::Failed { path, .. } => failed.push(path),
                _ => {}
            }
        }

        if !failed.is_empty() {
            anyhow::bail!(
                "Failed to download {} pack(s): {}",
                failed.len(),
                failed.join(", ")
            );
        }

        let mut paths = Vec::with_capacity(expected.len());
        for (_, path, _) in expected {
            match compio::fs::metadata(&path).await {
                Ok(_) => {}
                Err(err) if err.kind() == ErrorKind::NotFound => {
                    anyhow::bail!("Missing downloaded pack: {}", path.display());
                }
                Err(err) => {
                    return Err(err).with_context(|| format!("Failed to stat {}", path.display()))
                }
            }
            paths.push(path);
        }
        Ok(paths)
    }
}

impl Default for Downloader {
    fn default() -> Self {
        Self::new().expect("Failed to create default downloader")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_download_options_default() {
        let opts = DownloadOptions::default();
        assert_eq!(opts.concurrent_connections, 4);
        assert_eq!(opts.retry_attempts, 3);
        assert!(opts.resume);
        assert!(opts.verify_md5);
    }
}
