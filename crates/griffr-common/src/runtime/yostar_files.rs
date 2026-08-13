use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::api::yostar::{YostarManifest, YostarManifestEntry};
use crate::error::{Error, Result};
use crate::runtime::files::reuse::FileEnsureSummary;
use crate::runtime::task_pool::{
    FileEnsureTask, Task, TaskOutcome, TaskPoolRunner, TaskProgress, TransferClass,
};
use crate::runtime::{
    is_griffr_private_path, is_launcher_metadata_path, normalize_logical_path, ContentHash,
    PathOutcomeTracker, ProgressLane, ProgressSender,
};

fn planned_entries(manifest: &YostarManifest) -> Result<Vec<(&YostarManifestEntry, PathBuf)>> {
    crate::runtime::validate_remote_yostar_manifest(manifest)?;
    let mut out = Vec::with_capacity(manifest.files.len());
    for entry in &manifest.files {
        let relative = crate::runtime::task_pool::fs_ops::path_safety::parse_safe_relative_path(
            "YoStar manifest path",
            &entry.path,
        )?;
        if is_griffr_private_path(&relative) || is_launcher_metadata_path(&entry.path) {
            return Err(Error::Message {
                context: "YoStar manifest error: ",
                detail: format!("manifest cannot own launcher/private path {}", entry.path),
            });
        }
        out.push((entry, relative));
    }
    Ok(out)
}

fn provider_attempt_roots(cdn_roots: &[String]) -> Vec<String> {
    if cdn_roots.is_empty() {
        return vec![String::new()];
    }
    let mut roots = cdn_roots.to_vec();
    // The observed launcher returns to the primary CDN after trying its
    // backup. TaskPoolRunner already retries each individual provider, so the
    // phase sequence here only models provider failover/recovery.
    if roots.len() > 1 {
        roots.push(roots[0].clone());
    }
    roots
}

fn metadata_only_paths(
    current: Option<&YostarManifest>,
    target: &YostarManifest,
) -> BTreeSet<String> {
    let Some(current) = current else {
        return BTreeSet::new();
    };
    let current = current
        .files
        .iter()
        .map(|entry| {
            (
                normalize_logical_path(&entry.path),
                (entry.hash.as_str(), entry.size),
            )
        })
        .collect::<BTreeMap<_, _>>();
    target
        .files
        .iter()
        .filter_map(|entry| {
            let key = normalize_logical_path(&entry.path);
            current
                .get(&key)
                .is_some_and(|(hash, size)| *size == entry.size && *hash == entry.hash)
                .then_some(key)
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub async fn ensure_yostar_files_with_pool(
    install_path: &Path,
    current: Option<&YostarManifest>,
    target: &YostarManifest,
    cdn_roots: &[String],
    source_roots: &[PathBuf],
    allow_copy_fallback: bool,
    skip_destination_check: bool,
    dry_run: bool,
    runner: &mut TaskPoolRunner,
    progress: ProgressSender,
) -> Result<FileEnsureSummary> {
    let planned = planned_entries(target)?;
    let metadata_only = metadata_only_paths(current, target);
    if dry_run {
        let mut summary = FileEnsureSummary::default();
        for (entry, relative) in planned {
            let hash = ContentHash::crc64_xz_decimal(&entry.hash)?;
            let metadata = metadata_only.contains(&normalize_logical_path(&entry.path));
            let issue = if metadata {
                crate::runtime::task_pool::verify::build_metadata_issue(
                    &install_path.join(&relative),
                    &entry.path,
                    &hash,
                    entry.size,
                )
            } else {
                crate::runtime::task_pool::verify::build_issue(
                    &install_path.join(&relative),
                    &entry.path,
                    &hash,
                    Some(entry.size),
                )
            };
            if issue.is_some() {
                summary.downloaded_files += 1;
            }
        }
        return Ok(summary);
    }

    let roots = provider_attempt_roots(cdn_roots);
    let mut last_error = None;
    for (index, cdn_root) in roots.iter().enumerate() {
        let mut tasks = Vec::with_capacity(planned.len());
        for (entry, relative) in &planned {
            let spec = FileEnsureTask {
                dest: install_path.join(relative),
                logical_path: entry.path.clone(),
                expected_hash: ContentHash::crc64_xz_decimal(&entry.hash)?,
                expected_size: entry.size,
                source_candidates: source_roots
                    .iter()
                    .map(|root| root.join(relative))
                    .collect(),
                download_url: (!cdn_root.is_empty()).then(|| target.file_url(cdn_root, entry)),
                allow_copy_fallback,
                copy_only: false,
                prefer_reuse: false,
                retry_count: 0,
                transfer_class: TransferClass::General,
                archive_repair: None,
            };
            tasks.push(if skip_destination_check {
                Task::materialize_file(spec)
            } else if metadata_only.contains(&normalize_logical_path(&entry.path)) {
                Task::ensure_file_metadata(spec)
            } else {
                Task::ensure_file(spec)
            });
        }
        let task_progress = TaskProgress::new(progress.clone())
            .with_verify(ProgressLane::FILE_ENSURE_VERIFY, tasks.len())
            .with_download(ProgressLane::FILE_ENSURE_DOWNLOAD);
        match runner.run_batch(tasks, task_progress) {
            Ok(result) => match summarize_result(result.outcomes, &result.metrics.graph, true) {
                Ok(summary) => return Ok(summary),
                Err(error) => last_error = Some(error),
            },
            Err(error) => {
                last_error = Some(Error::Message {
                    context: "YoStar materialization error: ",
                    detail: error.to_string(),
                })
            }
        }
        if index + 1 < roots.len() {
            tracing::warn!(
                "YoStar CDN provider failed; retrying unresolved files on the next provider phase"
            );
        }
    }
    Err(last_error.unwrap_or_else(|| Error::Message {
        context: "YoStar materialization error: ",
        detail: "no download provider was available".to_string(),
    }))
}

pub async fn check_yostar_file_metadata_with_pool(
    install_path: &Path,
    manifest: &YostarManifest,
    runner: &mut TaskPoolRunner,
    progress: ProgressSender,
) -> Result<FileEnsureSummary> {
    let planned = planned_entries(manifest)?;
    let tasks = planned
        .into_iter()
        .map(|(entry, relative)| {
            Ok(Task::VerifyMetadata {
                path: install_path.join(relative),
                logical_path: entry.path.clone(),
                expected_hash: ContentHash::crc64_xz_decimal(&entry.hash)?,
                expected_size: entry.size,
                on_fail: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let task_progress = TaskProgress::new(progress)
        .with_verify(ProgressLane::INTEGRITY_VERIFY, tasks.len())
        .with_download(ProgressLane::INTEGRITY_DOWNLOAD);
    let result = runner.run_batch(tasks, task_progress)?;
    summarize_result(result.outcomes, &result.metrics.graph, false)
}

pub async fn verify_yostar_files_with_pool(
    install_path: &Path,
    manifest: &YostarManifest,
    repair: bool,
    cdn_roots: &[String],
    source_roots: &[PathBuf],
    allow_copy_fallback: bool,
    runner: &mut TaskPoolRunner,
    progress: ProgressSender,
) -> Result<FileEnsureSummary> {
    if repair {
        return ensure_yostar_files_with_pool(
            install_path,
            None,
            manifest,
            cdn_roots,
            source_roots,
            allow_copy_fallback,
            false,
            false,
            runner,
            progress,
        )
        .await;
    }

    let planned = planned_entries(manifest)?;
    let tasks = planned
        .into_iter()
        .map(|(entry, relative)| {
            Ok(Task::Verify {
                path: install_path.join(relative),
                logical_path: entry.path.clone(),
                expected_hash: ContentHash::crc64_xz_decimal(&entry.hash)?,
                expected_size: Some(entry.size),
                on_fail: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let task_progress = TaskProgress::new(progress)
        .with_verify(ProgressLane::INTEGRITY_VERIFY, tasks.len())
        .with_download(ProgressLane::INTEGRITY_DOWNLOAD);
    let result = runner.run_batch(tasks, task_progress)?;
    summarize_result(result.outcomes, &result.metrics.graph, false)
}

fn summarize_result(
    outcomes_raw: Vec<TaskOutcome>,
    graph: &crate::runtime::task_pool::TaskGraphSummary,
    strict_failures: bool,
) -> Result<FileEnsureSummary> {
    let mut issues = BTreeMap::new();
    let mut proofs = Vec::new();
    let mut outcomes = PathOutcomeTracker::new();
    let mut failures = Vec::new();
    for outcome in outcomes_raw {
        match outcome {
            TaskOutcome::Committed { proof } => {
                issues.remove(proof.logical_path());
                outcomes.record_committed(&proof);
                proofs.push(proof);
            }
            TaskOutcome::Verified { path, ok, issue } => {
                outcomes.record_verified(&path, ok);
                if ok {
                    issues.remove(&path);
                } else if let Some(issue) = issue {
                    issues.insert(path, issue);
                }
            }
            TaskOutcome::Downloaded { path, bytes } => outcomes.record_downloaded(&path, bytes),
            TaskOutcome::Failed { path, reason } => {
                outcomes.record_failed(&path);
                failures.push(format!("{path}: {reason}"));
            }
            _ => {}
        }
    }
    if strict_failures
        && (graph.failed_nodes > 0 || graph.cancelled_nodes > 0 || !failures.is_empty())
    {
        return Err(Error::Message {
            context: "YoStar materialization error: ",
            detail: format!(
                "{} failed, {} cancelled graph nodes{}",
                graph.failed_nodes,
                graph.cancelled_nodes,
                if failures.is_empty() {
                    String::new()
                } else {
                    format!(": {}", failures.join("; "))
                }
            ),
        });
    }
    let counts = outcomes.summary();
    Ok(FileEnsureSummary {
        reused_files: counts.reused_files,
        downloaded_files: counts.downloaded_files,
        issues: issues.into_values().collect(),
        verified_artifacts: proofs,
    })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct YostarCleanupSummary {
    pub removed_files: usize,
    pub retained_modified_files: usize,
}

pub async fn cleanup_yostar_obsolete_files(
    install_path: &Path,
    current: &YostarManifest,
    target: &YostarManifest,
    blocking_only: bool,
) -> Result<YostarCleanupSummary> {
    planned_entries(current)?;
    planned_entries(target)?;
    let target_paths = target
        .files
        .iter()
        .map(|entry| normalize_logical_path(&entry.path))
        .collect::<BTreeSet<_>>();
    let mut summary = YostarCleanupSummary::default();
    let mut candidate_dirs = BTreeSet::new();
    for entry in &current.files {
        let old = normalize_logical_path(&entry.path);
        if target_paths.contains(&old) {
            continue;
        }
        let blocks = target_paths.iter().any(|target_path| {
            target_path.starts_with(&(old.clone() + "/"))
                || old.starts_with(&(target_path.clone() + "/"))
        });
        if blocking_only != blocks && blocking_only {
            continue;
        }
        if !blocking_only && blocks {
            continue;
        }
        let relative = crate::runtime::task_pool::fs_ops::path_safety::parse_safe_relative_path(
            "YoStar obsolete path",
            &entry.path,
        )?;
        let path = install_path.join(&relative);
        let expected = ContentHash::crc64_xz_decimal(&entry.hash)?;
        match crate::runtime::task_pool::verify::build_issue(
            &path,
            &entry.path,
            &expected,
            Some(entry.size),
        ) {
            None => {
                match compio::fs::remove_file(&path).await {
                    Ok(()) => summary.removed_files += 1,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(source) => {
                        return Err(Error::IoAt {
                            action: "remove obsolete YoStar file",
                            path: path.clone(),
                            source,
                        })
                    }
                }
                if let Some(parent) = relative.parent() {
                    candidate_dirs.insert(parent.to_path_buf());
                }
            }
            Some(issue) if issue.kind == crate::runtime::FileIssueKind::Missing => {}
            Some(_) if blocking_only => {
                return Err(Error::Message {
                    context: "YoStar update error: ",
                    detail: format!(
                        "modified obsolete file {} blocks the target layout; refusing to delete it",
                        entry.path
                    ),
                });
            }
            Some(_) => summary.retained_modified_files += 1,
        }
    }
    let mut dirs = candidate_dirs.into_iter().collect::<Vec<_>>();
    dirs.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for relative in dirs {
        let _ = crate::runtime::remove_empty_dir(install_path.join(relative)).await;
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::yostar::YostarManifestEntry;
    use crate::runtime::task_pool::{TaskPoolConfig, TaskPoolRunner};

    fn manifest() -> YostarManifest {
        YostarManifest {
            source: "files".to_string(),
            files: vec![YostarManifestEntry {
                path: "data.bin".to_string(),
                size: 9,
                hash: "11051210869376104954".to_string(),
            }],
        }
    }

    #[test]
    fn cdn_failover_returns_to_primary_after_backup() {
        let roots = vec!["primary".to_string(), "backup".to_string()];
        assert_eq!(
            provider_attempt_roots(&roots),
            vec![
                "primary".to_string(),
                "backup".to_string(),
                "primary".to_string()
            ]
        );
        assert_eq!(provider_attempt_roots(&[]), vec![String::new()]);
    }

    #[test]
    fn launch_metadata_check_catches_missing_or_wrong_size_without_hashing() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let temp = tempfile::tempdir().unwrap();
            std::fs::write(temp.path().join("data.bin"), b"abcdefghi").unwrap();
            let manifest = manifest();
            let mut runner = TaskPoolRunner::new(TaskPoolConfig::default()).unwrap();

            let summary = check_yostar_file_metadata_with_pool(
                temp.path(),
                &manifest,
                &mut runner,
                ProgressSender::disabled(),
            )
            .await
            .unwrap();
            assert!(
                summary.issues.is_empty(),
                "same-size corruption is a quick-check hit"
            );

            std::fs::write(temp.path().join("data.bin"), b"short").unwrap();
            let summary = check_yostar_file_metadata_with_pool(
                temp.path(),
                &manifest,
                &mut runner,
                ProgressSender::disabled(),
            )
            .await
            .unwrap();
            assert_eq!(summary.issues.len(), 1);
            assert_eq!(
                summary.issues[0].kind,
                crate::runtime::FileIssueKind::SizeMismatch
            );
        });
    }

    #[test]
    fn normal_update_trusts_same_manifest_size_but_full_verify_hashes() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let temp = tempfile::tempdir().unwrap();
            std::fs::write(temp.path().join("data.bin"), b"abcdefghi").unwrap();
            let manifest = manifest();
            let mut runner = TaskPoolRunner::new(TaskPoolConfig::default()).unwrap();
            let summary = ensure_yostar_files_with_pool(
                temp.path(),
                Some(&manifest),
                &manifest,
                &[],
                &[],
                false,
                false,
                false,
                &mut runner,
                ProgressSender::disabled(),
            )
            .await
            .unwrap();
            assert!(summary.issues.is_empty());

            let summary = verify_yostar_files_with_pool(
                temp.path(),
                &manifest,
                false,
                &[],
                &[],
                false,
                &mut runner,
                ProgressSender::disabled(),
            )
            .await
            .unwrap();
            assert_eq!(summary.issues.len(), 1);
            assert_eq!(
                summary.issues[0].kind,
                crate::runtime::FileIssueKind::HashMismatch
            );
        });
    }

    #[test]
    fn repair_can_reuse_crc64_verified_file_without_cdn() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let temp = tempfile::tempdir().unwrap();
            let source = tempfile::tempdir().unwrap();
            std::fs::write(temp.path().join("data.bin"), b"abcdefghi").unwrap();
            std::fs::write(source.path().join("data.bin"), b"123456789").unwrap();
            let manifest = manifest();
            let mut runner = TaskPoolRunner::new(TaskPoolConfig::default()).unwrap();
            let summary = verify_yostar_files_with_pool(
                temp.path(),
                &manifest,
                true,
                &[],
                &[source.path().to_path_buf()],
                true,
                &mut runner,
                ProgressSender::disabled(),
            )
            .await
            .unwrap();
            assert!(summary.issues.is_empty());
            assert_eq!(
                std::fs::read(temp.path().join("data.bin")).unwrap(),
                b"123456789"
            );
        });
    }
}
