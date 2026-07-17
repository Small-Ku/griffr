use rapidhash::{RapidHashMap as HashMap, RapidHashSet as HashSet};
use std::path::{Path, PathBuf};

use crate::api::ApiClient;
use crate::config::InstallTarget;
use crate::error::{Error, Result};
use crate::runtime::task_pool::{
    run_tasks_with_progress, FileEnsureTask, Task, TaskOutcome, TaskPoolConfig, TaskPoolRunner,
    TaskProgress, TransferClass,
};
use crate::runtime::{
    build_cdn_file_url, files_base_url, normalize_logical_path, FileIssue, PathOutcomeTracker,
    PathReuseMethod, ProgressLane, ProgressSender,
};

#[derive(Debug, Clone, Default)]
pub struct IntegrityRunSummary {
    pub issues: Vec<FileIssue>,
    pub verified_files: usize,
    pub downloaded_files: usize,
    pub reused_files: usize,
}

/// Chooses whether integrity reads the complete manifest or only paths known
/// to have been committed by the current operation.
#[derive(Debug, Clone)]
pub enum IntegritySelection {
    Full,
    Paths(Vec<String>),
}

fn normalize_progress_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn normalize_target_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn task_target_path(task: &Task) -> Option<&Path> {
    match task {
        Task::Download { dest, .. }
        | Task::RepairFile { dest, .. }
        | Task::ReuseFile { dest, .. } => Some(dest.as_path()),
        Task::Verify { path, .. } => Some(path.as_path()),
        _ => None,
    }
}

fn resolve_reused_logical_path(
    path: &Path,
    filename_index: &HashMap<String, Vec<String>>,
) -> Option<String> {
    let normalized = normalize_progress_path(path);
    let filename = path.file_name()?.to_str()?;
    let candidates = filename_index.get(filename)?;
    candidates
        .iter()
        .find(|candidate| normalized.ends_with(candidate.as_str()))
        .cloned()
}

fn task_progress_path(task: &Task) -> Option<&str> {
    match task {
        Task::Download { logical_path, .. }
        | Task::Verify { logical_path, .. }
        | Task::RepairFile { logical_path, .. }
        | Task::ReuseFile { logical_path, .. } => Some(logical_path.as_str()),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run_integrity_pool(
    api_client: &ApiClient,
    install_path: &Path,
    install_target: &InstallTarget,
    version: Option<&str>,
    selection: IntegritySelection,
    repair: bool,
    source_roots: &[PathBuf],
    allow_copy_fallback: bool,
    prefer_reuse: bool,
    extra_tasks: Vec<Task>,
    task_pool_runner: Option<&mut TaskPoolRunner>,
    progress: ProgressSender,
) -> Result<IntegrityRunSummary> {
    let incremental_selection = matches!(&selection, IntegritySelection::Paths(_));
    let extra_target_paths = if incremental_selection {
        extra_tasks
            .iter()
            .filter_map(task_target_path)
            .map(normalize_target_path)
            .collect::<HashSet<_>>()
    } else {
        HashSet::default()
    };
    let selected_paths = match selection {
        IntegritySelection::Full => None,
        IntegritySelection::Paths(paths) => Some(
            paths
                .into_iter()
                .map(|path| normalize_logical_path(&path))
                .filter(|path| !path.is_empty() && path != ".")
                .collect::<HashSet<_>>(),
        ),
    };

    let (entries, files_url_base) = if selected_paths
        .as_ref()
        .is_some_and(|paths| paths.is_empty())
    {
        (Vec::new(), None)
    } else {
        let version_info = api_client
            .get_latest_game(&install_target.api, version)
            .await?;
        let pkg = version_info
            .pkg
            .as_ref()
            .ok_or_else(|| Error::ApiClient("No package information available".to_string()))?;
        let mut entries = api_client
            .fetch_game_files(&pkg.file_path, pkg.game_files_md5.as_deref())
            .await?;
        if let Some(paths) = selected_paths.as_ref() {
            entries.retain(|entry| paths.contains(&normalize_logical_path(&entry.path)));
        }
        if incremental_selection && !extra_target_paths.is_empty() {
            entries.retain(|entry| {
                let target = install_path.join(&entry.path);
                !extra_target_paths.contains(&normalize_target_path(&target))
            });
        }
        (
            entries,
            repair
                .then(|| files_base_url(&pkg.file_path).map(str::to_owned))
                .transpose()?,
        )
    };

    let tracked_paths = entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<HashSet<_>>();
    let extra_tracked_paths = extra_tasks
        .iter()
        .filter_map(task_progress_path)
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    let mut filename_index = HashMap::default();
    for path in tracked_paths.iter().chain(extra_tracked_paths.iter()) {
        if let Some(filename) = Path::new(path).file_name().and_then(|f| f.to_str()) {
            filename_index
                .entry(filename.to_string())
                .or_insert_with(Vec::new)
                .push(path.clone());
        }
    }

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
                    prefer_reuse,
                    retry_count: 0,
                    transfer_class: TransferClass::General,
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
    let result = if let Some(runner) = task_pool_runner {
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
            TaskOutcome::Downloaded { path, bytes }
                if tracked_paths.contains(&path) || extra_tracked_paths.contains(&path) =>
            {
                outcomes.record_downloaded(&path, bytes);
            }
            TaskOutcome::Hardlinked { path } => {
                if let Some(logical_path) = resolve_reused_logical_path(&path, &filename_index) {
                    outcomes.record_reused(&logical_path, PathReuseMethod::Hardlink);
                }
            }
            TaskOutcome::Copied { path } => {
                if let Some(logical_path) = resolve_reused_logical_path(&path, &filename_index) {
                    outcomes.record_reused(&logical_path, PathReuseMethod::Copy);
                }
            }
            TaskOutcome::Failed { path, reason } => {
                tracing::warn!("integrity task failed for {}: {}", path, reason);
                failed_paths.push(format!("{path}: {reason}"));
            }
            _ => {}
        }
    }
    if !failed_paths.is_empty() {
        return Err(Error::Integrity(format!(
            "{} integrity task(s) failed: {}",
            failed_paths.len(),
            failed_paths.join("; ")
        )));
    }

    let summary = outcomes.summary();
    let mut issues = issues_by_path
        .into_values()
        .collect::<Vec<_>>();
    issues.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(IntegrityRunSummary {
        issues,
        verified_files: finished_paths.len(),
        downloaded_files: summary.downloaded_files,
        reused_files: summary.reused_files,
    })
}
