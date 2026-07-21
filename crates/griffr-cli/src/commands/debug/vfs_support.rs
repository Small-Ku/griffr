use anyhow::{Context, Result};
use compio::buf::BufResult;
use compio::io::AsyncReadAt;
use griffr_common::api::crypto;
use griffr_common::api::types::ResIndex;
use griffr_common::runtime::{
    collect_files_recursive, normalize_logical_path, path_is_dir, path_is_file,
};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::VfsDiffAgainst;
use griffr_common::config::{game_definition, GameId};
use griffr_common::runtime::{
    persistent_path, streaming_assets_path, vfs_path, CONFIG_INI_NAME, PERSISTENT_DIR,
    STREAMING_ASSETS_DIR, VFS_DIR,
};
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LocalResManifests {
    pub index_initial: Option<ResIndex>,
    pub index_main: Option<ResIndex>,
    pub pref_initial: Option<ResIndex>,
    pub pref_main: Option<ResIndex>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VfsExpectedScope {
    IndexFull,
    PrefOnly,
    IndexFullFallback,
}

impl VfsExpectedScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IndexFull => "index-full",
            Self::PrefOnly => "pref-only",
            Self::IndexFullFallback => "index-full-fallback",
        }
    }
}

impl std::fmt::Display for VfsExpectedScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VfsExpectedSet {
    pub scope: VfsExpectedScope,
    pub entries: std::collections::BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VfsExpectedMap {
    pub scope: VfsExpectedScope,
    pub entries: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestFileCounts {
    pub index_initial: Option<usize>,
    pub index_main: Option<usize>,
    pub pref_initial: Option<usize>,
    pub pref_main: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VfsHashMismatch {
    pub path: String,
    pub expected_md5: String,
    pub actual_md5: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceRootSnapshot {
    pub root_path: String,
    pub present: bool,
    pub manifest_counts: ManifestFileCounts,
    pub scope: Option<String>,
    pub expected_files: usize,
    pub actual_files: usize,
    pub missing_files: usize,
    pub extra_files: usize,
    pub hash_mismatch_files: usize,
    pub hash_checked: bool,
    pub actual_paths: Vec<String>,
    pub missing_paths: Vec<String>,
    pub extra_paths: Vec<String>,
    pub hash_mismatches: Vec<VfsHashMismatch>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceStateSnapshot {
    pub schema_version: u32,
    pub captured_at_utc: String,
    pub source_path: String,
    pub endfield_data_path: String,
    pub persistent: ResourceRootSnapshot,
    pub streamingassets: ResourceRootSnapshot,
}

pub async fn emit_json(output: Option<PathBuf>, payload: Value) -> Result<()> {
    let body = serde_json::to_vec_pretty(&payload)?;
    if let Some(output) = output {
        let write_result = compio::fs::write(&output, body).await;
        write_result
            .0
            .with_context(|| format!("Failed to write {}", output.display()))?;
        println!("output={}", output.display());
    } else {
        println!("{}", String::from_utf8(body).context("JSON is not UTF-8")?);
    }
    Ok(())
}

pub fn merge_entries(
    target: &mut std::collections::BTreeSet<String>,
    index: &Option<ResIndex>,
) -> usize {
    if let Some(index) = index {
        let mut added = 0usize;
        for file in &index.files {
            if file.name.is_empty() {
                continue;
            }
            let normalized = normalize_logical_path(&file.name);
            if !normalized.is_empty() && target.insert(normalized) {
                added += 1;
            }
        }
        added
    } else {
        0
    }
}

pub fn normalize_checksum(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_ascii_lowercase())
}

pub fn merge_entries_with_checksums(
    target: &mut std::collections::BTreeMap<String, String>,
    index: &Option<ResIndex>,
) -> usize {
    if let Some(index) = index {
        let mut added = 0usize;
        for file in &index.files {
            if file.name.is_empty() {
                continue;
            }
            let Some(expected_md5) = file
                .md5
                .as_deref()
                .and_then(normalize_checksum)
                .or_else(|| file.hash.as_deref().and_then(normalize_checksum))
            else {
                continue;
            };
            let normalized = normalize_logical_path(&file.name);
            if normalized.is_empty() {
                continue;
            }
            if target.insert(normalized, expected_md5).is_none() {
                added += 1;
            }
        }
        added
    } else {
        0
    }
}

struct VfsManifestSelection<'a> {
    scope: VfsExpectedScope,
    first: &'a Option<ResIndex>,
    second: &'a Option<ResIndex>,
}

fn select_vfs_manifests(
    against: VfsDiffAgainst,
    manifests: &LocalResManifests,
) -> VfsManifestSelection<'_> {
    match against {
        VfsDiffAgainst::Streamingassets => VfsManifestSelection {
            scope: VfsExpectedScope::IndexFull,
            first: &manifests.index_initial,
            second: &manifests.index_main,
        },
        VfsDiffAgainst::Persistent
            if manifests.pref_initial.is_some() || manifests.pref_main.is_some() =>
        {
            VfsManifestSelection {
                scope: VfsExpectedScope::PrefOnly,
                first: &manifests.pref_initial,
                second: &manifests.pref_main,
            }
        }
        VfsDiffAgainst::Persistent => VfsManifestSelection {
            scope: VfsExpectedScope::IndexFullFallback,
            first: &manifests.index_initial,
            second: &manifests.index_main,
        },
    }
}

pub fn select_expected_vfs_set(
    against: VfsDiffAgainst,
    manifests: &LocalResManifests,
) -> Result<VfsExpectedSet> {
    let selection = select_vfs_manifests(against, manifests);
    let mut entries = std::collections::BTreeSet::new();
    merge_entries(&mut entries, selection.first);
    merge_entries(&mut entries, selection.second);
    if entries.is_empty() {
        anyhow::bail!(match selection.scope {
            VfsExpectedScope::IndexFull =>
                "No index files found or index files were empty. Expected index_initial.json and/or index_main.json.",
            VfsExpectedScope::PrefOnly =>
                "Pref files were present but empty. Expected pref_initial.json and/or pref_main.json entries.",
            VfsExpectedScope::IndexFullFallback =>
                "No pref files found and no index files found. Expected pref_*.json or index_*.json.",
        });
    }
    Ok(VfsExpectedSet {
        scope: selection.scope,
        entries,
    })
}

pub fn select_expected_vfs_map(
    against: VfsDiffAgainst,
    manifests: &LocalResManifests,
) -> Result<VfsExpectedMap> {
    let selection = select_vfs_manifests(against, manifests);
    let mut entries = std::collections::BTreeMap::new();
    merge_entries_with_checksums(&mut entries, selection.first);
    merge_entries_with_checksums(&mut entries, selection.second);
    if entries.is_empty() {
        anyhow::bail!(match selection.scope {
            VfsExpectedScope::IndexFull =>
                "No index files with checksum fields found. Expected index_initial.json and/or index_main.json.",
            VfsExpectedScope::PrefOnly =>
                "Pref files were present but contained no checksum fields.",
            VfsExpectedScope::IndexFullFallback =>
                "No pref files found and no index files with checksum fields found.",
        });
    }
    Ok(VfsExpectedMap {
        scope: selection.scope,
        entries,
    })
}

pub async fn resolve_vfs_root(path: &Path) -> Result<PathBuf> {
    let direct_vfs = vfs_path(path);
    if path_is_dir(&direct_vfs).await {
        return Ok(path.to_path_buf());
    }
    if path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case(VFS_DIR))
    {
        let parent = path
            .parent()
            .context("VFS path has no parent directory")?
            .to_path_buf();
        if path_is_dir(&vfs_path(&parent)).await {
            return Ok(parent);
        }
    }
    anyhow::bail!(
        "Path {} is not a VFS root. Expected a directory containing VFS/.",
        path.display()
    );
}

pub async fn try_read_local_res_index(path: &Path, key: &str) -> Result<Option<ResIndex>> {
    match compio::fs::metadata(path).await {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Ok(None),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(source).with_context(|| format!("Failed to inspect {}", path.display()))
        }
    }
    let encrypted_b64 = compio::fs::read(path)
        .await
        .with_context(|| format!("Failed to read {}", path.display()))
        .and_then(|bytes| {
            String::from_utf8(bytes)
                .with_context(|| format!("{} is not valid UTF-8 text", path.display()))
        })?;
    let decrypted = crypto::decrypt_res_index(encrypted_b64.trim(), key)
        .with_context(|| format!("Failed to decrypt {}", path.display()))?;
    let index: ResIndex = serde_json::from_str(&decrypted)
        .with_context(|| format!("Failed to parse decrypted JSON from {}", path.display()))?;
    Ok(Some(index))
}

pub async fn collect_actual_vfs_files(root: &Path) -> Result<std::collections::BTreeSet<String>> {
    let vfs_root = vfs_path(root);
    if !path_is_dir(&vfs_root).await {
        anyhow::bail!("Missing VFS directory at {}", vfs_root.display());
    }

    let files = collect_files_recursive(vfs_root).await?;
    files
        .into_iter()
        .map(|path| {
            let rel = path
                .strip_prefix(root)
                .with_context(|| format!("Failed to strip prefix {}", root.display()))?;
            Ok(normalize_logical_path(&rel.to_string_lossy()))
        })
        .collect()
}

pub async fn file_md5(path: &Path) -> Result<String> {
    const BUFFER_BYTES: usize = 1024 * 1024;

    let file = compio::fs::File::open(path)
        .await
        .with_context(|| format!("Failed to open {}", path.display()))?;
    let mut hasher = Md5::new();
    let mut offset = 0u64;
    let mut buffer = vec![0u8; BUFFER_BYTES];
    loop {
        let BufResult(result, mut returned_buffer) = file.read_at(buffer, offset).await;
        let read = result.with_context(|| format!("Failed to read {}", path.display()))?;
        if read == 0 {
            break;
        }
        returned_buffer.truncate(read);
        hasher.update(&returned_buffer);
        offset = offset.saturating_add(read as u64);
        returned_buffer.resize(BUFFER_BYTES, 0);
        buffer = returned_buffer;
    }
    file.close()
        .await
        .with_context(|| format!("Failed to close {}", path.display()))?;
    Ok(griffr_common::to_hex(&hasher.finalize()))
}

pub async fn resolve_endfield_data_root(path: &Path) -> Result<PathBuf> {
    let data_root_name = game_definition(&GameId::ENDFIELD)
        .expect("Endfield must be present in the product catalog")
        .data_root;
    let mut candidate = if path_is_file(path).await {
        path.parent()
            .context("Input file path has no parent directory")?
            .to_path_buf()
    } else {
        path.to_path_buf()
    };

    if candidate
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case(data_root_name))
    {
        return Ok(candidate);
    }
    let nested_data_root = candidate.join(data_root_name);
    if path_is_dir(&nested_data_root).await {
        return Ok(nested_data_root);
    }
    if path_is_dir(&persistent_path(&candidate)).await
        && path_is_dir(&streaming_assets_path(&candidate)).await
    {
        return Ok(candidate);
    }
    if path_is_file(&candidate.join(CONFIG_INI_NAME)).await {
        return Ok(candidate.join(data_root_name));
    }
    if candidate
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| {
            n.eq_ignore_ascii_case(PERSISTENT_DIR) || n.eq_ignore_ascii_case(STREAMING_ASSETS_DIR)
        })
    {
        candidate = candidate
            .parent()
            .context("Persistent/StreamingAssets path has no parent")?
            .to_path_buf();
        if path_is_dir(&persistent_path(&candidate)).await
            && path_is_dir(&streaming_assets_path(&candidate)).await
        {
            return Ok(candidate);
        }
    }
    anyhow::bail!(
        "Could not resolve {} root from {}. Expected install root, {}, or directory containing {} and {}.",
        data_root_name,
        path.display(),
        data_root_name,
        PERSISTENT_DIR,
        STREAMING_ASSETS_DIR
    );
}

pub fn manifest_file_counts(manifests: &LocalResManifests) -> ManifestFileCounts {
    ManifestFileCounts {
        index_initial: manifests.index_initial.as_ref().map(|m| m.files.len()),
        index_main: manifests.index_main.as_ref().map(|m| m.files.len()),
        pref_initial: manifests.pref_initial.as_ref().map(|m| m.files.len()),
        pref_main: manifests.pref_main.as_ref().map(|m| m.files.len()),
    }
}

pub fn sorted_difference(left: &[String], right: &[String]) -> std::collections::BTreeSet<String> {
    let right_set = right
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    left.iter()
        .filter(|v| !right_set.contains(*v))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
}

pub(super) async fn collect_hash_mismatches(
    root: &Path,
    expected_checksums: &std::collections::BTreeMap<String, String>,
    progress_callback: Option<&dyn Fn(usize, usize, &str)>,
) -> Vec<VfsHashMismatch> {
    let total = expected_checksums.len();
    if let Some(callback) = progress_callback {
        callback(0, total, "");
    }
    let mut mismatches = Vec::new();
    let mut finished = 0usize;
    for (rel_path, expected_md5) in expected_checksums {
        let file_path = root.join(rel_path.replace('/', "\\"));
        if path_is_file(&file_path).await {
            if let Ok(actual_md5) = file_md5(&file_path).await {
                if actual_md5 != *expected_md5 {
                    mismatches.push(VfsHashMismatch {
                        path: rel_path.clone(),
                        expected_md5: expected_md5.clone(),
                        actual_md5,
                    });
                }
            }
        }
        finished = finished.saturating_add(1);
        if let Some(callback) = progress_callback {
            callback(finished, total, rel_path);
        }
    }
    mismatches
}
