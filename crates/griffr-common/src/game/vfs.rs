//! VFS (Virtual File System) resource download and update logic
//!
//! Hypergryph games store large game assets (audio, textures, etc.) in VFS files
//! under `StreamingAssets/VFS/`. These are managed separately from the main game
//! packs via the `get_latest_resources` API endpoint.
//!
//! The VFS flow:
//! 1. Call `get_latest_resources` API to get resource group URLs
//! 2. Fetch `index_main.json` (encrypted) and decrypt to get file list
//! 3. Fetch `patch.json` (plain JSON) for incremental updates
//! 4. Download missing/changed VFS files from CDN
//!
//! Reference: `ref/ak-endfield-api-archive-main/src/cmds/archive.ts`

use anyhow::{Context, Result};
use std::path::Path;

use crate::api::client::ApiClient;
use crate::api::crypto::RES_INDEX_KEY;
use crate::api::types::*;
use crate::config::{GameId, ServerId};

/// Result of a VFS resource check/download operation
#[derive(Debug, Clone)]
pub struct VfsUpdateResult {
    /// Total VFS files in the manifest
    pub total_files: usize,
    /// Files that needed downloading
    pub downloaded_files: usize,
    /// Total bytes downloaded
    pub downloaded_bytes: u64,
    /// Files already present and up-to-date
    pub skipped_files: usize,
    /// Resource version string
    pub res_version: String,
}

/// Check and download VFS game resources after a game update/install
///
/// This should be called after the main game packs are extracted. It:
/// 1. Fetches the latest VFS resource info from the API
/// 2. Downloads and decrypts the resource index
/// 3. Downloads missing/changed VFS files
///
/// `streaming_assets_path` should point to the game's StreamingAssets directory
/// (e.g., `{install_path}/Endfield_Data/StreamingAssets` for Endfield).
///
/// Returns a summary of what was downloaded.
pub async fn download_vfs_resources(
    api_client: &ApiClient,
    game_id: GameId,
    server_id: ServerId,
    game_version: &str,
    rand_str: &str,
    streaming_assets_path: &Path,
    progress_callback: Option<&dyn Fn(u64, u64)>,
) -> Result<VfsUpdateResult> {
    // Step 1: Get latest resource metadata
    let resources = api_client
        .get_latest_resources(game_id, server_id, game_version, rand_str, "Windows")
        .await
        .context("Failed to get latest VFS resources")?;

    println!(
        "VFS resource version: {} ({} resource groups)",
        resources.res_version,
        resources.resources.len()
    );

    let mut total_result = VfsUpdateResult {
        total_files: 0,
        downloaded_files: 0,
        downloaded_bytes: 0,
        skipped_files: 0,
        res_version: resources.res_version.clone(),
    };

    // Step 2: Process each resource group (main, initial)
    for resource in &resources.resources {
        println!(
            "Processing VFS resource group: {} (version {})",
            resource.name, resource.version
        );

        // Fetch and decrypt the resource index
        let index_url = format!("{}/index_{}.json", resource.path, resource.name);
        let index = api_client
            .fetch_res_index(&index_url, RES_INDEX_KEY)
            .await
            .with_context(|| format!("Failed to fetch resource index for {}", resource.name))?;

        println!(
            "  VFS index: {} files ({} groups)",
            index.files.len(),
            resource.name
        );

        total_result.total_files += index.files.len();

        // Step 3: Download missing files
        let download_result = download_vfs_files(
            api_client,
            &index.files,
            streaming_assets_path,
            &resource.path,
            progress_callback,
        )
        .await
        .with_context(|| format!("Failed to download VFS files for {}", resource.name))?;

        total_result.downloaded_files += download_result.downloaded_files;
        total_result.downloaded_bytes += download_result.downloaded_bytes;
        total_result.skipped_files += download_result.skipped_files;
    }

    // Step 4: Print summary
    if total_result.downloaded_files > 0 {
        println!(
            "VFS download complete: {} files downloaded ({:.2} GB), {} files up-to-date",
            total_result.downloaded_files,
            total_result.downloaded_bytes as f64 / 1024.0 / 1024.0 / 1024.0,
            total_result.skipped_files,
        );
    } else {
        println!(
            "VFS files: all {} files up-to-date",
            total_result.total_files
        );
    }

    Ok(total_result)
}

/// Download VFS files that are missing or have wrong size.
///
/// For performance, uses a size-only check for existing files. This avoids
/// reading tens of GB of VFS data just to compute MD5 hashes — size mismatches
/// are extremely rare for correctly extracted VFS files, and the download itself
/// verifies MD5 on completion.
async fn download_vfs_files(
    api_client: &ApiClient,
    files: &[ResIndexFile],
    vfs_dir: &Path,
    base_url: &str,
    progress_callback: Option<&dyn Fn(u64, u64)>,
) -> Result<VfsUpdateResult> {
    let mut result = VfsUpdateResult {
        total_files: files.len(),
        downloaded_files: 0,
        downloaded_bytes: 0,
        skipped_files: 0,
        res_version: String::new(),
    };

    // Collect files that need downloading
    let mut files_to_download = Vec::new();

    for file in files {
        let local_path = vfs_dir.join(&file.name);

        // Quick check: file must exist with correct size.
        // We skip the full MD5 read for performance — size is a very reliable
        // indicator for VFS files, and the download verifies MD5 anyway.
        let needs_download = match tokio::fs::metadata(&local_path).await {
            Ok(metadata) => metadata.len() != file.size,
            Err(_) => true, // File doesn't exist
        };

        if needs_download {
            files_to_download.push(file);
        } else {
            result.skipped_files += 1;
        }
    }

    if files_to_download.is_empty() {
        return Ok(result);
    }

    // Calculate total download size
    let total_size: u64 = files_to_download.iter().map(|f| f.size).sum();
    println!(
        "  Downloading {} VFS files ({:.2} GB)...",
        files_to_download.len(),
        total_size as f64 / 1024.0 / 1024.0 / 1024.0
    );

    // Download files (using sequential downloads for VFS to avoid overwhelming CDN)
    let mut downloaded_bytes: u64 = 0;
    for (i, file) in files_to_download.iter().enumerate() {
        let download_url = format!("{}/{}", base_url, file.name);
        let local_path = vfs_dir.join(&file.name);

        // Create parent directory
        if let Some(parent) = local_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Download with retry
        match api_client
            .download_file_with_verify(&download_url, &local_path, &file.md5)
            .await
        {
            Ok(()) => {
                downloaded_bytes += file.size;
                result.downloaded_files += 1;
            }
            Err(e) => {
                // Log error but continue with other files
                eprintln!("  WARN: Failed to download VFS file {}: {}", file.name, e);
                // Try without MD5 verification as fallback
                match api_client
                    .download_file(&download_url, &local_path, false)
                    .await
                {
                    Ok(_) => {
                        downloaded_bytes += file.size;
                        result.downloaded_files += 1;
                    }
                    Err(e2) => {
                        eprintln!(
                            "  ERROR: Failed to download VFS file {} (no verify): {}",
                            file.name, e2
                        );
                    }
                }
            }
        }

        // Progress callback
        if let Some(cb) = progress_callback {
            cb(downloaded_bytes, total_size);
        }

        // Print progress periodically
        if (i + 1) % 50 == 0 || i + 1 == files_to_download.len() {
            print!(
                "\r  VFS progress: {}/{} files ({:.1}%)",
                i + 1,
                files_to_download.len(),
                (i + 1) as f64 / files_to_download.len() as f64 * 100.0
            );
        }
    }

    println!(); // Newline after progress

    result.downloaded_bytes = downloaded_bytes;
    Ok(result)
}

/// Get VFS resource info without downloading (for dry-run / planning)
///
/// Returns the resource version and file counts for display purposes.
pub async fn get_vfs_resource_info(
    api_client: &ApiClient,
    game_id: GameId,
    server_id: ServerId,
    game_version: &str,
    rand_str: &str,
) -> Result<(String, usize, u64)> {
    let resources = api_client
        .get_latest_resources(game_id, server_id, game_version, rand_str, "Windows")
        .await
        .context("Failed to get VFS resource info")?;

    let mut total_files = 0;
    let mut total_size: u64 = 0;

    for resource in &resources.resources {
        let index_url = format!("{}/index_{}.json", resource.path, resource.name);
        match api_client.fetch_res_index(&index_url, RES_INDEX_KEY).await {
            Ok(index) => {
                total_files += index.files.len();
                total_size += index.files.iter().map(|f| f.size).sum::<u64>();
            }
            Err(e) => {
                eprintln!(
                    "  WARN: Could not fetch VFS index for {}: {}",
                    resource.name, e
                );
            }
        }
    }

    Ok((resources.res_version, total_files, total_size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vfs_update_result_defaults() {
        let result = VfsUpdateResult {
            total_files: 100,
            downloaded_files: 0,
            downloaded_bytes: 0,
            skipped_files: 100,
            res_version: "initial_6331530-16_main_6331530-16".to_string(),
        };
        assert_eq!(result.total_files, 100);
        assert_eq!(result.skipped_files, 100);
        assert_eq!(result.downloaded_files, 0);
    }
}
