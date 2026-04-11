//! Parallel download with resume support

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use md5::{Digest, Md5};
use tokio::io::AsyncWriteExt;

use crate::api::types::PackFile;

/// Progress callback trait for download updates
pub trait ProgressCallback: Send + Sync {
    /// Called when a download starts
    fn on_start(&self, filename: &str, total_bytes: u64);

    /// Called with progress updates
    fn on_progress(&self, filename: &str, downloaded_bytes: u64, total_bytes: u64);

    /// Called when a download completes
    fn on_complete(&self, filename: &str, success: bool);
}

/// Simple progress callback that prints to stdout
pub struct ConsoleProgress;

impl ProgressCallback for ConsoleProgress {
    fn on_start(&self, filename: &str, total_bytes: u64) {
        let mb = total_bytes as f64 / 1024.0 / 1024.0;
        println!("Downloading {} ({:.1} MB)...", filename, mb);
    }

    fn on_progress(&self, _filename: &str, _downloaded_bytes: u64, _total_bytes: u64) {
        // No-op for console to avoid spam
    }

    fn on_complete(&self, filename: &str, success: bool) {
        if success {
            println!("Downloaded {}", filename);
        } else {
            println!("Failed to download {}", filename);
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
    client: reqwest::Client,
    options: DownloadOptions,
}

impl Downloader {
    /// Create a new downloader with default options
    pub fn new() -> Result<Self> {
        Self::with_options(DownloadOptions::default())
    }

    /// Create a new downloader with custom options
    pub fn with_options(options: DownloadOptions) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self { client, options })
    }

    pub async fn download_pack(
        &self,
        pack: &PackFile,
        output_dir: &Path,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<PathBuf> {
        let filename = pack
            .filename()
            .context("Failed to extract filename from URL")?;

        // Remove query parameters from filename
        let filename = filename.split('?').next().unwrap_or(filename);
        let output_path = output_dir.join(filename);

        if let Some(p) = progress {
            p.on_start(filename, pack.size());
        }

        let result = self
            .download_with_retry(&pack.url, &output_path, pack.size(), progress)
            .await;

        // Verify MD5 if requested and download succeeded
        if result.is_ok() && self.options.verify_md5 {
            if let Err(e) = self.verify_file_md5(&output_path, &pack.md5).await {
                if let Some(p) = progress {
                    p.on_complete(filename, false);
                }
                return Err(e);
            }
        }

        if let Some(p) = progress {
            p.on_complete(filename, result.is_ok());
        }

        result.map(|_| output_path)
    }

    /// Download multiple pack files in parallel
    pub async fn download_packs(
        &self,
        packs: &[PackFile],
        output_dir: &Path,
        progress: Option<std::sync::Arc<dyn ProgressCallback>>,
    ) -> Result<Vec<PathBuf>> {
        use futures_util::stream::{self, StreamExt};

        let concurrent = self.options.concurrent_connections as usize;

        let results = stream::iter(packs)
            .map(|pack| {
                let downloader = self.clone();
                let output_dir = output_dir.to_path_buf();
                let progress = progress.clone();
                async move {
                    downloader
                        .download_pack(pack, &output_dir, progress.as_deref())
                        .await
                }
            })
            .buffer_unordered(concurrent)
            .collect::<Vec<_>>()
            .await;

        let mut paths = Vec::with_capacity(packs.len());
        for res in results {
            paths.push(res?);
        }

        Ok(paths)
    }

    /// Download a file with retry logic
    async fn download_with_retry(
        &self,
        url: &str,
        output_path: &Path,
        expected_size: u64,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<()> {
        let mut last_error = None;

        for attempt in 0..self.options.retry_attempts {
            if attempt > 0 {
                let delay = std::time::Duration::from_secs(2u64.pow(attempt));
                tokio::time::sleep(delay).await;
            }

            match self
                .download_file(url, output_path, expected_size, progress)
                .await
            {
                Ok(()) => return Ok(()),
                Err(e) => {
                    tracing::warn!("Download attempt {} failed: {}", attempt + 1, e);
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("All download attempts failed")))
    }

    /// Download a single file with optional resume
    async fn download_file(
        &self,
        url: &str,
        output_path: &Path,
        expected_size: u64,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<()> {
        // Create output directory
        if let Some(parent) = output_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Determine start byte for resume
        let start_byte = if self.options.resume && output_path.exists() {
            let metadata = tokio::fs::metadata(output_path).await?;
            let size = metadata.len();
            if size >= expected_size {
                // File already complete
                return Ok(());
            }
            Some(size)
        } else {
            None
        };

        // Build request
        let mut request = self.client.get(url);
        if let Some(start) = start_byte {
            request = request.header(reqwest::header::RANGE, format!("bytes={}-", start));
        }

        let response = request
            .send()
            .await
            .with_context(|| format!("Failed to send request to {}", url))?;

        let status = response.status();
        if !status.is_success() && status != reqwest::StatusCode::PARTIAL_CONTENT {
            anyhow::bail!("HTTP error: {}", status);
        }

        // Open file for writing
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .append(start_byte.is_some())
            .truncate(start_byte.is_none())
            .open(output_path)
            .await
            .with_context(|| format!("Failed to open file {}", output_path.display()))?;

        // Stream response body to file
        let mut stream = response.bytes_stream();
        use futures_util::StreamExt;

        let mut downloaded = start_byte.unwrap_or(0);
        let filename = output_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("Failed to read response chunk")?;
            let chunk_len = chunk.len() as u64;
            file.write_all(&chunk).await?;

            downloaded += chunk_len;
            if let Some(p) = progress {
                p.on_progress(filename, downloaded, expected_size);
            }
        }

        file.flush().await?;

        Ok(())
    }

    /// Verify file MD5
    async fn verify_file_md5(&self, path: &Path, expected_md5: &str) -> Result<()> {
        use tokio::io::AsyncReadExt;

        let mut file = tokio::fs::File::open(path).await.with_context(|| {
            format!(
                "Failed to open file for MD5 verification: {}",
                path.display()
            )
        })?;

        let mut hasher = Md5::new();
        let mut buffer = vec![0u8; 8192];

        loop {
            let n = file.read(&mut buffer).await?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }

        let result = hasher.finalize();
        let actual_md5 = format!("{:x}", result);

        if actual_md5 != expected_md5.to_lowercase() {
            anyhow::bail!(
                "MD5 mismatch: expected {}, got {}",
                expected_md5,
                actual_md5
            );
        }

        Ok(())
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
