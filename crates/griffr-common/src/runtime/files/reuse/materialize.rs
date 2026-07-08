use rapidhash::RapidHashMap as HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use futures_util::stream::{self, StreamExt};
use tracing::{info, warn};

use crate::api::types::GameFileEntry;
use crate::api::ApiClient;
use crate::runtime::{
    is_launcher_metadata_path, logical_path_from_root, PathOutcomeTracker, PathReuseMethod,
};

#[allow(clippy::too_many_arguments)]
pub async fn apply_file_reuse_flow(
    api_client: &ApiClient,
    game_id: crate::config::GameId,
    target_server_id: crate::config::ServerId,
    target_version: &str,
    install_path: &Path,
    file_path: &str,
    game_files_md5: Option<&str>,
    config: &super::models::FileReuseConfig,
) -> Result<usize> {
    let summary = materialize_game_files_with_pool(
        api_client,
        game_id,
        target_server_id,
        target_version,
        install_path,
        file_path,
        game_files_md5,
        config,
        None,
        None::<fn(usize, usize, &str)>,
        None::<fn(u64, u64, &str)>,
    )
    .await?;
    if !summary.issues.is_empty() {
        anyhow::bail!(
            "File materialization finished with {} issue(s)",
            summary.issues.len()
        );
    }
    Ok(summary.reused_files)
}

#[allow(clippy::too_many_arguments)]
pub async fn materialize_game_files_with_pool(
    api_client: &ApiClient,
    game_id: crate::config::GameId,
    _target_server_id: crate::config::ServerId,
    _target_version: &str,
    install_path: &Path,
    file_path: &str,
    game_files_md5: Option<&str>,
    config: &super::models::FileReuseConfig,
    task_pool_runner: Option<&mut crate::runtime::task_pool::TaskPoolRunner>,
    progress_callback: Option<impl Fn(usize, usize, &str)>,
    download_progress_callback: Option<impl Fn(u64, u64, &str)>,
) -> Result<super::models::MaterializeSummary> {
    let manifest = api_client
        .fetch_game_files(file_path, game_files_md5)
        .await
        .context("Failed to fetch target manifest for reuse planning")?;
    let files_base_url = super::plan::derive_files_base_url(file_path)?;

    let source_manifest_results = stream::iter(config.source_installs.iter().cloned())
        .map(|source| async move {
            let version_info = api_client
                .get_latest_game(game_id, source.server_id, Some(&source.version))
                .await
                .ok()?;
            let pkg = version_info.pkg.as_ref()?;
            if version_info.version != source.version {
                return None;
            }
            let manifest = api_client
                .fetch_game_files(&pkg.file_path, pkg.game_files_md5.as_deref())
                .await
                .ok()?;
            Some((source, manifest))
        })
        .buffer_unordered(config.source_installs.len().clamp(1, 8))
        .collect::<Vec<_>>()
        .await;

    let target_by_path: HashMap<&str, &GameFileEntry> = manifest
        .iter()
        .filter(|entry| !is_launcher_metadata_path(&entry.path))
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    let mut source_candidates: HashMap<String, Vec<std::path::PathBuf>> = HashMap::default();

    for item in source_manifest_results.into_iter().flatten() {
        let (source, source_manifest) = item;
        for entry in source_manifest {
            if is_launcher_metadata_path(&entry.path) {
                continue;
            }
            let Some(target_entry) = target_by_path.get(entry.path.as_str()) else {
                continue;
            };
            if target_entry.md5.to_lowercase() != entry.md5.to_lowercase()
                || target_entry.size != entry.size
            {
                continue;
            }
            source_candidates
                .entry(entry.path.clone())
                .or_default()
                .push(source.install_path.join(&entry.path));
        }
    }

    let mut dry_run_reused = 0usize;
    let mut dry_run_downloaded = 0usize;
    let tasks = manifest
        .iter()
        .filter(|entry| !is_launcher_metadata_path(&entry.path))
        .map(|entry| {
            let candidates = source_candidates.remove(&entry.path).unwrap_or_default();
            if config.dry_run {
                if candidates.iter().any(|path| path.exists()) {
                    dry_run_reused = dry_run_reused.saturating_add(1);
                } else {
                    dry_run_downloaded = dry_run_downloaded.saturating_add(1);
                }
            }

            crate::runtime::task_pool::Task::EnsureFile {
                dest: install_path.join(&entry.path),
                logical_path: entry.path.clone(),
                expected_md5: entry.md5.clone(),
                expected_size: entry.size,
                source_candidates: candidates,
                download_url: Some(format!("{}/{}", files_base_url, entry.path)),
                allow_copy_fallback: config.allow_copy_fallback,
                prefer_reuse: false,
                retry_count: 0,
            }
        })
        .collect::<Vec<_>>();

    if config.dry_run {
        info!(
            "File materialization dry-run: would_reuse={} would_download={}",
            dry_run_reused, dry_run_downloaded
        );
        return Ok(super::models::MaterializeSummary {
            reused_files: dry_run_reused,
            downloaded_files: dry_run_downloaded,
            issues: Vec::new(),
        });
    }

    let total = tasks.len();
    let total_bytes: u64 = manifest
        .iter()
        .filter(|entry| !is_launcher_metadata_path(&entry.path))
        .map(|entry| entry.size)
        .sum();
    let mut finished_stream = 0usize;
    let mut downloaded_bytes = 0u64;
    let mut progress_event_cb = |event: &crate::runtime::task_pool::ProgressEvent| match event {
        crate::runtime::task_pool::ProgressEvent::Verified { path, .. } => {
            if let Some(ref cb) = progress_callback {
                cb(finished_stream, total, path);
            }
            finished_stream = finished_stream.saturating_add(1);
        }
        crate::runtime::task_pool::ProgressEvent::Downloaded { path, bytes } => {
            downloaded_bytes = downloaded_bytes.saturating_add(*bytes);
            if let Some(ref cb) = download_progress_callback {
                cb(downloaded_bytes, total_bytes, path);
            }
        }
        _ => {}
    };
    let result = if let Some(runner) = task_pool_runner {
        runner
            .run_batch_with_progress(tasks, Some(&mut progress_event_cb))
            .context("File materialization pool failed")?
    } else {
        let mut pool_cfg = crate::runtime::task_pool::TaskPoolConfig::default();
        pool_cfg.io_slots = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(4, 24);
        crate::runtime::task_pool::run_tasks_with_progress(
            tasks,
            pool_cfg,
            Some(&mut progress_event_cb),
        )
        .context("File materialization pool failed")?
    };

    let mut issues = Vec::new();
    let mut outcomes = PathOutcomeTracker::new();
    for event in result.events {
        match event {
            crate::runtime::task_pool::ProgressEvent::Verified { path, ok, issue } => {
                outcomes.record_verified(&path, ok);
                if !ok {
                    if let Some(issue) = issue {
                        issues.push(issue);
                    }
                }
            }
            crate::runtime::task_pool::ProgressEvent::Hardlinked { path } => {
                if let Some(rel) = logical_path_from_root(install_path, &path) {
                    outcomes.record_reused(
                        &rel,
                        PathReuseMethod::Hardlink,
                    );
                }
            }
            crate::runtime::task_pool::ProgressEvent::Copied { path } => {
                if let Some(rel) = logical_path_from_root(install_path, &path) {
                    outcomes.record_reused(
                        &rel,
                        PathReuseMethod::Copy,
                    );
                }
            }
            crate::runtime::task_pool::ProgressEvent::Downloaded { path, bytes } => {
                outcomes.record_downloaded(&path, bytes);
            }
            crate::runtime::task_pool::ProgressEvent::Failed { path, reason } => {
                outcomes.record_failed(&path);
                warn!("materialize failed for {}: {}", path, reason);
            }
            _ => {}
        }
    }
    let summary = outcomes.summary();

    if !config.dry_run {
        info!(
            "File materialization complete: reused={} downloaded={} issues={}",
            summary.reused_files,
            summary.downloaded_files,
            issues.len()
        );
    }

    Ok(super::models::MaterializeSummary {
        reused_files: summary.reused_files,
        downloaded_files: summary.downloaded_files,
        issues,
    })
}
