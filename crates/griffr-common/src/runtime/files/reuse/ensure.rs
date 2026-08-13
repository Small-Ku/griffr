use std::path::{Path, PathBuf};

use rapidhash::{RapidHashMap as HashMap, RapidHashSet as HashSet};

use crate::api::types::GameFileEntry;
use crate::error::{Error, Result};
use crate::runtime::task_pool::types::{ArchiveRepairGroupSpec, ArchiveRepairSession};
use crate::runtime::task_pool::{
    archive_expected_files, plan_archive_groups, FileEnsureTask, Task, TransferClass,
};
use crate::runtime::{
    build_cdn_file_url, files_base_url, griffr_archives_path, is_griffr_private_path,
    is_launcher_metadata_path, normalize_logical_path, PathOutcomeTracker, ProgressLane,
    ProgressSender,
};
use tracing::{info, warn};

pub(super) fn normalized_relative_path(path: &Path) -> String {
    normalize_logical_path(&path.to_string_lossy())
}

pub(super) fn validated_game_file_entries(
    manifest: &[GameFileEntry],
) -> Result<Vec<(&GameFileEntry, PathBuf)>> {
    let mut seen = HashSet::default();
    seen.reserve(manifest.len());
    let mut validated = Vec::with_capacity(manifest.len());

    for entry in manifest {
        let relative = crate::runtime::task_pool::fs_ops::path_safety::parse_safe_relative_path(
            "target game_files entry",
            &entry.path,
        )?;
        if is_griffr_private_path(&relative) {
            return Err(Error::Message {
                context: "File reuse planning error: ",
                detail: format!(
                    "Target manifest cannot own private Griffr path {}",
                    entry.path
                ),
            });
        }
        let normalized = normalized_relative_path(&relative);
        if !seen.insert(normalized.clone()) {
            return Err(Error::Message {
                context: "File reuse planning error: ",
                detail: format!("Target manifest contains duplicate path {}", entry.path),
            });
        }
        validated.push((entry, relative, normalized));
    }

    for (entry, _, normalized) in &validated {
        let mut ancestor = normalized.as_str();
        while let Some(separator) = ancestor.rfind('/') {
            ancestor = &ancestor[..separator];
            if seen.contains(ancestor) {
                return Err(Error::Message {
                    context: "File reuse planning error: ",
                    detail: format!(
                        "Target manifest contains conflicting file/directory path {}",
                        entry.path
                    ),
                });
            }
        }
    }

    Ok(validated
        .into_iter()
        .filter_map(|(entry, relative, _)| {
            (!is_launcher_metadata_path(&relative.to_string_lossy())).then_some((entry, relative))
        })
        .collect())
}

pub async fn ensure_game_files_from_manifest_with_pool(
    install_path: &Path,
    file_path: &str,
    manifest: &[GameFileEntry],
    config: &super::types::FileMaterializationConfig,
    task_pool_runner: Option<&mut crate::runtime::task_pool::TaskPoolRunner>,
    progress: ProgressSender,
) -> Result<super::types::FileEnsureSummary> {
    ensure_game_files_impl(
        install_path,
        file_path,
        manifest,
        &HashSet::default(),
        config,
        task_pool_runner,
        progress,
    )
    .await
}

/// Normal-update ensure path. Entries whose expected MD5 and size are
/// unchanged from the locally committed manifest receive an existence/size
/// check only; changed/new entries retain the strong destination hash check.
pub async fn ensure_manifest_delta_with_pool(
    install_path: &Path,
    file_path: &str,
    current_manifest: &[GameFileEntry],
    target_manifest: &[GameFileEntry],
    config: &super::types::FileMaterializationConfig,
    task_pool_runner: Option<&mut crate::runtime::task_pool::TaskPoolRunner>,
    progress: ProgressSender,
) -> Result<super::types::FileEnsureSummary> {
    let mut current = HashMap::<String, (&str, u64)>::default();
    current.reserve(current_manifest.len());
    for entry in current_manifest {
        current.insert(
            normalize_logical_path(&entry.path),
            (entry.md5.as_str(), entry.size),
        );
    }

    let mut metadata_only = HashSet::default();
    metadata_only.reserve(target_manifest.len());
    for entry in target_manifest {
        let key = normalize_logical_path(&entry.path);
        if current
            .get(&key)
            .is_some_and(|(md5, size)| *size == entry.size && md5.eq_ignore_ascii_case(&entry.md5))
        {
            metadata_only.insert(key);
        }
    }

    info!(
        "Manifest delta: metadata_only={} strong_check={}",
        metadata_only.len(),
        target_manifest.len().saturating_sub(metadata_only.len())
    );
    ensure_game_files_impl(
        install_path,
        file_path,
        target_manifest,
        &metadata_only,
        config,
        task_pool_runner,
        progress,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn ensure_game_files_impl(
    install_path: &Path,
    file_path: &str,
    manifest: &[GameFileEntry],
    metadata_only_paths: &HashSet<String>,
    config: &super::types::FileMaterializationConfig,
    task_pool_runner: Option<&mut crate::runtime::task_pool::TaskPoolRunner>,
    progress: ProgressSender,
) -> Result<super::types::FileEnsureSummary> {
    let planned_entries = validated_game_file_entries(manifest)?;
    let files_url_base = files_base_url(file_path)?;
    let source_roots = &config.source_roots;
    let archive_session = if config.archive_packs.is_empty() || config.dry_run {
        None
    } else {
        let groups =
            plan_archive_groups(&config.archive_packs, &griffr_archives_path(install_path))?;
        (!groups.is_empty()).then(|| {
            ArchiveRepairSession::new(
                groups
                    .into_iter()
                    .map(|group| ArchiveRepairGroupSpec {
                        base_name: group.base_name,
                        parts: group.parts,
                    })
                    .collect(),
                install_path.to_path_buf(),
                archive_expected_files(planned_entries.iter().map(|(entry, _)| (*entry).clone())),
            )
        })
    };

    info!(
        "Materialization providers: reuse_roots={} archive_groups={} direct_cdn=true",
        source_roots.len(),
        archive_session
            .as_ref()
            .map_or(0, |session| session.group_specs().len())
    );

    let mut dry_run_reused = 0usize;
    let mut dry_run_downloaded = 0usize;
    let mut tasks = Vec::with_capacity(planned_entries.len());
    for (entry, relative) in planned_entries {
        let candidates = source_roots
            .iter()
            .map(|root| root.join(&relative))
            .collect::<Vec<_>>();
        let metadata_only = metadata_only_paths.contains(&normalize_logical_path(&entry.path));
        if config.dry_run {
            let destination_matches = if metadata_only {
                crate::runtime::task_pool::verify::build_metadata_issue(
                    &install_path.join(&relative),
                    &entry.path,
                    &entry.md5,
                    entry.size,
                )
                .is_none()
            } else {
                crate::runtime::task_pool::verify::build_issue(
                    &install_path.join(&relative),
                    &entry.path,
                    &entry.md5,
                    Some(entry.size),
                )
                .is_none()
            };
            let reusable_candidate_exists = candidates.iter().any(|path| {
                crate::runtime::task_pool::verify::build_issue(
                    path,
                    &entry.path,
                    &entry.md5,
                    Some(entry.size),
                )
                .is_none()
            });
            if !destination_matches && reusable_candidate_exists {
                dry_run_reused = dry_run_reused.saturating_add(1);
            } else if !destination_matches {
                dry_run_downloaded = dry_run_downloaded.saturating_add(1);
            }
        }

        let spec = FileEnsureTask {
            dest: install_path.join(&relative),
            logical_path: entry.path.clone(),
            expected_md5: entry.md5.clone(),
            expected_size: entry.size,
            source_candidates: candidates,
            download_url: Some(build_cdn_file_url(files_url_base, &entry.path)),
            allow_copy_fallback: config.allow_copy_fallback,
            copy_only: false,
            prefer_reuse: false,
            retry_count: 0,
            transfer_class: TransferClass::General,
            archive_repair: archive_session.clone(),
        };
        tasks.push(if config.skip_destination_check {
            Task::materialize_file(spec)
        } else if metadata_only {
            Task::ensure_file_metadata(spec)
        } else {
            Task::ensure_file(spec)
        });
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
            verified_artifacts: Vec::new(),
        });
    }

    let total = tasks.len();
    let task_progress = crate::runtime::task_pool::TaskProgress::new(progress)
        .with_verify(ProgressLane::FILE_ENSURE_VERIFY, total)
        .with_download(ProgressLane::FILE_ENSURE_DOWNLOAD);
    let run_result = if let Some(runner) = task_pool_runner {
        runner.run_batch(tasks, task_progress)
    } else {
        let pool_cfg = crate::runtime::task_pool::TaskPoolConfig::for_file_ensure();
        crate::runtime::task_pool::run_tasks_with_progress(tasks, pool_cfg, task_progress)
    };
    if let Some(session) = archive_session.as_ref() {
        session.cleanup();
    }
    let result = run_result.map_err(|error| Error::Message {
        context: "Task pool error: ",
        detail: format!("Game-file ensure pool failed: {error}"),
    })?;

    let failed_graph_nodes = result
        .metrics
        .graph
        .failed_nodes
        .saturating_add(result.metrics.graph.cancelled_nodes);
    let mut issues_by_path = HashMap::default();
    let mut verified_artifacts = Vec::new();
    let mut failed_paths = Vec::new();
    let mut outcomes = PathOutcomeTracker::new();
    for event in result.outcomes {
        match event {
            crate::runtime::task_pool::TaskOutcome::Committed { proof } => {
                outcomes.record_committed(&proof);
                issues_by_path.remove(proof.logical_path());
                verified_artifacts.push(proof);
            }
            crate::runtime::task_pool::TaskOutcome::Verified { path, ok, issue } => {
                outcomes.record_verified(&path, ok);
                if ok {
                    issues_by_path.remove(&path);
                } else if let Some(issue) = issue {
                    issues_by_path.insert(path, issue);
                }
            }
            crate::runtime::task_pool::TaskOutcome::Downloaded { path, bytes } => {
                outcomes.record_downloaded(&path, bytes);
            }
            crate::runtime::task_pool::TaskOutcome::Failed { path, reason } => {
                outcomes.record_failed(&path);
                warn!("Failed to ensure game file {}: {}", path, reason);
                failed_paths.push(format!("{path}: {reason}"));
            }
            _ => {}
        }
    }
    if !failed_paths.is_empty() {
        return Err(Error::Message {
            context: "File materialization error: ",
            detail: format!(
                "{} game-file ensure task(s) failed: {}",
                failed_paths.len(),
                failed_paths.join("; ")
            ),
        });
    }

    let summary = outcomes.summary();
    if failed_graph_nodes > 0 || summary.failed_files > 0 {
        return Err(Error::Message {
            context: "Task pool error: ",
            detail: format!(
                "Game-file ensure left {} failed or cancelled graph node(s) and {} failed file(s)",
                failed_graph_nodes, summary.failed_files
            ),
        });
    }
    let mut issues = issues_by_path.into_values().collect::<Vec<_>>();
    issues.sort_by(|left, right| left.path.cmp(&right.path));

    info!(
        "Game-file ensure finished: reused={} downloaded={} issues={}",
        summary.reused_files,
        summary.downloaded_files,
        issues.len()
    );

    Ok(super::types::FileEnsureSummary {
        reused_files: summary.reused_files,
        downloaded_files: summary.downloaded_files,
        issues,
        verified_artifacts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str) -> GameFileEntry {
        GameFileEntry {
            path: path.to_string(),
            md5: "00".to_string(),
            size: 1,
        }
    }

    #[test]
    fn planned_entries_reject_manifest_escape_and_private_paths() {
        let error = validated_game_file_entries(&[entry("../outside.bin")]).unwrap_err();
        assert!(error.to_string().contains("Invalid path"));

        let error = validated_game_file_entries(&[entry(".griffr/state.json")]).unwrap_err();
        assert!(error.to_string().contains("private Griffr path"));
    }

    #[test]
    fn planned_entries_reject_equivalent_paths() {
        for alias in ["data\\FILE.bin", "Data//file.bin", "Data/./file.bin"] {
            let error =
                validated_game_file_entries(&[entry("Data/file.bin"), entry(alias)]).unwrap_err();
            assert!(error.to_string().contains("duplicate path"));
        }
    }

    #[test]
    fn planned_entries_reject_file_directory_conflicts() {
        for manifest in [
            vec![entry("Data"), entry("Data/file.bin")],
            vec![entry("Data/file.bin"), entry("Data")],
        ] {
            let error = validated_game_file_entries(&manifest).unwrap_err();
            assert!(error.to_string().contains("file/directory path"));
        }
    }

    #[test]
    fn planned_entries_allow_component_prefix_siblings() {
        assert!(validated_game_file_entries(&[entry("Data"), entry("Database/file.bin")]).is_ok());
    }
}
