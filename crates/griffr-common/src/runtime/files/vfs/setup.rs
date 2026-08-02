use super::sync::is_compatible_res_index_version;
use crate::error::{Error, Result};
use futures_util::{stream, StreamExt, TryStreamExt};
use md5::Digest;
use std::collections::BTreeMap;

use rapidhash::RapidHashSet as HashSet;
use serde::{Deserialize, Serialize};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::api::client::ApiClient;
use crate::api::crypto::RES_INDEX_KEY;
use crate::api::protocol::DEFAULT_PLATFORM;
use crate::config::ApiTarget;
use crate::runtime::task_pool::{
    FileEnsureTask, Task, TaskOutcome, TaskPoolRunner, TaskProgress, TransferClass,
};
use crate::runtime::{
    build_cdn_file_url, normalize_logical_path, path_is_file, remove_empty_dirs_recursive,
    resource_manifest_filename, resource_manifest_url, vfs_path, PathOutcomeTracker, ProgressLane,
    ProgressSender, ResourceManifestKind, RESOURCE_GROUP_BASE, RESOURCE_GROUP_MAIN,
};

#[derive(Debug, Clone, Default)]
pub struct VfsFilePlanOptions {
    /// Candidate StreamingAssets roots from other installs for VFS file reuse.
    pub source_streaming_assets: Vec<std::path::PathBuf>,
    /// Allow invalid destinations to be repaired by reuse or download.
    pub allow_repair: bool,
    /// Allow copying from source installs when reuse is available.
    pub allow_copy_fallback: bool,
    /// Prefer copying from reuse sources even when local files already verify.
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
pub struct VfsManifestCommit {
    pub dest: std::path::PathBuf,
    pub logical_path: String,
    pub encrypted_bytes: Vec<u8>,
    pub expected_md5: String,
}

impl VfsManifestCommit {
    pub fn claim(&self) -> crate::runtime::ArtifactClaim {
        crate::runtime::ArtifactClaim::new(
            self.dest.clone(),
            crate::runtime::ArtifactExpectation::new(
                &self.logical_path,
                &self.expected_md5,
                Some(self.encrypted_bytes.len() as u64),
            ),
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceManifestIdentity {
    pub logical_path: String,
    pub md5: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceIdentity {
    pub res_version: String,
    pub manifests: Vec<ResourceManifestIdentity>,
}

#[derive(Debug, Clone, Default)]
pub struct VfsTaskPlan {
    pub tasks: Vec<Task>,
    pub claims: Vec<crate::runtime::ArtifactClaim>,
    pub manifest_commits: Vec<VfsManifestCommit>,
    pub managed_files: Vec<super::ManagedResourceFile>,
    pub streaming_assets_root: PathBuf,
    pub identity: Option<ResourceIdentity>,
    pub total_files: usize,
    pub total_bytes: u64,
    pub res_version: String,
}

pub fn commit_vfs_manifests(manifests: &[VfsManifestCommit]) -> Result<()> {
    for manifest in manifests {
        crate::runtime::task_pool::fs_ops::write_atomic_bytes(
            &manifest.dest,
            &manifest.encrypted_bytes,
        )?;
    }
    Ok(())
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
    /// Prefer copying from source candidates even when destination already verifies.
    pub prefer_reuse: bool,
    /// Allow downloading missing files from CDN when not found in source roots.
    pub allow_download: bool,
    /// Remove previously Griffr-managed files that are no longer selected.
    pub prune_extra_files: bool,
    /// Griffr private state root for pending and managed-file records.
    pub state_root: PathBuf,
}

const PERSISTENT_VFS_STATE_NAME: &str = "persistent-vfs.json";
const PERSISTENT_VFS_PENDING_NAME: &str = "persistent-vfs.pending.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistentManagedFile {
    pub path: String,
    pub md5: String,
    pub size: u64,
}

impl PersistentManagedFile {
    fn expectation(&self) -> crate::runtime::ArtifactExpectation {
        crate::runtime::ArtifactExpectation::new(&self.path, &self.md5, Some(self.size))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PersistentVfsState {
    res_version: String,
    file_set: String,
    manifests: Vec<ResourceManifestIdentity>,
    files: Vec<PersistentManagedFile>,
}

#[derive(Debug, Clone)]
pub struct PersistentVfsPlan {
    pub tasks: Vec<Task>,
    pub manifest_commits: Vec<VfsManifestCommit>,
    pub manifest_identities: Vec<ResourceManifestIdentity>,
    pub managed_files: Vec<PersistentManagedFile>,
    pub total_files: usize,
    pub total_bytes: u64,
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

async fn read_local_res_index_document(
    path: &Path,
) -> Result<Option<crate::api::client::ResIndexDocument>> {
    if !path_is_file(path).await {
        return Ok(None);
    }
    let encrypted_bytes = compio::fs::read(path).await.map_err(|e| Error::IoAt {
        action: "open file",
        path: path.to_path_buf(),
        source: e,
    })?;
    let encrypted_b64 = std::str::from_utf8(&encrypted_bytes).map_err(|e| Error::Message {
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
    let md5 = crate::to_hex(&md5::Md5::digest(&encrypted_bytes));
    Ok(Some(crate::api::client::ResIndexDocument {
        index,
        encrypted_bytes,
        md5,
    }))
}

fn res_index_to_ensure_tasks(
    index: &crate::api::types::ResIndex,
    source_candidates: &[PathBuf],
    resource_path: &str,
    persistent_root: &Path,
    cfg: &PersistentVfsConfig,
) -> Result<(Vec<Task>, Vec<PersistentManagedFile>, u64)> {
    let mut tasks = Vec::new();
    let mut managed_files = Vec::new();
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
            .trim()
            .to_ascii_lowercase();
        if expected_md5.is_empty() {
            continue;
        }
        if expected_md5.len() != 32 || !expected_md5.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error::Message {
                context: "VFS error: ",
                detail: format!(
                    "Persistent preference entry {} contains invalid MD5 {:?}",
                    file.name, expected_md5
                ),
            });
        }
        let relative = crate::runtime::task_pool::fs_ops::path_safety::parse_safe_relative_path(
            "resource index path",
            &file.name,
        )?;
        let logical_path = relative.to_string_lossy().replace('\\', "/");
        managed_files.push(PersistentManagedFile {
            path: logical_path,
            md5: expected_md5.clone(),
            size: file.size,
        });
        total_bytes = total_bytes.saturating_add(file.size);
        tasks.push(Task::ensure_file(FileEnsureTask {
            dest: persistent_root.join(&relative),
            logical_path: file.name.clone(),
            expected_md5,
            expected_size: file.size,
            source_candidates: source_candidates
                .iter()
                .map(|root| root.join(&relative))
                .collect(),
            download_url: if cfg.allow_download {
                Some(build_cdn_file_url(resource_path, &file.name))
            } else {
                None
            },
            // Persistent is game-managed mutable state. Never share an inode
            // with StreamingAssets or another install.
            allow_copy_fallback: true,
            copy_only: true,
            prefer_reuse: cfg.prefer_reuse,
            retry_count: 0,
            transfer_class: TransferClass::Vfs,
            archive_repair: None,
        }));
    }

    Ok((tasks, managed_files, total_bytes))
}

fn persistent_state_path(cfg: &PersistentVfsConfig) -> PathBuf {
    cfg.state_root.join(PERSISTENT_VFS_STATE_NAME)
}

fn persistent_pending_path(cfg: &PersistentVfsConfig) -> PathBuf {
    cfg.state_root.join(PERSISTENT_VFS_PENDING_NAME)
}

async fn read_persistent_state(path: &Path) -> Result<Option<PersistentVfsState>> {
    let bytes = match compio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(Error::IoAt {
                action: "open file",
                path: path.to_path_buf(),
                source,
            });
        }
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|source| Error::Message {
            context: "VFS state error: ",
            detail: format!("Failed to parse {}: {source}", path.display()),
        })
}

fn write_persistent_state(path: &Path, state: &PersistentVfsState) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(state).map_err(|source| Error::Message {
        context: "VFS state error: ",
        detail: format!("Failed to serialize Persistent VFS state: {source}"),
    })?;
    crate::runtime::task_pool::fs_ops::write_atomic_bytes(path, &bytes)
}

async fn remove_pending_state(path: &Path) -> Result<()> {
    match compio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::IoAt {
            action: "remove file or directory",
            path: path.to_path_buf(),
            source,
        }),
    }
}

async fn prune_previous_managed_files(
    persistent_root: &Path,
    previous: Option<&PersistentVfsState>,
    current_files: &[PersistentManagedFile],
) -> Result<()> {
    let Some(previous) = previous else {
        return Ok(());
    };
    let current: HashSet<String> = current_files
        .iter()
        .map(|file| normalize_logical_path(&file.path))
        .collect();
    let mut seen = HashSet::default();
    let mut jobs = Vec::new();
    for file in &previous.files {
        let normalized = normalize_logical_path(&file.path);
        if current.contains(&normalized) || !seen.insert(normalized) {
            continue;
        }
        let relative = crate::runtime::task_pool::fs_ops::path_safety::parse_safe_relative_path(
            "Persistent VFS state path",
            &file.path,
        )?;
        jobs.push((persistent_root.join(relative), file.expectation()));
    }

    let prune_jobs = stream::iter(jobs)
        .map(|(path, expectation)| async move {
            crate::runtime::compat_fs::run_blocking("Persistent VFS prune", move || {
                if !std::fs::metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
                    return Ok(false);
                }
                // Delete only files that still match the exact artifact Griffr
                // previously managed. Preserve game- or user-modified content.
                if crate::runtime::task_pool::fs_ops::verify_artifact(
                    &path,
                    &expectation,
                    crate::runtime::ArtifactSource::Existing,
                )
                .is_err()
                {
                    return Ok(false);
                }
                std::fs::remove_file(&path).map_err(|source| Error::IoAt {
                    action: "remove file or directory",
                    path: path.clone(),
                    source,
                })?;
                Ok(true)
            })
            .await
        })
        .buffered(super::RESOURCE_PRUNE_CONCURRENCY);
    futures_util::pin_mut!(prune_jobs);
    let mut removed_any = false;
    while let Some(removed) = prune_jobs.try_next().await? {
        removed_any |= removed;
    }

    if removed_any {
        remove_empty_dirs_recursive(vfs_path(persistent_root)).await?;
    }
    Ok(())
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

    let mut files_by_path = BTreeMap::<String, (PersistentManagedFile, Task)>::new();
    let mut manifest_commits = Vec::new();
    let mut manifest_identities = Vec::new();
    let mut file_set_parts = Vec::new();

    let mut source_roots = Vec::with_capacity(1 + cfg.extra_source_streaming_assets.len());
    let mut seen_source_roots = HashSet::default();
    for root in
        std::iter::once(&cfg.source_streaming_assets).chain(&cfg.extra_source_streaming_assets)
    {
        if seen_source_roots.insert(root.clone()) {
            source_roots.push(root.clone());
        }
    }

    let selected_resources = resources
        .resources
        .iter()
        .filter(|resource| file_set_includes_group(cfg.file_set, &resource.name));
    let resource_documents = stream::iter(selected_resources)
        .map(|resource| async move {
            let pref_filename =
                resource_manifest_filename(ResourceManifestKind::Pref, &resource.name);
            let pref_path = persistent_root.join(&pref_filename);
            let pref_url =
                resource_manifest_url(&resource.path, ResourceManifestKind::Pref, &resource.name);

            let (document, commit_manifest) =
                if let Some(document) = read_local_res_index_document(&pref_path)
                    .await
                    .map_err(|e| Error::Message {
                        context: "VFS error: ",
                        detail: format!("Failed to parse local {pref_filename}: {e}"),
                    })?
                {
                    (document, false)
                } else {
                    let document = api_client
                        .fetch_res_index_document(&pref_url, RES_INDEX_KEY)
                        .await
                        .map_err(|e| Error::Message {
                            context: "VFS error: ",
                            detail: format!(
                                "Persistent requires the game-selected {pref_filename} manifest for resource group {}. Start the game and let it select resources, or make the pref manifest available from the resource service. The full index is intentionally not used as a fallback: {e}",
                                resource.name
                            ),
                        })?;
                    (document, true)
                };

            Ok::<_, Error>((resource, pref_filename, pref_path, document, commit_manifest))
        })
        .buffered(super::RESOURCE_MANIFEST_CONCURRENCY);
    futures_util::pin_mut!(resource_documents);

    while let Some((resource, pref_filename, pref_path, document, commit_manifest)) =
        resource_documents.try_next().await?
    {
        if !is_compatible_res_index_version(
            &document.index.version,
            &resource.version,
            &resources.res_version,
        ) {
            return Err(Error::Message {
                context: "VFS error: ",
                detail: format!(
                    "Persistent preference manifest {} has resource version {}, expected {} or {}",
                    pref_filename, document.index.version, resource.version, resources.res_version
                ),
            });
        }

        manifest_identities.push(ResourceManifestIdentity {
            logical_path: pref_filename.clone(),
            md5: document.md5.clone(),
            size: document.encrypted_bytes.len() as u64,
        });
        let (group_tasks, group_files, _) = res_index_to_ensure_tasks(
            &document.index,
            &source_roots,
            &resource.path,
            persistent_root,
            cfg,
        )?;
        for (task, file) in group_tasks.into_iter().zip(group_files) {
            let key = normalize_logical_path(&file.path);
            if let Some((previous, _)) = files_by_path.get(&key) {
                if previous.md5 != file.md5 || previous.size != file.size {
                    return Err(Error::Message {
                        context: "VFS error: ",
                        detail: format!(
                            "Persistent preference manifests claim {} with different expected content",
                            file.path
                        ),
                    });
                }
                continue;
            }
            files_by_path.insert(key, (file, task));
        }
        if commit_manifest {
            manifest_commits.push(VfsManifestCommit {
                dest: pref_path,
                logical_path: pref_filename,
                encrypted_bytes: document.encrypted_bytes,
                expected_md5: document.md5,
            });
        }
        file_set_parts.push(resource.name.clone());
    }

    if file_set_parts.is_empty() {
        return Err(Error::Message {
            context: "VFS error: ",
            detail: format!(
                "Resource response did not contain the selected Persistent file set {}",
                cfg.file_set
            ),
        });
    }

    let (managed_files, tasks): (Vec<_>, Vec<_>) = files_by_path.into_values().unzip();
    let total_bytes = managed_files
        .iter()
        .map(|file| file.size)
        .fold(0u64, u64::saturating_add);
    manifest_identities.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
    for pair in manifest_identities.windows(2) {
        if normalize_logical_path(&pair[0].logical_path)
            == normalize_logical_path(&pair[1].logical_path)
            && (pair[0].md5 != pair[1].md5 || pair[0].size != pair[1].size)
        {
            return Err(Error::Message {
                context: "VFS error: ",
                detail: format!(
                    "Persistent preference manifests provide {} with different content",
                    pair[0].logical_path
                ),
            });
        }
    }
    manifest_identities.dedup_by(|left, right| {
        normalize_logical_path(&left.logical_path) == normalize_logical_path(&right.logical_path)
    });
    manifest_commits.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
    for pair in manifest_commits.windows(2) {
        if normalize_logical_path(&pair[0].logical_path)
            == normalize_logical_path(&pair[1].logical_path)
            && (pair[0].expected_md5 != pair[1].expected_md5
                || pair[0].encrypted_bytes.len() != pair[1].encrypted_bytes.len())
        {
            return Err(Error::Message {
                context: "VFS error: ",
                detail: format!(
                    "Persistent preference manifest commits provide {} with different content",
                    pair[0].logical_path
                ),
            });
        }
    }
    manifest_commits.dedup_by(|left, right| {
        normalize_logical_path(&left.logical_path) == normalize_logical_path(&right.logical_path)
    });
    file_set_parts.sort();
    file_set_parts.dedup();

    Ok(Some(PersistentVfsPlan {
        tasks,
        manifest_commits,
        manifest_identities,
        total_files: managed_files.len(),
        total_bytes,
        managed_files,
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
        Some(plan) => plan,
        None => return Ok(None),
    };

    compio::fs::create_dir_all(persistent_root)
        .await
        .map_err(|source| Error::IoAt {
            action: "create directory",
            path: persistent_root.to_path_buf(),
            source,
        })?;
    compio::fs::create_dir_all(&cfg.state_root)
        .await
        .map_err(|source| Error::IoAt {
            action: "create directory",
            path: cfg.state_root.clone(),
            source,
        })?;

    let state_path = persistent_state_path(cfg);
    let pending_path = persistent_pending_path(cfg);
    let previous_state = read_persistent_state(&state_path).await?;
    let next_state = PersistentVfsState {
        res_version: plan.res_version.clone(),
        file_set: plan.file_set.clone(),
        manifests: plan.manifest_identities.clone(),
        files: plan.managed_files.clone(),
    };
    // Persist intent before any payload can change. A failed run leaves this
    // marker so the next idempotent run can finish the same change. Do not
    // silently replace it with a different resource selection under the same
    // command path.
    if let Some(pending_state) = read_persistent_state(&pending_path).await? {
        if pending_state != next_state {
            return Err(Error::Message {
                context: "VFS state error: ",
                detail: format!(
                    "Pending Persistent VFS plan at {} differs from the current resource selection; remove or inspect the pending marker before starting a different change",
                    pending_path.display()
                ),
            });
        }
    } else {
        write_persistent_state(&pending_path, &next_state)?;
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
            TaskOutcome::Committed { proof } => outcomes.record_committed(&proof),
            TaskOutcome::Verified { path, ok, .. } => {
                outcomes.record_verified(&path, ok);
            }
            TaskOutcome::Failed { path, .. } => {
                outcomes.record_failed(&path);
            }
            _ => {}
        }
    }

    let summary = outcomes.summary();
    let failed_graph_nodes = result
        .metrics
        .graph
        .failed_nodes
        .saturating_add(result.metrics.graph.cancelled_nodes);
    if summary.failed_files > 0 || failed_graph_nodes > 0 {
        return Err(Error::Message {
            context: "VFS error: ",
            detail: format!(
                "Persistent VFS payload failed for {} reported file(s) and {} failed or cancelled graph node(s); preference manifests and managed state were not committed",
                summary.failed_files, failed_graph_nodes
            ),
        });
    }

    // Preference manifests describe a usable working set, so publish them
    // only after every selected payload has verified.
    commit_vfs_manifests(&plan.manifest_commits)?;

    if cfg.prune_extra_files {
        prune_previous_managed_files(
            persistent_root,
            previous_state.as_ref(),
            &plan.managed_files,
        )
        .await?;
    }

    write_persistent_state(&state_path, &next_state)?;
    remove_pending_state(&pending_path).await?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::{ResIndex, ResIndexFile};

    fn persistent_config(root: &Path) -> PersistentVfsConfig {
        PersistentVfsConfig {
            file_set: PersistentVfsFileSet::Base,
            source_streaming_assets: root.join("StreamingAssets"),
            extra_source_streaming_assets: Vec::new(),
            prefer_reuse: false,
            allow_download: false,
            prune_extra_files: false,
            state_root: root.join(".griffr"),
        }
    }

    #[test]
    fn persistent_payload_reuse_is_copy_only() {
        let root = Path::new("game");
        let index = ResIndex {
            version: "r1".to_string(),
            path: String::new(),
            files: vec![ResIndexFile {
                index: 0,
                name: "VFS/file.bin".to_string(),
                hash: None,
                size: 4,
                r#type: 0,
                md5: Some("00000000000000000000000000000000".to_string()),
                manifest: 0,
            }],
        };

        let (tasks, managed, _) = res_index_to_ensure_tasks(
            &index,
            &[root.join("StreamingAssets")],
            "https://example.invalid/resources",
            &root.join("Persistent"),
            &persistent_config(root),
        )
        .unwrap();

        assert_eq!(managed.len(), 1);
        assert!(matches!(
            tasks.as_slice(),
            [Task::Verify {
                on_fail: Some(repair),
                ..
            }] if matches!(repair.as_ref(), Task::RepairFile { copy_only: true, .. })
        ));
    }

    #[compio::test]
    async fn prune_removes_only_unchanged_previous_managed_files() {
        let temp = tempfile::tempdir().unwrap();
        let persistent = temp.path().join("Persistent");
        let unchanged = persistent.join("VFS/unchanged.bin");
        let modified = persistent.join("VFS/modified.bin");
        std::fs::create_dir_all(unchanged.parent().unwrap()).unwrap();
        std::fs::write(&unchanged, b"same").unwrap();
        std::fs::write(&modified, b"user").unwrap();
        let expected_md5 = crate::to_hex(&md5::Md5::digest(b"same"));
        let previous = PersistentVfsState {
            res_version: "r1".to_string(),
            file_set: "initial".to_string(),
            manifests: Vec::new(),
            files: vec![
                PersistentManagedFile {
                    path: "VFS/unchanged.bin".to_string(),
                    md5: expected_md5.clone(),
                    size: 4,
                },
                PersistentManagedFile {
                    path: "VFS/modified.bin".to_string(),
                    md5: expected_md5,
                    size: 4,
                },
            ],
        };

        prune_previous_managed_files(&persistent, Some(&previous), &[])
            .await
            .unwrap();

        assert!(!unchanged.exists());
        assert!(modified.exists());
    }
}
