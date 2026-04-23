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
use std::collections::HashSet;
use std::path::Path;
use tracing::{info, warn};

use crate::api::client::ApiClient;
use crate::api::crypto::RES_INDEX_KEY;
use crate::config::{GameId, ServerId};
use crate::game::task_pool::{
    ProgressEvent, Task, TaskPoolRunner,
};

#[derive(Debug, Clone, Default)]
pub struct VfsMaterializeConfig {
    /// Candidate StreamingAssets roots from other installs for VFS file reuse.
    pub source_streaming_assets: Vec<std::path::PathBuf>,
    /// Allow copy fallback when hardlinking from source installs fails.
    pub allow_copy_fallback: bool,
}

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

#[derive(Debug, Clone)]
pub struct VfsTaskPlan {
    pub tasks: Vec<Task>,
    pub total_files: usize,
    pub total_bytes: u64,
    pub res_version: String,
}

pub async fn plan_vfs_tasks(
    api_client: &ApiClient,
    game_id: GameId,
    server_id: ServerId,
    game_version: &str,
    rand_str: &str,
    streaming_assets_path: &Path,
    materialize: &VfsMaterializeConfig,
) -> Result<VfsTaskPlan> {
    let resources = api_client
        .get_latest_resources(game_id, server_id, game_version, rand_str, "Windows")
        .await
        .context("Failed to get latest VFS resources")?;

    let mut tasks = Vec::new();
    let mut total_files = 0usize;
    let mut total_bytes = 0u64;

    for resource in &resources.resources {
        let index_url = format!("{}/index_{}.json", resource.path, resource.name);
        let index = api_client
            .fetch_res_index(&index_url, RES_INDEX_KEY)
            .await
            .with_context(|| format!("Failed to fetch resource index for {}", resource.name))?;

        for file in &index.files {
            if file.name.is_empty() {
                warn!(
                    "Skipping VFS file with empty name in index {}",
                    resource.name
                );
                continue;
            }
            let expected_md5 = file
                .md5
                .as_deref()
                .or(file.hash.as_deref())
                .unwrap_or("")
                .to_string();
            if expected_md5.is_empty() {
                warn!(
                    "Skipping VFS file without checksum in index {}: {}",
                    resource.name, file.name
                );
                continue;
            }
            let source_candidates = materialize
                .source_streaming_assets
                .iter()
                .map(|root| root.join(&file.name))
                .collect::<Vec<_>>();
            total_files += 1;
            total_bytes = total_bytes.saturating_add(file.size);
            tasks.push(Task::EnsureFile {
                dest: streaming_assets_path.join(&file.name),
                logical_path: file.name.clone(),
                expected_md5,
                expected_size: file.size,
                source_candidates,
                download_url: Some(format!("{}/{}", resource.path, file.name)),
                allow_copy_fallback: materialize.allow_copy_fallback,
                prefer_reuse: false,
                retry_count: 0,
            });
        }
    }

    Ok(VfsTaskPlan {
        tasks,
        total_files,
        total_bytes,
        res_version: resources.res_version,
    })
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
/// Check/download VFS resources using task-pool EnsureFile flow with optional
/// hardlink/copy reuse from source installs.
pub async fn download_vfs_resources(
    api_client: &ApiClient,
    game_id: GameId,
    server_id: ServerId,
    game_version: &str,
    rand_str: &str,
    streaming_assets_path: &Path,
    materialize: &VfsMaterializeConfig,
    task_pool_runner: &mut TaskPoolRunner,
    progress_callback: Option<&dyn Fn(u64, u64)>,
) -> Result<VfsUpdateResult> {
    let plan = plan_vfs_tasks(
        api_client,
        game_id,
        server_id,
        game_version,
        rand_str,
        streaming_assets_path,
        materialize,
    )
    .await?;

    info!("VFS resource version: {}", plan.res_version);

    let mut total_result = VfsUpdateResult {
        total_files: plan.total_files,
        downloaded_files: 0,
        downloaded_bytes: 0,
        skipped_files: 0,
        res_version: plan.res_version.clone(),
    };

    let mut downloaded_paths = HashSet::<String>::new();
    let mut verified_paths = HashSet::<String>::new();
    let mut failed_paths = Vec::<String>::new();
    let mut downloaded_bytes = 0u64;
    let mut on_event = |event: &ProgressEvent| match event {
        ProgressEvent::Downloaded { path, bytes } => {
            downloaded_paths.insert(path.clone());
            downloaded_bytes = downloaded_bytes.saturating_add(*bytes);
        }
        ProgressEvent::Verified { path, ok, .. } => {
            if *ok {
                verified_paths.insert(path.clone());
            }
            if let Some(cb) = progress_callback {
                cb(downloaded_bytes, plan.total_bytes);
            }
        }
        ProgressEvent::Failed { path, reason } => {
            warn!("Failed to materialize VFS file {}: {}", path, reason);
            failed_paths.push(path.clone());
        }
        _ => {}
    };
    let _ = task_pool_runner
        .run_batch_with_progress(plan.tasks, Some(&mut on_event))
        .context("Failed to materialize VFS files")?;

    total_result.downloaded_files = downloaded_paths.len();
    total_result.downloaded_bytes = downloaded_bytes;
    total_result.skipped_files = verified_paths.len().saturating_sub(downloaded_paths.len());

    if !failed_paths.is_empty() {
        warn!("VFS sync had {} failed file(s)", failed_paths.len());
    }

    // Step 4: Print summary
    if total_result.downloaded_files > 0 {
        info!(
            "VFS download complete: {} files downloaded ({:.2} GB), {} files up-to-date",
            total_result.downloaded_files,
            total_result.downloaded_bytes as f64 / 1024.0 / 1024.0 / 1024.0,
            total_result.skipped_files,
        );
    } else {
        info!(
            "VFS files: all {} files up-to-date",
            total_result.total_files
        );
    }

    Ok(total_result)
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
                warn!("Could not fetch VFS index for {}: {}", resource.name, e);
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
