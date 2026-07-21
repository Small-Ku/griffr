use rapidhash::RapidHashMap as HashMap;
use std::path::Path;

use crate::error::{Error, Result};
use futures_util::stream::{self, StreamExt};
use tracing::{info, warn};

use crate::api::types::GameFileEntry;
use crate::api::ApiClient;
use crate::runtime::task_pool::{FileEnsureTask, Task, TransferClass};
use crate::runtime::{
    build_cdn_file_url, files_base_url, is_launcher_metadata_path, logical_path_from_root,
    path_is_file, PathOutcomeTracker, PathReuseMethod, ProgressLane, ProgressSender,
};

#[allow(clippy::too_many_arguments)]
pub async fn ensure_game_files_with_pool(
    api_client: &ApiClient,
    game_id: crate::config::GameId,
    install_path: &Path,
    file_path: &str,
    game_files_md5: Option<&str>,
    config: &super::types::FileReuseConfig,
    task_pool_runner: Option<&mut crate::runtime::task_pool::TaskPoolRunner>,
    progress: ProgressSender,
) -> Result<super::types::FileEnsureSummary> {
    let manifest = api_client
        .fetch_game_files(file_path, game_files_md5)
        .await
        .map_err(|e| Error::Message {
            context: "API client wrapper error: ",
            detail: format!("Failed to fetch target manifest for reuse planning: {e}"),
        })?;
    let files_url_base = files_base_url(file_path)?;

    let source_manifest_results = stream::iter(config.source_installs.iter().cloned())
        .map(|source| {
            let game_id = game_id.clone();
            async move {
                let target = crate::config::resolve_api_target(
                    &game_id,
                    source.region_id,
                    &source.channel_id,
                    &crate::config::ApiTargetOverrides::default(),
                )
                .ok()?;
                let version_info = api_client
                    .get_latest_game(&target, Some(&source.version))
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
            }
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
    let mut tasks = Vec::with_capacity(manifest.len());
    for entry in manifest
        .iter()
        .filter(|entry| !is_launcher_metadata_path(&entry.path))
    {
        let candidates = source_candidates.remove(&entry.path).unwrap_or_default();
        if config.dry_run {
            let mut reusable_candidate_exists = false;
            for path in &candidates {
                if path_is_file(path).await {
                    reusable_candidate_exists = true;
                    break;
                }
            }
            if reusable_candidate_exists {
                dry_run_reused = dry_run_reused.saturating_add(1);
            } else {
                dry_run_downloaded = dry_run_downloaded.saturating_add(1);
            }
        }

        tasks.push(Task::ensure_file(FileEnsureTask {
            dest: install_path.join(&entry.path),
            logical_path: entry.path.clone(),
            expected_md5: entry.md5.clone(),
            expected_size: entry.size,
            source_candidates: candidates,
            download_url: Some(build_cdn_file_url(files_url_base, &entry.path)),
            allow_copy_fallback: config.allow_copy_fallback,
            prefer_reuse: false,
            retry_count: 0,
            transfer_class: TransferClass::General,
            archive_repair: None,
        }));
    }

    if config.dry_run {
        info!(
            "Game-file ensure dry-run: would_reuse={} would_download={}",
            dry_run_reused, dry_run_downloaded
        );
        return Ok(super::types::FileEnsureSummary {
            reused_files: dry_run_reused,
            downloaded_files: dry_run_downloaded,
            issues: Vec::new(),
        });
    }

    let total = tasks.len();
    let task_progress = crate::runtime::task_pool::TaskProgress::new(progress)
        .with_verify(ProgressLane::FILE_ENSURE_VERIFY, total)
        .with_download(ProgressLane::FILE_ENSURE_DOWNLOAD);
    let result = if let Some(runner) = task_pool_runner {
        runner
            .run_batch(tasks, task_progress)
            .map_err(|e| Error::Message {
                context: "Task pool error: ",
                detail: format!("Game-file ensure pool failed: {e}"),
            })?
    } else {
        let pool_cfg = crate::runtime::task_pool::TaskPoolConfig::for_file_ensure();
        crate::runtime::task_pool::run_tasks_with_progress(tasks, pool_cfg, task_progress).map_err(
            |e| Error::Message {
                context: "Task pool error: ",
                detail: format!("Game-file ensure pool failed: {e}"),
            },
        )?
    };

    let mut issues = Vec::new();
    let mut outcomes = PathOutcomeTracker::new();
    for event in result.outcomes {
        match event {
            crate::runtime::task_pool::TaskOutcome::Verified { path, ok, issue } => {
                outcomes.record_verified(&path, ok);
                if !ok {
                    if let Some(issue) = issue {
                        issues.push(issue);
                    }
                }
            }
            crate::runtime::task_pool::TaskOutcome::Hardlinked { path } => {
                if let Some(rel) = logical_path_from_root(install_path, &path) {
                    outcomes.record_reused(&rel, PathReuseMethod::Hardlink);
                }
            }
            crate::runtime::task_pool::TaskOutcome::Copied { path } => {
                if let Some(rel) = logical_path_from_root(install_path, &path) {
                    outcomes.record_reused(&rel, PathReuseMethod::Copy);
                }
            }
            crate::runtime::task_pool::TaskOutcome::Downloaded { path, bytes } => {
                outcomes.record_downloaded(&path, bytes);
            }
            crate::runtime::task_pool::TaskOutcome::Failed { path, reason } => {
                outcomes.record_failed(&path);
                warn!("Failed to ensure game file {}: {}", path, reason);
            }
            _ => {}
        }
    }
    let summary = outcomes.summary();

    if !config.dry_run {
        info!(
            "Game-file ensure finished: reused={} downloaded={} issues={}",
            summary.reused_files,
            summary.downloaded_files,
            issues.len()
        );
    }

    Ok(super::types::FileEnsureSummary {
        reused_files: summary.reused_files,
        downloaded_files: summary.downloaded_files,
        issues,
    })
}
