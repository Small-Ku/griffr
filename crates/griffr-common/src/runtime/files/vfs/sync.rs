use std::path::Path;

use crate::error::{Error, Result};
use tracing::{info, warn};

use crate::api::client::{ApiClient, ApiError};
use crate::api::crypto::RES_INDEX_KEY;
use crate::api::protocol::DEFAULT_PLATFORM;
use crate::config::ApiTarget;
use crate::runtime::task_pool::{
    FileEnsureTask, Task, TaskOutcome, TaskPoolRunner, TaskProgress, TransferClass,
};
use crate::runtime::{
    resource_manifest_url, PathOutcomeTracker, ProgressLane, ProgressSender, ResourceManifestKind,
};

use super::{VfsFilePlanOptions, VfsPlanOutcome, VfsTaskPlan, VfsUpdateOutcome, VfsUpdateResult};

fn plan_vfs_file_task(
    dest: std::path::PathBuf,
    logical_path: String,
    expected_md5: String,
    expected_size: u64,
    source_candidates: Vec<std::path::PathBuf>,
    download_url: String,
    options: &VfsFilePlanOptions,
) -> Task {
    if options.allow_repair {
        Task::ensure_file(FileEnsureTask {
            dest,
            logical_path,
            expected_md5,
            expected_size,
            source_candidates,
            download_url: Some(download_url),
            allow_copy_fallback: options.allow_copy_fallback,
            prefer_reuse: options.prefer_reuse,
            retry_count: 0,
            transfer_class: TransferClass::Vfs,
        })
    } else {
        Task::Verify {
            path: dest,
            logical_path,
            expected_md5,
            expected_size: Some(expected_size),
            on_fail: None,
        }
    }
}

pub async fn plan_vfs_tasks(
    api_client: &ApiClient,
    target: &ApiTarget,
    game_version: &str,
    rand_str: &str,
    streaming_assets_path: &Path,
    options: &VfsFilePlanOptions,
) -> Result<VfsPlanOutcome> {
    let resources = match api_client
        .get_latest_resources(target, game_version, rand_str, DEFAULT_PLATFORM)
        .await
    {
        Ok(res) => res,
        Err(ApiError::ResourcePipelineUnavailable(_)) => return Ok(VfsPlanOutcome::Unsupported),
        Err(err) => return Err(err.into()),
    };

    let mut tasks = Vec::new();
    let mut total_files = 0usize;
    let mut total_bytes = 0u64;

    for resource in &resources.resources {
        let index_url =
            resource_manifest_url(&resource.path, ResourceManifestKind::Index, &resource.name);
        let index = api_client
            .fetch_res_index(&index_url, RES_INDEX_KEY)
            .await
            .map_err(|e| {
                Error::Vfs(format!(
                    "Failed to fetch resource index for {}: {e}",
                    resource.name
                ))
            })?;

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
            let source_candidates = options
                .source_streaming_assets
                .iter()
                .map(|root| root.join(&file.name))
                .collect::<Vec<_>>();
            total_files += 1;
            total_bytes = total_bytes.saturating_add(file.size);
            let dest = streaming_assets_path.join(&file.name);
            tasks.push(plan_vfs_file_task(
                dest,
                file.name.clone(),
                expected_md5,
                file.size,
                source_candidates,
                format!("{}/{}", resource.path, file.name),
                options,
            ));
        }
    }

    Ok(VfsPlanOutcome::Planned(VfsTaskPlan {
        tasks,
        total_files,
        total_bytes,
        res_version: resources.res_version,
    }))
}

/// Check and download VFS game resources after a game update/install
pub async fn download_vfs_resources(
    api_client: &ApiClient,
    target: &ApiTarget,
    game_version: &str,
    rand_str: &str,
    streaming_assets_path: &Path,
    options: &VfsFilePlanOptions,
    task_pool_runner: &mut TaskPoolRunner,
    progress: ProgressSender,
) -> Result<VfsUpdateOutcome> {
    let plan = match plan_vfs_tasks(
        api_client,
        target,
        game_version,
        rand_str,
        streaming_assets_path,
        options,
    )
    .await?
    {
        VfsPlanOutcome::Planned(p) => p,
        VfsPlanOutcome::Unsupported => {
            info!("VFS resources sync is unsupported for this target");
            return Ok(VfsUpdateOutcome::Unsupported);
        }
    };

    info!("VFS resource version: {}", plan.res_version);

    let mut total_result = VfsUpdateResult {
        total_files: plan.total_files,
        downloaded_files: 0,
        downloaded_bytes: 0,
        skipped_files: 0,
        res_version: plan.res_version.clone(),
    };

    let task_progress = TaskProgress::new(progress)
        .with_verify(ProgressLane::VFS_VERIFY, plan.total_files)
        .with_download(ProgressLane::VFS_DOWNLOAD);
    let result = task_pool_runner
        .run_batch(plan.tasks, task_progress)
        .map_err(|e| Error::TaskPool(format!("Failed to ensure VFS files: {e}")))?;

    let mut failed_paths = Vec::<String>::new();
    let mut outcomes = PathOutcomeTracker::new();
    for event in result.outcomes {
        match event {
            TaskOutcome::Downloaded { path, bytes } => {
                outcomes.record_downloaded(&path, bytes);
            }
            TaskOutcome::Verified { path, ok, .. } => {
                outcomes.record_verified(&path, ok);
            }
            TaskOutcome::Failed { path, reason } => {
                warn!("Failed to ensure VFS file {}: {}", path, reason);
                outcomes.record_failed(&path);
                failed_paths.push(path);
            }
            _ => {}
        }
    }

    let summary = outcomes.summary();
    total_result.downloaded_files = summary.downloaded_files;
    total_result.downloaded_bytes = summary.downloaded_bytes;
    total_result.skipped_files = summary.skipped_files;

    if !failed_paths.is_empty() {
        return Err(Error::Vfs(format!(
            "VFS sync failed for {} file(s): {}",
            failed_paths.len(),
            failed_paths.join(", ")
        )));
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

    Ok(VfsUpdateOutcome::Updated(total_result))
}

/// Get VFS resource info without downloading (for dry-run / planning)
///
/// Returns the resource version and file counts for display purposes.
pub async fn get_vfs_resource_info(
    api_client: &ApiClient,
    target: &ApiTarget,
    game_version: &str,
    rand_str: &str,
) -> Result<Option<(String, usize, u64)>> {
    let resources = match api_client
        .get_latest_resources(target, game_version, rand_str, DEFAULT_PLATFORM)
        .await
    {
        Ok(res) => res,
        Err(ApiError::ResourcePipelineUnavailable(_)) => return Ok(None),
        Err(err) => return Err(err.into()),
    };

    let mut total_files = 0;
    let mut total_size: u64 = 0;

    for resource in &resources.resources {
        let index_url =
            resource_manifest_url(&resource.path, ResourceManifestKind::Index, &resource.name);
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

    Ok(Some((resources.res_version, total_files, total_size)))
}

#[cfg(test)]
mod tests {
    use super::super::bootstrap::{should_include_bootstrap_group, VfsBootstrapScope};
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

    #[test]
    fn read_only_vfs_plan_has_no_repair_continuation() {
        let task = plan_vfs_file_task(
            "StreamingAssets/VFS/file.blc".into(),
            "VFS/file.blc".to_string(),
            "00".repeat(16),
            4,
            vec!["source/VFS/file.blc".into()],
            "https://example.invalid/VFS/file.blc".to_string(),
            &VfsFilePlanOptions {
                source_streaming_assets: Vec::new(),
                allow_repair: false,
                allow_copy_fallback: false,
                prefer_reuse: false,
            },
        );

        assert!(matches!(task, Task::Verify { on_fail: None, .. }));
    }
}
