use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::runtime::{
    griffr_path, normalize_logical_path, path_is_file, remove_empty_dirs_recursive,
    ArtifactExpectation, ArtifactSource,
};

use super::{commit_vfs_manifests, ResourceIdentity, VfsTaskPlan};

const RESOURCE_BASELINE_STATE_NAME: &str = "resource-baseline.json";
const RESOURCE_BASELINE_PENDING_NAME: &str = "resource-baseline.pending.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedResourceFile {
    pub path: String,
    pub md5: String,
    pub size: u64,
}

impl ManagedResourceFile {
    pub(super) fn expectation(&self) -> ArtifactExpectation {
        ArtifactExpectation::new(&self.path, &self.md5, Some(self.size))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ResourceBaselineState {
    identity: ResourceIdentity,
    files: Vec<ManagedResourceFile>,
}

fn state_path(install_root: &Path) -> PathBuf {
    griffr_path(install_root).join(RESOURCE_BASELINE_STATE_NAME)
}

fn pending_path(install_root: &Path) -> PathBuf {
    griffr_path(install_root).join(RESOURCE_BASELINE_PENDING_NAME)
}

async fn read_state(path: &Path) -> Result<Option<ResourceBaselineState>> {
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

fn write_state(path: &Path, state: &ResourceBaselineState) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(state).map_err(|source| Error::Message {
        context: "VFS state error: ",
        detail: format!("Failed to serialize resource baseline state: {source}"),
    })?;
    crate::runtime::task_pool::fs_ops::write_atomic_bytes(path, &bytes)
}

async fn remove_pending(path: &Path) -> Result<()> {
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

async fn prune_previous_files(
    streaming_assets_root: &Path,
    previous: Option<&ResourceBaselineState>,
    current_files: &[ManagedResourceFile],
) -> Result<()> {
    let Some(previous) = previous else {
        return Ok(());
    };
    let current = current_files
        .iter()
        .map(|file| normalize_logical_path(&file.path))
        .collect::<rapidhash::RapidHashSet<_>>();
    let mut removed_any = false;

    for file in &previous.files {
        if current.contains(&normalize_logical_path(&file.path)) {
            continue;
        }
        let relative = crate::runtime::task_pool::fs_ops::path_safety::parse_safe_relative_path(
            "resource baseline state path",
            &file.path,
        )?;
        let path = streaming_assets_root.join(relative);
        if !path_is_file(&path).await {
            continue;
        }
        // A state record grants permission to remove only the exact artifact
        // Griffr committed previously. Preserve user- or game-modified files.
        if crate::runtime::task_pool::fs_ops::verify_artifact(
            &path,
            &file.expectation(),
            ArtifactSource::Existing,
        )
        .is_err()
        {
            continue;
        }
        compio::fs::remove_file(&path)
            .await
            .map_err(|source| Error::IoAt {
                action: "remove file or directory",
                path: path.clone(),
                source,
            })?;
        removed_any = true;
    }

    if removed_any {
        remove_empty_dirs_recursive(streaming_assets_root.join("VFS")).await?;
    }
    Ok(())
}

/// Publish a verified resource-baseline plan as one recoverable change.
/// Payload tasks must already have finished successfully before this call.
pub async fn finish_vfs_plan(
    install_root: &Path,
    plan: &VfsTaskPlan,
    prune_obsolete: bool,
) -> Result<()> {
    let Some(identity) = plan.identity.clone() else {
        // Package-only or an unsupported resource endpoint makes game_files
        // authoritative for this finished change. Revoke the old managed
        // deletion authority without touching any resource payload.
        remove_pending(&state_path(install_root)).await?;
        remove_pending(&pending_path(install_root)).await?;
        return Ok(());
    };

    let next = ResourceBaselineState {
        identity,
        files: plan.managed_files.clone(),
    };
    let state_path = state_path(install_root);
    let pending_path = pending_path(install_root);
    compio::fs::create_dir_all(griffr_path(install_root))
        .await
        .map_err(|source| Error::IoAt {
            action: "create directory",
            path: griffr_path(install_root),
            source,
        })?;

    let previous = read_state(&state_path).await?;
    if let Some(pending) = read_state(&pending_path).await? {
        if pending != next {
            return Err(Error::Message {
                context: "VFS state error: ",
                detail: format!(
                    "Pending resource baseline plan at {} differs from the current release; finish or inspect the pending change before changing resource identity",
                    pending_path.display()
                ),
            });
        }
    } else {
        write_state(&pending_path, &next)?;
    }

    // Index manifests describe the finished payload set. Publish them after
    // payload closure, then prune only unchanged files from the previous
    // managed state and finally publish the new state.
    commit_vfs_manifests(&plan.manifest_commits)?;
    if prune_obsolete {
        prune_previous_files(
            &plan.streaming_assets_root,
            previous.as_ref(),
            &plan.managed_files,
        )
        .await?;
    }
    write_state(&state_path, &next)?;
    remove_pending(&pending_path).await
}

#[cfg(test)]
mod tests {
    use md5::Digest;

    use super::*;

    #[compio::test]
    async fn prune_preserves_modified_previous_resource() {
        let temp = tempfile::tempdir().unwrap();
        let streaming = temp.path().join("StreamingAssets");
        let unchanged = streaming.join("VFS/unchanged.bin");
        let modified = streaming.join("VFS/modified.bin");
        std::fs::create_dir_all(unchanged.parent().unwrap()).unwrap();
        std::fs::write(&unchanged, b"same").unwrap();
        std::fs::write(&modified, b"user").unwrap();
        let expected_md5 = crate::to_hex(&md5::Md5::digest(b"same"));
        let previous = ResourceBaselineState {
            identity: ResourceIdentity {
                res_version: "r1".to_string(),
                manifests: Vec::new(),
            },
            files: vec![
                ManagedResourceFile {
                    path: "VFS/unchanged.bin".to_string(),
                    md5: expected_md5.clone(),
                    size: 4,
                },
                ManagedResourceFile {
                    path: "VFS/modified.bin".to_string(),
                    md5: expected_md5,
                    size: 4,
                },
            ],
        };

        prune_previous_files(&streaming, Some(&previous), &[])
            .await
            .unwrap();

        assert!(!unchanged.exists());
        assert!(modified.exists());
    }
}
