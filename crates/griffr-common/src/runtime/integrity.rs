use rapidhash::{RapidHashMap as HashMap, RapidHashSet as HashSet};
use std::path::{Path, PathBuf};

use crate::api::ApiClient;
use crate::config::InstallProfile;
use crate::error::{Error, Result};
use crate::runtime::task_pool::{
    run_tasks_with_progress, ProgressEvent, Task, TaskPoolConfig, TaskPoolRunner,
};
use crate::runtime::{
    build_cdn_file_url, FileIssue, PathOutcomeTracker, PathReuseMethod, RunningByteProgress,
};

#[derive(Debug, Clone, Default)]
pub struct IntegrityRunSummary {
    pub issues: Vec<FileIssue>,
    pub verified_files: usize,
    pub downloaded_files: usize,
    pub reused_files: usize,
}

fn normalize_progress_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
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
        | Task::EnsureFile { logical_path, .. } => Some(logical_path.as_str()),
        _ => None,
    }
}

fn task_expected_bytes(task: &Task) -> u64 {
    match task {
        Task::Download { expected_size, .. } => expected_size.unwrap_or(0),
        Task::EnsureFile { expected_size, .. } => *expected_size,
        _ => 0,
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run_integrity_pool(
    api_client: &ApiClient,
    install_path: &Path,
    profile: &InstallProfile,
    version: Option<&str>,
    repair: bool,
    source_roots: &[PathBuf],
    allow_copy_fallback: bool,
    prefer_reuse: bool,
    extra_tasks: Vec<Task>,
    task_pool_runner: Option<&mut TaskPoolRunner>,
    progress_callback: Option<impl Fn(usize, usize, &str)>,
    download_progress_callback: Option<impl Fn(u64, u64, &str)>,
) -> Result<IntegrityRunSummary> {
    let version_info = api_client.get_latest_game(&profile.target, version).await?;

    let pkg = version_info
        .pkg
        .as_ref()
        .ok_or_else(|| Error::ApiClient("No package information available".to_string()))?;

    let entries = api_client
        .fetch_game_files(&pkg.file_path, pkg.game_files_md5.as_deref())
        .await?;
    let files_base_url = pkg.file_path.trim_end_matches("/game_files");

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
    let tracked_total_bytes: u64 = entries
        .iter()
        .map(|entry| entry.size)
        .sum::<u64>()
        .saturating_add(extra_tasks.iter().map(task_expected_bytes).sum::<u64>());

    let mut tasks = entries
        .iter()
        .map(|entry| {
            if repair {
                let source_candidates = source_roots
                    .iter()
                    .map(|root| root.join(&entry.path))
                    .collect::<Vec<_>>();
                Task::EnsureFile {
                    dest: install_path.join(&entry.path),
                    logical_path: entry.path.clone(),
                    expected_md5: entry.md5.clone(),
                    expected_size: entry.size,
                    source_candidates,
                    download_url: Some(build_cdn_file_url(files_base_url, &entry.path)),
                    allow_copy_fallback,
                    prefer_reuse,
                    retry_count: 0,
                }
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
    let total = tasks.len();
    let mut issues = Vec::new();
    let mut finished = 0usize;
    let mut download_progress = RunningByteProgress::new();
    let mut outcomes = PathOutcomeTracker::new();
    let mut failed_paths = Vec::new();

    let mut on_event = |event: &ProgressEvent| match event {
        ProgressEvent::Verified { path, ok, issue } => {
            if !tracked_paths.contains(path) && !extra_tracked_paths.contains(path) {
                return;
            }
            if let Some(ref cb) = progress_callback {
                cb(finished, total, path);
            }
            finished += 1;
            outcomes.record_verified(path, *ok);
            if tracked_paths.contains(path) {
                if let Some(issue) = issue.clone() {
                    issues.push(issue);
                }
            }
        }
        ProgressEvent::DownloadedBytes { path, .. } => {
            if !tracked_paths.contains(path) && !extra_tracked_paths.contains(path) {
                return;
            }
            let running_downloaded_bytes = download_progress
                .handle_download_event(event)
                .expect("download event");
            if let Some(ref cb) = download_progress_callback {
                cb(running_downloaded_bytes, tracked_total_bytes, path);
            }
        }
        ProgressEvent::Downloaded { path, bytes } => {
            if !tracked_paths.contains(path) && !extra_tracked_paths.contains(path) {
                return;
            }
            outcomes.record_downloaded(path, *bytes);
            let running_downloaded_bytes = download_progress
                .handle_download_event(event)
                .expect("download event");
            if let Some(ref cb) = progress_callback {
                cb(finished, total, path);
            }
            if let Some(ref cb) = download_progress_callback {
                cb(running_downloaded_bytes, tracked_total_bytes, path);
            }
        }
        ProgressEvent::Hardlinked { path } | ProgressEvent::Copied { path } => {
            if let Some(logical_path) = resolve_reused_logical_path(path, &filename_index) {
                outcomes.record_reused(
                    &logical_path,
                    if matches!(event, ProgressEvent::Hardlinked { .. }) {
                        PathReuseMethod::Hardlink
                    } else {
                        PathReuseMethod::Copy
                    },
                );
            }
        }
        ProgressEvent::Retried { path, reason } => {
            tracing::debug!("retrying {}: {}", path, reason);
            if let Some(ref cb) = progress_callback {
                cb(finished, total, path);
            }
        }
        ProgressEvent::Failed { path, reason } => {
            tracing::warn!("integrity task failed for {}: {}", path, reason);
            failed_paths.push(format!("{path}: {reason}"));
        }
        _ => {}
    };

    if let Some(runner) = task_pool_runner {
        let _ = runner.run_batch_with_progress(tasks, Some(&mut on_event))?;
    } else {
        let _ = run_tasks_with_progress(tasks, TaskPoolConfig::default(), Some(&mut on_event))?;
    }
    if !failed_paths.is_empty() {
        return Err(Error::Integrity(format!(
            "{} integrity task(s) failed: {}",
            failed_paths.len(),
            failed_paths.join("; ")
        )));
    }

    let summary = outcomes.summary();
    Ok(IntegrityRunSummary {
        issues,
        verified_files: finished,
        downloaded_files: summary.downloaded_files,
        reused_files: summary.reused_files,
    })
}
