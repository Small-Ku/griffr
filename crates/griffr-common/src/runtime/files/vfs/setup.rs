use crate::error::{Error, Result};
use rapidhash::RapidHashSet as HashSet;
use std::path::Path;

use crate::api::client::ApiClient;
use crate::api::crypto::RES_INDEX_KEY;
use crate::api::protocol::DEFAULT_PLATFORM;
use crate::config::ApiTarget;
use crate::runtime::task_pool::{
    FileEnsureTask, Task, TaskOutcome, TaskPoolRunner, TaskProgress, TransferClass,
};
use crate::runtime::{
    collect_files_recursive, logical_path_from_root, normalize_logical_path, path_is_dir,
    path_is_file, remove_empty_dirs_recursive, resource_manifest_filename, resource_manifest_url,
    vfs_path, PathOutcomeTracker, PathReuseMethod, ProgressLane, ProgressSender,
    ResourceManifestKind, RESOURCE_GROUP_BASE, RESOURCE_GROUP_MAIN,
};

#[derive(Debug, Clone, Default)]
pub struct VfsFilePlanOptions {
    /// Candidate StreamingAssets roots from other installs for VFS file reuse.
    pub source_streaming_assets: Vec<std::path::PathBuf>,
    /// Allow invalid destinations to be repaired by reuse or download.
    pub allow_repair: bool,
    /// Allow copy fallback when hardlinking from source installs fails.
    pub allow_copy_fallback: bool,
    /// Prefer relinking from reuse sources even when local files already verify.
    pub prefer_reuse: bool,
}

/// Result of a VFS resource check/download work
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
pub enum PersistentVfsFileSet {
    /// Use only the base `pref_initial` file set in Persistent.
    Base,
    /// Use the base `pref_initial` and main `pref_main` file sets in Persistent.
    All,
}

impl PersistentVfsFileSet {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Base => RESOURCE_GROUP_BASE,
            Self::All => "all",
        }
    }
}

impl std::fmt::Display for PersistentVfsFileSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for PersistentVfsFileSet {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            RESOURCE_GROUP_BASE => Ok(Self::Base),
            value if value == Self::All.as_str() => Ok(Self::All),
            other => Err(Error::Message {
                context: "Configuration error: ",
                detail: format!(
                    "invalid Persistent VFS file set {other:?}: expected {} or {}",
                    Self::Base.as_str(),
                    Self::All.as_str()
                ),
            }),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PersistentVfsConfig {
    /// File set to write in Persistent.
    pub file_set: PersistentVfsFileSet,
    /// Primary StreamingAssets root for local file reuse.
    pub source_streaming_assets: std::path::PathBuf,
    /// Other StreamingAssets roots that can supply files.
    pub extra_source_streaming_assets: Vec<std::path::PathBuf>,
    /// Allow copy fallback when hardlinking fails.
    pub allow_copy_fallback: bool,
    /// Prefer relinking from source candidates even when destination already verifies.
    pub prefer_reuse: bool,
    /// Allow downloading missing files from CDN when not found in source roots.
    pub allow_download: bool,
    /// Remove files under Persistent/VFS that are not in the selected file set.
    pub prune_extra_files: bool,
}

#[derive(Debug, Clone)]
pub struct PersistentVfsManifestDownload {
    pub url: String,
    pub filename: String,
}

#[derive(Debug, Clone)]
pub struct PersistentVfsPlan {
    pub tasks: Vec<Task>,
    pub manifest_downloads: Vec<PersistentVfsManifestDownload>,
    pub total_files: usize,
    pub total_bytes: u64,
    pub expected_paths: HashSet<String>,
    pub res_version: String,
    pub file_set: String,
}

#[derive(Debug, Clone)]
pub struct PersistentVfsResult {
    pub total_files: usize,
    pub downloaded_files: usize,
    pub downloaded_bytes: u64,
    pub reused_files: usize,
    pub skipped_files: usize,
    pub failed_files: usize,
    pub res_version: String,
    pub file_set: String,
}

pub(super) fn file_set_includes_group(file_set: PersistentVfsFileSet, resource_name: &str) -> bool {
    match file_set {
        PersistentVfsFileSet::Base => resource_name.eq_ignore_ascii_case(RESOURCE_GROUP_BASE),
        PersistentVfsFileSet::All => {
            resource_name.eq_ignore_ascii_case(RESOURCE_GROUP_BASE)
                || resource_name.eq_ignore_ascii_case(RESOURCE_GROUP_MAIN)
        }
    }
}

async fn read_local_res_index(path: &Path) -> Result<Option<crate::api::types::ResIndex>> {
    if !path_is_file(path).await {
        return Ok(None);
    }
    let encrypted_b64 =
        String::from_utf8(compio::fs::read(path).await.map_err(|e| Error::IoAt {
            action: "open file",
            path: path.to_path_buf(),
            source: e,
        })?)
        .map_err(|e| Error::Message {
            context: "VFS error: ",
            detail: format!("{} is not valid UTF-8 text: {e}", path.display()),
        })?;
    let decrypted = crate::api::crypto::decrypt_res_index(encrypted_b64.trim(), RES_INDEX_KEY)
        .map_err(|e| Error::Message {
            context: "VFS error: ",
            detail: format!("Failed to decrypt {}: {e}", path.display()),
        })?;
    let index = serde_json::from_str::<crate::api::types::ResIndex>(&decrypted).map_err(|e| {
        Error::Message {
            context: "VFS error: ",
            detail: format!("Failed to parse {}: {e}", path.display()),
        }
    })?;
    Ok(Some(index))
}

fn res_index_to_ensure_tasks(
    index: &crate::api::types::ResIndex,
    source_candidates: &[std::path::PathBuf],
    resource_path: &str,
    persistent_root: &Path,
    cfg: &PersistentVfsConfig,
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
        tasks.push(Task::ensure_file(FileEnsureTask {
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
            transfer_class: TransferClass::Vfs,
            archive_repair: None,
        }));
    }

    (tasks, total_files, total_bytes)
}

pub async fn plan_persistent_vfs_tasks(
    api_client: &ApiClient,
    target: &ApiTarget,
    game_version: &str,
    rand_str: &str,
    persistent_root: &Path,
    cfg: &PersistentVfsConfig,
) -> Result<Option<PersistentVfsPlan>> {
    let Some(resources) = api_client
        .get_latest_resources(target, game_version, rand_str, DEFAULT_PLATFORM)
        .await?
    else {
        return Ok(None);
    };

    let mut tasks = Vec::new();
    let mut manifest_downloads = Vec::new();
    let mut total_files = 0usize;
    let mut total_bytes = 0u64;
    let mut expected_paths = HashSet::default();
    let mut file_set_parts = Vec::new();

    let mut source_roots = vec![cfg.source_streaming_assets.clone()];
    for root in &cfg.extra_source_streaming_assets {
        if !source_roots.iter().any(|r| r == root) {
            source_roots.push(root.clone());
        }
    }

    for resource in &resources.resources {
        if !file_set_includes_group(cfg.file_set, &resource.name) {
            continue;
        }

        let pref_filename = resource_manifest_filename(ResourceManifestKind::Pref, &resource.name);
        let index_filename =
            resource_manifest_filename(ResourceManifestKind::Index, &resource.name);
        let pref_url =
            resource_manifest_url(&resource.path, ResourceManifestKind::Pref, &resource.name);
        let index_url =
            resource_manifest_url(&resource.path, ResourceManifestKind::Index, &resource.name);

        let local_pref = read_local_res_index(&persistent_root.join(&pref_filename))
            .await
            .map_err(|e| Error::Message {
                context: "VFS error: ",
                detail: format!("Failed to parse local {pref_filename}: {e}"),
            })?;
        let local_index = read_local_res_index(&persistent_root.join(&index_filename))
            .await
            .map_err(|e| Error::Message {
                context: "VFS error: ",
                detail: format!("Failed to parse local {index_filename}: {e}"),
            })?;

        let (selected_index, manifest_kind) = if let Some(pref) = local_pref {
            (pref, "pref-local")
        } else if let Ok(pref) = api_client.fetch_res_index(&pref_url, RES_INDEX_KEY).await {
            manifest_downloads.push(PersistentVfsManifestDownload {
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
                .map_err(|e| Error::Message {
                    context: "VFS error: ",
                    detail: format!(
                        "Failed to fetch both pref and index manifests for resource group {}: {e}",
                        resource.name
                    ),
                })?;
            manifest_downloads.push(PersistentVfsManifestDownload {
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
            let logical_path = match task {
                Task::Verify { logical_path, .. } | Task::RepairFile { logical_path, .. } => {
                    Some(logical_path)
                }
                _ => None,
            };
            if let Some(logical_path) = logical_path {
                expected_paths.insert(normalize_logical_path(logical_path));
            }
        }
        tasks.extend(group_tasks);
        total_files += group_files;
        total_bytes = total_bytes.saturating_add(group_bytes);
        file_set_parts.push(format!("{}:{}", resource.name, manifest_kind));
    }

    Ok(Some(PersistentVfsPlan {
        tasks,
        manifest_downloads,
        total_files,
        total_bytes,
        expected_paths,
        res_version: resources.res_version,
        file_set: file_set_parts.join(","),
    }))
}

pub async fn setup_persistent_vfs(
    api_client: &ApiClient,
    target: &ApiTarget,
    game_version: &str,
    rand_str: &str,
    persistent_root: &Path,
    cfg: &PersistentVfsConfig,
    task_pool_runner: &mut TaskPoolRunner,
    progress: ProgressSender,
) -> Result<Option<PersistentVfsResult>> {
    let plan = match plan_persistent_vfs_tasks(
        api_client,
        target,
        game_version,
        rand_str,
        persistent_root,
        cfg,
    )
    .await?
    {
        Some(p) => p,
        None => return Ok(None),
    };

    compio::fs::create_dir_all(persistent_root)
        .await
        .map_err(|e| Error::IoAt {
            action: "create directory",
            path: persistent_root.to_path_buf(),
            source: e,
        })?;

    for manifest in &plan.manifest_downloads {
        let dest = persistent_root.join(&manifest.filename);
        api_client
            .download_file(&manifest.url, &dest, false)
            .await
            .map_err(|e| Error::Message {
                context: "API client wrapper error: ",
                detail: format!("Failed to download {}: {e}", manifest.url),
            })?;
    }

    let task_progress = TaskProgress::new(progress)
        .with_verify(ProgressLane::VFS_VERIFY, plan.total_files)
        .with_download(ProgressLane::VFS_DOWNLOAD);
    let result = task_pool_runner
        .run_batch(plan.tasks, task_progress)
        .map_err(|e| Error::Message {
            context: "Task pool error: ",
            detail: format!("Failed to set up Persistent VFS files: {e}"),
        })?;

    let mut outcomes = PathOutcomeTracker::new();
    for event in result.outcomes {
        match event {
            TaskOutcome::Downloaded { path, bytes } => {
                outcomes.record_downloaded(&path, bytes);
            }
            TaskOutcome::Hardlinked { path } => {
                if let Some(logical_path) = logical_path_from_root(persistent_root, &path) {
                    outcomes.record_reused(&logical_path, PathReuseMethod::Hardlink);
                }
            }
            TaskOutcome::Copied { path } => {
                if let Some(logical_path) = logical_path_from_root(persistent_root, &path) {
                    outcomes.record_reused(&logical_path, PathReuseMethod::Copy);
                }
            }
            TaskOutcome::Verified { path, ok, .. } => {
                outcomes.record_verified(&path, ok);
            }
            TaskOutcome::Failed { path, .. } => {
                outcomes.record_failed(&path);
            }
            _ => {}
        }
    }

    if cfg.prune_extra_files {
        let vfs_root = vfs_path(persistent_root);
        if path_is_dir(&vfs_root).await {
            let files = collect_files_recursive(vfs_root.clone()).await?;
            for file in files {
                let rel = match file.strip_prefix(persistent_root) {
                    Ok(r) => r.to_path_buf(),
                    Err(_) => continue,
                };
                let rel_norm = normalize_logical_path(&rel.to_string_lossy());
                if !plan.expected_paths.contains(&rel_norm) {
                    compio::fs::remove_file(&file)
                        .await
                        .map_err(|e| Error::IoAt {
                            action: "remove file or directory",
                            path: file.clone(),
                            source: e,
                        })?;
                }
            }
            remove_empty_dirs_recursive(vfs_root.clone()).await?;
        }
    }

    let summary = outcomes.summary();

    Ok(Some(PersistentVfsResult {
        total_files: plan.total_files,
        downloaded_files: summary.downloaded_files,
        downloaded_bytes: summary.downloaded_bytes,
        reused_files: summary.reused_files,
        skipped_files: summary.skipped_files,
        failed_files: summary.failed_files,
        res_version: plan.res_version,
        file_set: plan.file_set,
    }))
}
