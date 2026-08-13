use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{Error, Result};
use futures_util::{stream, StreamExt, TryStreamExt};
use tracing::{info, warn};

use crate::api::client::ApiClient;
use crate::api::crypto::RES_INDEX_KEY;
use crate::api::protocol::DEFAULT_PLATFORM;
use crate::config::ApiTarget;
use crate::runtime::artifact::physical_path_key;
use crate::runtime::task_pool::{
    FileEnsureTask, Task, TaskOutcome, TaskPoolRunner, TaskProgress, TransferClass,
};
use crate::runtime::{
    build_cdn_file_url, resource_manifest_filename, resource_manifest_url, ArtifactClaim,
    ArtifactExpectation, PathOutcomeTracker, ProgressLane, ProgressSender, ResourceManifestKind,
};

use super::{
    finish_vfs_plan, ManagedResourceFile, ResourceIdentity, ResourceManifestIdentity,
    VfsFilePlanOptions, VfsManifestCommit, VfsTaskPlan, VfsUpdateResult,
};

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
            expected_hash: crate::runtime::ContentHash::from(&expected_md5),
            expected_size,
            source_candidates,
            download_url: Some(download_url),
            allow_copy_fallback: options.allow_copy_fallback,
            copy_only: false,
            prefer_reuse: options.prefer_reuse,
            retry_count: 0,
            transfer_class: TransferClass::Vfs,
            archive_repair: None,
        })
    } else {
        Task::Verify {
            path: dest,
            logical_path,
            expected_hash: crate::runtime::ContentHash::from(&expected_md5),
            expected_size: Some(expected_size),
            on_fail: None,
        }
    }
}

fn register_resource_claim(
    claims_by_path: &mut BTreeMap<String, ArtifactClaim>,
    claim: ArtifactClaim,
) -> Result<bool> {
    let key = physical_path_key(claim.path());
    if let Some(previous) = claims_by_path.get(&key) {
        let previous_expectation = previous.expectation();
        let expectation = claim.expectation();
        if previous_expectation.expected_md5() != expectation.expected_md5()
            || previous_expectation.expected_size() != expectation.expected_size()
        {
            return Err(Error::Message {
                context: "VFS error: ",
                detail: format!(
                    "Resource manifests claim {} with different expected content",
                    claim.path().display()
                ),
            });
        }
        return Ok(false);
    }
    claims_by_path.insert(key, claim);
    Ok(true)
}

/// Returns whether a decrypted resource index's version string is compatible with the expected resource versions.
///
/// An index version is compatible if:
/// - It is empty (many server index manifests omit or leave the internal top-level version field blank), OR
/// - It matches the specific resource group's version (e.g. `"8764515-7"`), OR
/// - It matches the aggregate resource version string (e.g. `"initial_8764515-7_main_8764515-7"`).
pub fn is_compatible_res_index_version(
    index_version: &str,
    group_version: &str,
    aggregate_res_version: &str,
) -> bool {
    let trimmed = index_version.trim();
    trimmed.is_empty() || trimmed == group_version.trim() || trimmed == aggregate_res_version.trim()
}

pub async fn plan_vfs_tasks(
    api_client: &ApiClient,
    target: &ApiTarget,
    game_version: &str,
    rand_str: &str,
    streaming_assets_path: &Path,
    options: &VfsFilePlanOptions,
) -> Result<Option<VfsTaskPlan>> {
    let Some(resources) = api_client
        .get_latest_resources(target, game_version, rand_str, DEFAULT_PLATFORM)
        .await?
    else {
        return Ok(None);
    };

    let mut tasks = Vec::new();
    let mut claims_by_path = BTreeMap::new();
    let mut manifest_commits = Vec::new();
    let mut total_files = 0usize;
    let mut total_bytes = 0u64;

    let resource_documents = stream::iter(&resources.resources)
        .map(|resource| async move {
            let index_url =
                resource_manifest_url(&resource.path, ResourceManifestKind::Index, &resource.name);
            let document = api_client
                .fetch_res_index_document(&index_url, RES_INDEX_KEY)
                .await
                .map_err(|e| Error::Message {
                    context: "VFS error: ",
                    detail: format!("Failed to fetch resource index for {}: {e}", resource.name),
                })?;
            Ok::<_, Error>((resource, document))
        })
        .buffered(super::RESOURCE_MANIFEST_CONCURRENCY);
    futures_util::pin_mut!(resource_documents);

    while let Some((resource, document)) = resource_documents.try_next().await? {
        if !is_compatible_res_index_version(
            &document.index.version,
            &resource.version,
            &resources.res_version,
        ) {
            return Err(Error::Message {
                context: "VFS error: ",
                detail: format!(
                    "Resource index {} has version {}, expected {} or {}",
                    resource.name, document.index.version, resource.version, resources.res_version
                ),
            });
        }
        let manifest_filename =
            resource_manifest_filename(ResourceManifestKind::Index, &resource.name);
        let manifest_dest = streaming_assets_path.join(&manifest_filename);
        let manifest_commit = VfsManifestCommit {
            dest: manifest_dest.clone(),
            logical_path: manifest_filename.clone(),
            encrypted_bytes: document.encrypted_bytes,
            expected_md5: document.md5,
        };
        let manifest_claim = manifest_commit.claim();
        let new_manifest = register_resource_claim(&mut claims_by_path, manifest_claim)?;
        if new_manifest && !options.allow_repair {
            tasks.push(Task::Verify {
                path: manifest_dest,
                logical_path: manifest_filename,
                expected_hash: crate::runtime::ContentHash::from(&manifest_commit.expected_md5),
                expected_size: Some(manifest_commit.encrypted_bytes.len() as u64),
                on_fail: None,
            });
        }
        if new_manifest {
            manifest_commits.push(manifest_commit);
        }

        for file in &document.index.files {
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
                .trim()
                .to_ascii_lowercase();
            if expected_md5.is_empty() {
                warn!(
                    "Skipping VFS file without checksum in index {}: {}",
                    resource.name, file.name
                );
                continue;
            }
            if expected_md5.len() != 32
                || !expected_md5.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(Error::Message {
                    context: "VFS error: ",
                    detail: format!(
                        "Resource index {} entry {} contains invalid MD5 {:?}",
                        resource.name, file.name, expected_md5
                    ),
                });
            }
            let relative =
                crate::runtime::task_pool::fs_ops::path_safety::parse_safe_relative_path(
                    "resource index path",
                    &file.name,
                )?;
            let logical_path = relative.to_string_lossy().replace('\\', "/");
            let source_candidates = options
                .source_streaming_assets
                .iter()
                .map(|root| root.join(&relative))
                .collect::<Vec<_>>();
            let dest = streaming_assets_path.join(&relative);
            let claim = ArtifactClaim::new(
                dest.clone(),
                ArtifactExpectation::new(&logical_path, &expected_md5, Some(file.size)),
            );
            if !register_resource_claim(&mut claims_by_path, claim)? {
                continue;
            }
            total_files += 1;
            total_bytes = total_bytes.saturating_add(file.size);
            tasks.push(plan_vfs_file_task(
                dest,
                logical_path,
                expected_md5,
                file.size,
                source_candidates,
                build_cdn_file_url(&resource.path, &file.name),
                options,
            ));
        }
    }

    let claims = claims_by_path.into_values().collect::<Vec<_>>();
    let mut managed_files = Vec::with_capacity(claims.len());
    for claim in &claims {
        let Some(size) = claim.expectation().expected_size() else {
            return Err(Error::Message {
                context: "VFS error: ",
                detail: format!(
                    "Resource claim {} does not include an expected size",
                    claim.path().display()
                ),
            });
        };
        managed_files.push(ManagedResourceFile {
            path: claim.expectation().logical_path().to_string(),
            md5: claim.expectation().expected_md5().to_string(),
            size,
        });
    }
    managed_files.sort_by(|left, right| left.path.cmp(&right.path));
    let mut identity_manifests = manifest_commits
        .iter()
        .map(|manifest| ResourceManifestIdentity {
            logical_path: manifest.logical_path.clone(),
            md5: manifest.expected_md5.clone(),
            size: manifest.encrypted_bytes.len() as u64,
        })
        .collect::<Vec<_>>();
    identity_manifests.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
    let identity = ResourceIdentity {
        res_version: resources.res_version.clone(),
        manifests: identity_manifests,
    };

    Ok(Some(VfsTaskPlan {
        tasks,
        claims,
        manifest_commits,
        managed_files,
        streaming_assets_root: streaming_assets_path.to_path_buf(),
        identity: Some(identity),
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
) -> Result<Option<VfsUpdateResult>> {
    let Some(mut plan) = plan_vfs_tasks(
        api_client,
        target,
        game_version,
        rand_str,
        streaming_assets_path,
        options,
    )
    .await?
    else {
        info!("VFS resources sync is unsupported for this target");
        return Ok(None);
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
        .run_batch(std::mem::take(&mut plan.tasks), task_progress)
        .map_err(|e| Error::Message {
            context: "Task pool error: ",
            detail: format!("Failed to ensure VFS files: {e}"),
        })?;

    let mut failed_paths = Vec::<String>::new();
    let mut outcomes = PathOutcomeTracker::new();
    for event in result.outcomes {
        match event {
            TaskOutcome::Downloaded { path, bytes } => {
                outcomes.record_downloaded(&path, bytes);
            }
            TaskOutcome::Committed { proof } => {
                outcomes.record_verified(proof.logical_path(), true);
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
    let failed_graph_nodes = result
        .metrics
        .graph
        .failed_nodes
        .saturating_add(result.metrics.graph.cancelled_nodes);

    if failed_graph_nodes > 0 || !failed_paths.is_empty() {
        return Err(Error::Message {
            context: "VFS error: ",
            detail: format!(
                "VFS sync failed for {} reported path(s) and {} failed or cancelled graph node(s): {}",
                failed_paths.len(),
                failed_graph_nodes,
                failed_paths.join(", ")
            ),
        });
    }
    if options.allow_repair {
        let install_root = streaming_assets_path
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| Error::Message {
                context: "VFS error: ",
                detail: format!(
                    "Cannot derive the install root from StreamingAssets path {}",
                    streaming_assets_path.display()
                ),
            })?;
        finish_vfs_plan(install_root, &plan, true).await?;
    }

    // Step 4: Print summary
    if total_result.downloaded_files > 0 {
        info!(
            "VFS download finished: {} files downloaded ({:.2} GB), {} files up-to-date",
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

    Ok(Some(total_result))
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
    let Some(resources) = api_client
        .get_latest_resources(target, game_version, rand_str, DEFAULT_PLATFORM)
        .await?
    else {
        return Ok(None);
    };

    let mut total_files = 0;
    let mut total_size: u64 = 0;

    let resource_indexes = stream::iter(&resources.resources)
        .map(|resource| async move {
            let index_url =
                resource_manifest_url(&resource.path, ResourceManifestKind::Index, &resource.name);
            (
                resource.name.as_str(),
                api_client.fetch_res_index(&index_url, RES_INDEX_KEY).await,
            )
        })
        .buffered(super::RESOURCE_MANIFEST_CONCURRENCY);
    futures_util::pin_mut!(resource_indexes);

    while let Some((resource_name, result)) = resource_indexes.next().await {
        match result {
            Ok(index) => {
                total_files += index.files.len();
                total_size += index.files.iter().map(|f| f.size).sum::<u64>();
            }
            Err(e) => {
                warn!("Could not fetch VFS index for {}: {}", resource_name, e);
            }
        }
    }

    Ok(Some((resources.res_version, total_files, total_size)))
}

#[cfg(test)]
mod tests {
    use super::super::setup::{file_set_includes_group, PersistentVfsFileSet};
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
    fn persistent_vfs_file_set_includes_expected_groups() {
        assert!(file_set_includes_group(
            PersistentVfsFileSet::Base,
            "initial"
        ));
        assert!(!file_set_includes_group(PersistentVfsFileSet::Base, "main"));
        assert!(file_set_includes_group(
            PersistentVfsFileSet::All,
            "initial"
        ));
        assert!(file_set_includes_group(PersistentVfsFileSet::All, "main"));
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
    #[test]
    fn duplicate_resource_claim_with_same_expectation_is_shared() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("StreamingAssets/VFS/file.bin");
        let expectation = ArtifactExpectation::new("VFS/file.bin", "00".repeat(16), Some(4));
        let mut claims = BTreeMap::new();

        assert!(register_resource_claim(
            &mut claims,
            ArtifactClaim::new(path.clone(), expectation.clone())
        )
        .unwrap());
        assert!(
            !register_resource_claim(&mut claims, ArtifactClaim::new(path, expectation)).unwrap()
        );
        assert_eq!(claims.len(), 1);
    }

    #[test]
    fn duplicate_resource_claim_with_different_expectation_fails() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("StreamingAssets/VFS/file.bin");
        let mut claims = BTreeMap::new();
        register_resource_claim(
            &mut claims,
            ArtifactClaim::new(
                path.clone(),
                ArtifactExpectation::new("VFS/file.bin", "00".repeat(16), Some(4)),
            ),
        )
        .unwrap();

        let error = register_resource_claim(
            &mut claims,
            ArtifactClaim::new(
                path,
                ArtifactExpectation::new("VFS/file.bin", "11".repeat(16), Some(4)),
            ),
        )
        .unwrap_err();
        assert!(error.to_string().contains("different expected content"));
    }

    #[test]
    fn test_is_compatible_res_index_version() {
        let group_ver = "8764515-7";
        let agg_ver = "initial_8764515-7_main_8764515-7";

        // Empty version string in manifest is compatible
        assert!(is_compatible_res_index_version("", group_ver, agg_ver));
        assert!(is_compatible_res_index_version("  ", group_ver, agg_ver));

        // Matching group version is compatible
        assert!(is_compatible_res_index_version(
            group_ver, group_ver, agg_ver
        ));

        // Matching aggregate version is compatible
        assert!(is_compatible_res_index_version(agg_ver, group_ver, agg_ver));

        // Mismatched version is incompatible
        assert!(!is_compatible_res_index_version(
            "9999999-9",
            group_ver,
            agg_ver
        ));
    }
}
