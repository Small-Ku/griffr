use rapidhash::{RapidHashMap as HashMap, RapidHashSet as HashSet};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::runtime::task_pool::types::{ArchiveRepairGroupSpec, ArchiveRepairSession};
use crate::runtime::task_pool::{
    archive_expected_files, plan_archive_groups, run_tasks_with_progress, FileEnsureTask, Task,
    TaskOutcome, TaskPoolConfig, TaskPoolRunner, TaskProgress, TransferClass,
};
use crate::runtime::{
    build_cdn_file_url, files_base_url, griffr_archives_path, is_launcher_metadata_path,
    is_resource_baseline_path, normalize_logical_path, ArtifactClaim, ArtifactProof, ContentPlan,
    FileIssue, PathOutcomeTracker, ProgressLane, ProgressSender,
};

#[derive(Debug, Clone, Default)]
pub struct IntegrityRunSummary {
    pub issues: Vec<FileIssue>,
    pub verified_files: usize,
    pub downloaded_files: usize,
    pub reused_files: usize,
}

/// Chooses whether integrity reads the full manifest or only paths known
/// to have been committed by the current work.
#[derive(Debug, Clone)]
pub enum IntegritySelection {
    Full,
    /// Checks every game payload file but leaves launcher-owned metadata for
    /// the final metadata sync step. This keeps `config.ini` as a finish marker.
    GameFiles,
    /// Checks game manifest entries outside the launcher resource baseline.
    Core,
    /// Checks only launcher resource baseline entries plus planned resource tasks.
    Resources,
    Paths(Vec<String>),
}

fn deduplicate_target_tasks(tasks: Vec<Task>) -> Result<Vec<Task>> {
    let mut targets = HashMap::<String, (String, Option<u64>)>::default();
    let mut unique = Vec::with_capacity(tasks.len());
    for task in tasks {
        let Some(file) = task.final_file() else {
            unique.push(task);
            continue;
        };
        let target = crate::runtime::artifact::physical_path_key(file.target());
        let expected = (
            file.expected_md5().to_ascii_lowercase(),
            file.expected_size(),
        );
        if let Some(previous) = targets.get(&target) {
            if previous != &expected {
                return Err(Error::Message {
                    context: "Integrity error: ",
                    detail: format!(
                        "conflicting integrity tasks target {} with different expected content",
                        file.target().display()
                    ),
                });
            }
            continue;
        }
        targets.insert(target, expected);
        unique.push(task);
    }
    Ok(unique)
}

fn remove_launcher_metadata_entries(entries: &mut Vec<crate::api::types::GameFileEntry>) {
    entries.retain(|entry| !is_launcher_metadata_path(&entry.path));
}

fn remove_already_verified_entries(
    entries: &mut Vec<crate::api::types::GameFileEntry>,
    install_root: &Path,
    proofs: &[ArtifactProof],
) {
    if proofs.is_empty() {
        return;
    }
    let current_proofs = proofs
        .iter()
        .filter(|proof| proof.is_current())
        .collect::<Vec<_>>();
    entries.retain(|entry| {
        !current_proofs
            .iter()
            .any(|proof| proof.matches_game_file(install_root, entry))
    });
}

fn remove_entries_owned_by_claims(
    entries: &mut Vec<crate::api::types::GameFileEntry>,
    install_path: &Path,
    claims: &[ArtifactClaim],
) -> Result<()> {
    if claims.is_empty() {
        return Ok(());
    }

    let mut conflict = None;
    entries.retain(|entry| {
        let Some(claim) = claims
            .iter()
            .find(|claim| claim.matches_game_file_path(install_path, entry))
        else {
            return true;
        };
        let is_res_baseline = is_resource_baseline_path(&entry.path)
            || is_resource_baseline_path(claim.expectation().logical_path());
        if !is_res_baseline && !claim.expects_game_file(entry) {
            conflict = Some(format!(
                "resource and game manifests claim {} with different expected content",
                entry.path
            ));
        }
        false
    });

    if let Some(detail) = conflict {
        return Err(Error::Message {
            context: "Integrity error: ",
            detail,
        });
    }
    Ok(())
}

fn task_progress_path(task: &Task) -> Option<&str> {
    task.logical_path()
}

#[allow(clippy::too_many_arguments)]
pub async fn run_integrity_pool(
    content_plan: &ContentPlan,
    selection: IntegritySelection,
    verified_artifacts: &[ArtifactProof],
    repair: bool,
    source_roots: &[PathBuf],
    allow_copy_fallback: bool,
    prefer_reuse: bool,
    extra_tasks: Vec<Task>,
    task_pool_runner: Option<&mut TaskPoolRunner>,
    progress: ProgressSender,
) -> Result<IntegrityRunSummary> {
    let extra_tasks = deduplicate_target_tasks(extra_tasks)?;
    let (selected_paths, skip_launcher_metadata, resource_filter) = match selection {
        IntegritySelection::Full => (None, false, None),
        IntegritySelection::GameFiles => (None, true, None),
        IntegritySelection::Core => (None, true, Some(false)),
        IntegritySelection::Resources => (None, false, Some(true)),
        IntegritySelection::Paths(paths) => (
            Some(
                paths
                    .into_iter()
                    .map(|path| normalize_logical_path(&path))
                    .filter(|path| !path.is_empty() && path != ".")
                    .collect::<HashSet<_>>(),
            ),
            false,
            None,
        ),
    };

    let install_path = content_plan.install_root();
    let snapshot = content_plan.snapshot();
    let (entries, files_url_base, archive_groups) = if selected_paths
        .as_ref()
        .is_some_and(|paths| paths.is_empty())
    {
        (Vec::new(), None, None)
    } else {
        let pkg = &snapshot.package;
        let mut entries = snapshot.entries.clone();
        if skip_launcher_metadata {
            remove_launcher_metadata_entries(&mut entries);
        }
        if let Some(resources_only) = resource_filter {
            entries.retain(|entry| is_resource_baseline_path(&entry.path) == resources_only);
        }
        if let Some(paths) = selected_paths.as_ref() {
            entries.retain(|entry| paths.contains(&normalize_logical_path(&entry.path)));
        }
        remove_already_verified_entries(&mut entries, install_path, verified_artifacts);
        remove_entries_owned_by_claims(&mut entries, install_path, content_plan.resource_claims())?;
        let archive_groups = (repair && !pkg.packs.is_empty())
            .then(|| plan_archive_groups(&pkg.packs, &griffr_archives_path(install_path)))
            .transpose()?
            .filter(|groups| !groups.is_empty());
        (
            entries,
            repair
                .then(|| files_base_url(&pkg.file_path).map(str::to_owned))
                .transpose()?,
            archive_groups,
        )
    };

    let archive_repair = archive_groups.map(|groups| {
        ArchiveRepairSession::new(
            groups
                .into_iter()
                .map(|group| ArchiveRepairGroupSpec {
                    base_name: group.base_name,
                    parts: group.parts,
                })
                .collect(),
            install_path.to_path_buf(),
            archive_expected_files(entries.clone()),
        )
    });

    let tracked_paths = entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<HashSet<_>>();
    let extra_tracked_paths = extra_tasks
        .iter()
        .filter_map(task_progress_path)
        .map(str::to_owned)
        .collect::<HashSet<_>>();

    let mut tasks = entries
        .iter()
        .map(|entry| {
            if repair {
                let source_candidates = source_roots
                    .iter()
                    .map(|root| root.join(&entry.path))
                    .collect::<Vec<_>>();
                Task::ensure_file(FileEnsureTask {
                    dest: install_path.join(&entry.path),
                    logical_path: entry.path.clone(),
                    expected_md5: entry.md5.clone(),
                    expected_size: entry.size,
                    source_candidates,
                    download_url: files_url_base
                        .as_ref()
                        .map(|base| build_cdn_file_url(base, &entry.path)),
                    allow_copy_fallback,
                    copy_only: false,
                    prefer_reuse,
                    retry_count: 0,
                    transfer_class: TransferClass::General,
                    archive_repair: archive_repair.clone(),
                })
            } else {
                Task::Verify {
                    path: install_path.join(&entry.path),
                    logical_path: entry.path.clone(),
                    expected_md5: entry.md5.clone(),
                    expected_size: Some(entry.size),
                    on_fail: None,
                }
            }
        })
        .collect::<Vec<_>>();
    tasks.extend(extra_tasks);
    if tasks.is_empty() {
        return Ok(IntegrityRunSummary::default());
    }

    let total = tasks.len();
    let task_progress = TaskProgress::new(progress)
        .with_verify(ProgressLane::INTEGRITY_VERIFY, total)
        .with_download(ProgressLane::INTEGRITY_DOWNLOAD);
    let result = if let Some(session) = archive_repair.as_ref() {
        let run_result = if let Some(runner) = task_pool_runner {
            runner.run_batch(tasks, task_progress)
        } else {
            run_tasks_with_progress(tasks, TaskPoolConfig::default(), task_progress)
        };
        session.cleanup();
        run_result?
    } else if let Some(runner) = task_pool_runner {
        runner.run_batch(tasks, task_progress)?
    } else {
        run_tasks_with_progress(tasks, TaskPoolConfig::default(), task_progress)?
    };

    let mut issues_by_path = HashMap::default();
    let mut finished_paths = HashSet::default();
    let mut outcomes = PathOutcomeTracker::new();
    let mut failed_paths = Vec::new();
    for event in result.outcomes {
        match event {
            TaskOutcome::Verified { path, ok, issue } => {
                if !tracked_paths.contains(&path) && !extra_tracked_paths.contains(&path) {
                    continue;
                }
                finished_paths.insert(path.clone());
                outcomes.record_verified(&path, ok);
                if tracked_paths.contains(&path) {
                    if let Some(issue) = issue {
                        issues_by_path.insert(path, issue);
                    } else if ok {
                        issues_by_path.remove(&path);
                    }
                }
            }
            TaskOutcome::Committed { proof } => {
                let path = proof.logical_path().to_string();
                if !tracked_paths.contains(&path) && !extra_tracked_paths.contains(&path) {
                    continue;
                }
                outcomes.record_committed(&proof);
                finished_paths.insert(path.clone());
                if tracked_paths.contains(&path) {
                    issues_by_path.remove(&path);
                }
            }
            TaskOutcome::Downloaded { path, bytes }
                if tracked_paths.contains(&path) || extra_tracked_paths.contains(&path) =>
            {
                outcomes.record_downloaded(&path, bytes);
            }
            TaskOutcome::Failed { path, reason } => {
                tracing::warn!("integrity task failed for {}: {}", path, reason);
                failed_paths.push(format!("{path}: {reason}"));
            }
            _ => {}
        }
    }
    let failed_graph_nodes = result
        .metrics
        .graph
        .failed_nodes
        .saturating_add(result.metrics.graph.cancelled_nodes);
    if failed_graph_nodes > 0 || !failed_paths.is_empty() {
        return Err(Error::Message {
            context: "Integrity error: ",
            detail: format!(
                "{} integrity task failure(s) and {} failed or cancelled graph node(s): {}",
                failed_paths.len(),
                failed_graph_nodes,
                failed_paths.join("; ")
            ),
        });
    }

    let summary = outcomes.summary();
    let mut issues = issues_by_path.into_values().collect::<Vec<_>>();
    issues.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(IntegrityRunSummary {
        issues,
        verified_files: finished_paths.len(),
        downloaded_files: summary.downloaded_files,
        reused_files: summary.reused_files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::GameFileEntry;

    fn verify_task(path: &str, md5: &str, size: u64) -> Task {
        Task::Verify {
            path: PathBuf::from(path),
            logical_path: path.replace('\\', "/"),
            expected_md5: md5.to_string(),
            expected_size: Some(size),
            on_fail: None,
        }
    }

    #[test]
    fn duplicate_identical_physical_targets_are_collapsed() {
        let tasks = vec![
            verify_task("root/VFS/file.blc", "00", 4),
            verify_task("ROOT\\vfs\\file.blc", "00", 4),
        ];

        let unique = deduplicate_target_tasks(tasks).unwrap();

        assert_eq!(unique.len(), 1);
    }

    #[test]
    fn conflicting_physical_targets_are_rejected() {
        let tasks = vec![
            verify_task("root/VFS/file.blc", "00", 4),
            verify_task("ROOT\\vfs\\file.blc", "11", 4),
        ];

        let error = deduplicate_target_tasks(tasks).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("conflicting integrity tasks target"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn game_file_scope_omits_launcher_metadata() {
        let mut entries = vec![
            GameFileEntry {
                path: "config.ini".to_string(),
                md5: "config".to_string(),
                size: 1,
            },
            GameFileEntry {
                path: "game_files".to_string(),
                md5: "manifest".to_string(),
                size: 2,
            },
            GameFileEntry {
                path: "package_files".to_string(),
                md5: "package".to_string(),
                size: 3,
            },
            GameFileEntry {
                path: "Data/game.bin".to_string(),
                md5: "game".to_string(),
                size: 8,
            },
        ];

        remove_launcher_metadata_entries(&mut entries);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "Data/game.bin");
    }

    #[test]
    fn current_matching_artifact_proof_removes_manifest_entry() {
        use md5::{Digest, Md5};

        let temp = tempfile::tempdir().unwrap();
        let install_root = temp.path().join("install");
        let payload = b"game-bin";
        let md5 = crate::to_hex(&Md5::digest(payload));
        let path = install_root.join("Data/game.bin");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, payload).unwrap();
        let proof = crate::runtime::ArtifactProof::from_verified_path(
            &path,
            crate::runtime::ArtifactExpectation::new(
                "Data/game.bin",
                &md5,
                Some(payload.len() as u64),
            ),
            crate::runtime::ArtifactSource::Archive,
        )
        .unwrap();
        let mut entries = vec![
            GameFileEntry {
                path: "Data/game.bin".to_string(),
                md5,
                size: payload.len() as u64,
            },
            GameFileEntry {
                path: "Data/other.bin".to_string(),
                md5: "other".to_string(),
                size: 4,
            },
        ];

        remove_already_verified_entries(&mut entries, &install_root, &[proof]);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "Data/other.bin");
    }

    #[test]
    fn stale_artifact_proof_does_not_remove_manifest_entry() {
        use md5::{Digest, Md5};

        let temp = tempfile::tempdir().unwrap();
        let install_root = temp.path().join("install");
        let payload = b"game-bin";
        let md5 = crate::to_hex(&Md5::digest(payload));
        let path = install_root.join("Data/game.bin");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, payload).unwrap();
        let proof = crate::runtime::ArtifactProof::from_verified_path(
            &path,
            crate::runtime::ArtifactExpectation::new(
                "Data/game.bin",
                &md5,
                Some(payload.len() as u64),
            ),
            crate::runtime::ArtifactSource::Archive,
        )
        .unwrap();
        std::fs::write(&path, b"externally changed bytes").unwrap();
        let mut entries = vec![GameFileEntry {
            path: "Data/game.bin".to_string(),
            md5,
            size: payload.len() as u64,
        }];

        remove_already_verified_entries(&mut entries, &install_root, &[proof]);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "Data/game.bin");
    }

    #[test]
    fn resource_claim_owns_overlapping_game_manifest_destination() {
        let install_path = Path::new("install");
        let claims = vec![ArtifactClaim::new(
            install_path.join("VFS/file.blc"),
            crate::runtime::ArtifactExpectation::new("file.blc", "base", Some(4)),
        )];
        let mut entries = vec![
            GameFileEntry {
                path: "VFS/file.blc".to_string(),
                md5: "base".to_string(),
                size: 4,
            },
            GameFileEntry {
                path: "game.bin".to_string(),
                md5: "game".to_string(),
                size: 8,
            },
        ];

        remove_entries_owned_by_claims(&mut entries, install_path, &claims).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "game.bin");
    }

    #[test]
    fn conflicting_resource_claim_is_rejected_before_integrity_tasks() {
        let install_path = Path::new("install");
        let claims = vec![ArtifactClaim::new(
            install_path.join("VFS/file.blc"),
            crate::runtime::ArtifactExpectation::new("file.blc", "resource", Some(4)),
        )];
        let mut entries = vec![GameFileEntry {
            path: "VFS/file.blc".to_string(),
            md5: "game".to_string(),
            size: 4,
        }];

        let error =
            remove_entries_owned_by_claims(&mut entries, install_path, &claims).unwrap_err();

        assert!(error.to_string().contains("different expected content"));
    }
}
