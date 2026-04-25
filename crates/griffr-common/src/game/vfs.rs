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
use crate::game::task_pool::{ProgressEvent, Task, TaskPoolRunner};

#[derive(Debug, Clone, Default)]
pub struct VfsMaterializeConfig {
    /// Candidate StreamingAssets roots from other installs for VFS file reuse.
    pub source_streaming_assets: Vec<std::path::PathBuf>,
    /// Allow copy fallback when hardlinking from source installs fails.
    pub allow_copy_fallback: bool,
    /// Prefer relinking from reuse sources even when local files already verify.
    pub prefer_reuse: bool,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsBootstrapScope {
    /// Materialize only initial pref set into Persistent.
    Initial,
    /// Materialize initial+main pref sets into Persistent.
    Complete,
}

#[derive(Debug, Clone)]
pub struct VfsBootstrapConfig {
    /// Scope for Persistent materialization.
    pub scope: VfsBootstrapScope,
    /// Primary StreamingAssets root for local materialization.
    pub source_streaming_assets: std::path::PathBuf,
    /// Additional StreamingAssets roots from other installs for reuse.
    pub extra_source_streaming_assets: Vec<std::path::PathBuf>,
    /// Allow copy fallback when hardlinking fails.
    pub allow_copy_fallback: bool,
    /// Prefer relinking from source candidates even when destination already verifies.
    pub prefer_reuse: bool,
    /// Allow downloading missing files from CDN when not found in source roots.
    pub allow_download: bool,
    /// Remove files under Persistent/VFS that are outside the selected bootstrap scope.
    pub prune_extra_files: bool,
}

#[derive(Debug, Clone)]
pub struct VfsBootstrapManifestDownload {
    pub url: String,
    pub filename: String,
}

#[derive(Debug, Clone)]
pub struct VfsBootstrapPlan {
    pub tasks: Vec<Task>,
    pub manifest_downloads: Vec<VfsBootstrapManifestDownload>,
    pub total_files: usize,
    pub total_bytes: u64,
    pub expected_paths: std::collections::HashSet<String>,
    pub res_version: String,
    pub scope_label: String,
}

#[derive(Debug, Clone)]
pub struct VfsBootstrapResult {
    pub total_files: usize,
    pub downloaded_files: usize,
    pub downloaded_bytes: u64,
    pub reused_files: usize,
    pub skipped_files: usize,
    pub failed_files: usize,
    pub res_version: String,
    pub scope_label: String,
}

fn should_include_bootstrap_group(scope: VfsBootstrapScope, resource_name: &str) -> bool {
    match scope {
        VfsBootstrapScope::Initial => resource_name.eq_ignore_ascii_case("initial"),
        VfsBootstrapScope::Complete => {
            resource_name.eq_ignore_ascii_case("initial")
                || resource_name.eq_ignore_ascii_case("main")
        }
    }
}

fn read_local_res_index(path: &Path) -> Result<Option<crate::api::types::ResIndex>> {
    if !path.is_file() {
        return Ok(None);
    }
    let encrypted_b64 = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let decrypted = crate::api::crypto::decrypt_res_index(encrypted_b64.trim(), RES_INDEX_KEY)
        .with_context(|| format!("Failed to decrypt {}", path.display()))?;
    let index = serde_json::from_str::<crate::api::types::ResIndex>(&decrypted)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(Some(index))
}

fn res_index_to_ensure_tasks(
    index: &crate::api::types::ResIndex,
    source_candidates: &[std::path::PathBuf],
    resource_path: &str,
    persistent_root: &Path,
    cfg: &VfsBootstrapConfig,
) -> (Vec<Task>, usize, u64) {
    let mut tasks = Vec::new();
    let mut total_files = 0usize;
    let mut total_bytes = 0u64;

    for file in &index.files {
        if file.name.is_empty() {
            continue;
        }
        let expected_md5 = file
            .md5
            .as_deref()
            .or(file.hash.as_deref())
            .unwrap_or("")
            .to_string();
        if expected_md5.is_empty() {
            continue;
        }
        total_files += 1;
        total_bytes = total_bytes.saturating_add(file.size);
        tasks.push(Task::EnsureFile {
            dest: persistent_root.join(&file.name),
            logical_path: file.name.clone(),
            expected_md5,
            expected_size: file.size,
            source_candidates: source_candidates
                .iter()
                .map(|root| root.join(&file.name))
                .collect(),
            download_url: if cfg.allow_download {
                Some(format!("{}/{}", resource_path, file.name))
            } else {
                None
            },
            allow_copy_fallback: cfg.allow_copy_fallback,
            prefer_reuse: cfg.prefer_reuse,
            retry_count: 0,
        });
    }

    (tasks, total_files, total_bytes)
}

fn normalize_rel_path(path: &str) -> String {
    path.replace('\\', "/")
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_ascii_lowercase()
}

fn collect_files_recursive(root: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut stack = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("Failed to read {}", dir.display()))?
        {
            let entry = entry.with_context(|| format!("Failed to read {}", dir.display()))?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                files.push(path);
            }
        }
    }
    Ok(files)
}

fn remove_empty_dirs_recursive(root: &Path) -> Result<()> {
    if !root.is_dir() {
        return Ok(());
    }
    let mut dirs = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        dirs.push(dir.clone());
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("Failed to read {}", dir.display()))?
        {
            let entry = entry.with_context(|| format!("Failed to read {}", dir.display()))?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for dir in dirs {
        if dir == root {
            continue;
        }
        if std::fs::read_dir(&dir)
            .with_context(|| format!("Failed to read {}", dir.display()))?
            .next()
            .is_none()
        {
            let _ = std::fs::remove_dir(&dir);
        }
    }
    Ok(())
}

pub async fn plan_persistent_bootstrap_tasks(
    api_client: &ApiClient,
    game_id: GameId,
    server_id: ServerId,
    game_version: &str,
    rand_str: &str,
    persistent_root: &Path,
    cfg: &VfsBootstrapConfig,
) -> Result<VfsBootstrapPlan> {
    let resources = api_client
        .get_latest_resources(game_id, server_id, game_version, rand_str, "Windows")
        .await
        .context("Failed to get latest VFS resources for bootstrap")?;

    let mut tasks = Vec::new();
    let mut manifest_downloads = Vec::new();
    let mut total_files = 0usize;
    let mut total_bytes = 0u64;
    let mut expected_paths = std::collections::HashSet::new();
    let mut scope_parts = Vec::new();

    let mut source_roots = vec![cfg.source_streaming_assets.clone()];
    for root in &cfg.extra_source_streaming_assets {
        if !source_roots.iter().any(|r| r == root) {
            source_roots.push(root.clone());
        }
    }

    for resource in &resources.resources {
        if !should_include_bootstrap_group(cfg.scope, &resource.name) {
            continue;
        }

        let pref_filename = format!("pref_{}.json", resource.name);
        let index_filename = format!("index_{}.json", resource.name);
        let pref_url = format!("{}/pref_{}.json", resource.path, resource.name);
        let index_url = format!("{}/index_{}.json", resource.path, resource.name);

        let local_pref = read_local_res_index(&persistent_root.join(&pref_filename))
            .with_context(|| format!("Failed to parse local {}", pref_filename))?;
        let local_index = read_local_res_index(&persistent_root.join(&index_filename))
            .with_context(|| format!("Failed to parse local {}", index_filename))?;

        let (selected_index, manifest_kind) = if let Some(pref) = local_pref {
            (pref, "pref-local")
        } else if let Ok(pref) = api_client.fetch_res_index(&pref_url, RES_INDEX_KEY).await {
            manifest_downloads.push(VfsBootstrapManifestDownload {
                url: pref_url,
                filename: pref_filename.clone(),
            });
            (pref, "pref-api")
        } else if let Some(index) = local_index {
            (index, "index-local-fallback")
        } else {
            let index = api_client
                .fetch_res_index(&index_url, RES_INDEX_KEY)
                .await
                .with_context(|| {
                    format!(
                        "Failed to fetch both pref and index manifests for resource group {}",
                        resource.name
                    )
                })?;
            manifest_downloads.push(VfsBootstrapManifestDownload {
                url: index_url,
                filename: index_filename.clone(),
            });
            (index, "index-api-fallback")
        };

        let (group_tasks, group_files, group_bytes) = res_index_to_ensure_tasks(
            &selected_index,
            &source_roots,
            &resource.path,
            persistent_root,
            cfg,
        );
        for task in &group_tasks {
            if let Task::EnsureFile { logical_path, .. } = task {
                expected_paths.insert(normalize_rel_path(logical_path));
            }
        }
        tasks.extend(group_tasks);
        total_files += group_files;
        total_bytes = total_bytes.saturating_add(group_bytes);
        scope_parts.push(format!("{}:{}", resource.name, manifest_kind));
    }

    Ok(VfsBootstrapPlan {
        tasks,
        manifest_downloads,
        total_files,
        total_bytes,
        expected_paths,
        res_version: resources.res_version,
        scope_label: scope_parts.join(","),
    })
}

pub async fn bootstrap_persistent_vfs_with_runner(
    api_client: &ApiClient,
    game_id: GameId,
    server_id: ServerId,
    game_version: &str,
    rand_str: &str,
    persistent_root: &Path,
    cfg: &VfsBootstrapConfig,
    task_pool_runner: &mut TaskPoolRunner,
    progress_callback: Option<&dyn Fn(u64, u64)>,
) -> Result<VfsBootstrapResult> {
    let plan = plan_persistent_bootstrap_tasks(
        api_client,
        game_id,
        server_id,
        game_version,
        rand_str,
        persistent_root,
        cfg,
    )
    .await?;

    compio::fs::create_dir_all(persistent_root)
        .await
        .with_context(|| format!("Failed to create {}", persistent_root.display()))?;

    for manifest in &plan.manifest_downloads {
        let dest = persistent_root.join(&manifest.filename);
        api_client
            .download_file(&manifest.url, &dest, false)
            .await
            .with_context(|| format!("Failed to download {}", manifest.url))?;
    }

    let mut downloaded_paths = HashSet::<String>::new();
    let mut reused_paths = HashSet::<String>::new();
    let mut verified_paths = HashSet::<String>::new();
    let mut failed_paths = HashSet::<String>::new();
    let mut downloaded_bytes = 0u64;
    let mut on_event = |event: &ProgressEvent| match event {
        ProgressEvent::Downloaded { path, bytes } => {
            downloaded_paths.insert(path.clone());
            downloaded_bytes = downloaded_bytes.saturating_add(*bytes);
        }
        ProgressEvent::Hardlinked { path } | ProgressEvent::Copied { path } => {
            reused_paths.insert(path.to_string_lossy().to_string());
        }
        ProgressEvent::Verified { path, ok, .. } => {
            if *ok {
                verified_paths.insert(path.clone());
            } else {
                failed_paths.insert(path.clone());
            }
            if let Some(cb) = progress_callback {
                cb(downloaded_bytes, plan.total_bytes);
            }
        }
        ProgressEvent::Failed { path, .. } => {
            failed_paths.insert(path.clone());
        }
        _ => {}
    };

    let _ = task_pool_runner
        .run_batch_with_progress(plan.tasks, Some(&mut on_event))
        .context("Failed to materialize Persistent VFS bootstrap files")?;

    if cfg.prune_extra_files {
        let vfs_root = persistent_root.join("VFS");
        if vfs_root.is_dir() {
            for file in collect_files_recursive(&vfs_root)? {
                let rel = match file.strip_prefix(persistent_root) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                let rel_norm = normalize_rel_path(&rel.to_string_lossy());
                if !plan.expected_paths.contains(&rel_norm) {
                    std::fs::remove_file(&file)
                        .with_context(|| format!("Failed to remove {}", file.display()))?;
                }
            }
            remove_empty_dirs_recursive(&vfs_root)?;
        }
    }

    let skipped_files = verified_paths
        .len()
        .saturating_sub(downloaded_paths.len())
        .saturating_sub(reused_paths.len());

    Ok(VfsBootstrapResult {
        total_files: plan.total_files,
        downloaded_files: downloaded_paths.len(),
        downloaded_bytes,
        reused_files: reused_paths.len(),
        skipped_files,
        failed_files: failed_paths.len(),
        res_version: plan.res_version,
        scope_label: plan.scope_label,
    })
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
                prefer_reuse: materialize.prefer_reuse,
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

    #[test]
    fn bootstrap_scope_includes_expected_groups() {
        assert!(should_include_bootstrap_group(
            VfsBootstrapScope::Initial,
            "initial"
        ));
        assert!(!should_include_bootstrap_group(
            VfsBootstrapScope::Initial,
            "main"
        ));
        assert!(should_include_bootstrap_group(
            VfsBootstrapScope::Complete,
            "initial"
        ));
        assert!(should_include_bootstrap_group(
            VfsBootstrapScope::Complete,
            "main"
        ));
    }
}
