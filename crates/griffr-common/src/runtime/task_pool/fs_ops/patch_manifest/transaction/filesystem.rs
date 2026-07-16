use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::runtime::patch_transaction::{
    write_patch_storage_topology, PatchExecutionPlan, PatchStorageTopology, PATCH_DEFERRED_DIR,
    PATCH_TRANSACTION_DIR,
};
use crate::runtime::task_pool::verify::file_md5;
use crate::runtime::{DELETE_FILES_MANIFEST_NAME, PATCH_MANIFEST_NAME, PATCH_STAGE_DIR};

use super::super::super::extract::move_path_replace_cross_volume;

pub(super) fn remove_path_if_exists(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            std::fs::remove_dir_all(path).map_err(|source| Error::RemoveFailed {
                path: path.to_path_buf(),
                source,
            })
        }
        Ok(_) => std::fs::remove_file(path).map_err(|source| Error::RemoveFailed {
            path: path.to_path_buf(),
            source,
        }),
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::StatFailed {
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub(super) fn move_file_cross_volume(source: &Path, target: &Path) -> Result<()> {
    move_path_replace_cross_volume(source, target)
}

pub(super) fn move_directory_contents(source: &Path, target: &Path) -> Result<()> {
    if !source.exists() {
        std::fs::create_dir_all(target).map_err(|source_error| Error::CreateDirFailed {
            path: target.to_path_buf(),
            source: source_error,
        })?;
        return Ok(());
    }
    std::fs::create_dir_all(target).map_err(|source_error| Error::CreateDirFailed {
        path: target.to_path_buf(),
        source: source_error,
    })?;
    for entry in std::fs::read_dir(source).map_err(|source_error| Error::ReadDirFailed {
        path: source.to_path_buf(),
        source: source_error,
    })? {
        let entry = entry.map_err(|source_error| Error::ReadDirFailed {
            path: source.to_path_buf(),
            source: source_error,
        })?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|source_error| Error::StatFailed {
                path: source_path.clone(),
                source: source_error,
            })?;
        if file_type.is_dir() {
            move_directory_contents(&source_path, &target_path)?;
            let _ = std::fs::remove_dir(&source_path);
        } else {
            if target_path.exists() {
                let source_metadata =
                    std::fs::metadata(&source_path).map_err(|source_error| Error::StatFailed {
                        path: source_path.clone(),
                        source: source_error,
                    })?;
                let target_metadata =
                    std::fs::metadata(&target_path).map_err(|source_error| Error::StatFailed {
                        path: target_path.clone(),
                        source: source_error,
                    })?;
                if !target_metadata.is_file()
                    || source_metadata.len() != target_metadata.len()
                    || file_md5(&source_path)? != file_md5(&target_path)?
                {
                    return Err(Error::Vfs(format!(
                        "External VFS relocation conflict at {}",
                        target_path.display()
                    )));
                }
                std::fs::remove_file(&source_path).map_err(|source_error| Error::RemoveFailed {
                    path: source_path.clone(),
                    source: source_error,
                })?;
                continue;
            }
            move_file_cross_volume(&source_path, &target_path)?;
        }
    }
    Ok(())
}

pub(super) fn create_directory_link(link: &Path, target: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(target, link).map_err(|source| Error::Other(format!(
            "Failed to create external VFS directory link {} -> {}: {}. Enable Windows Developer Mode or run with permission to create symbolic links",
            link.display(),
            target.display(),
            source
        )))
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).map_err(|source| {
            Error::Other(format!(
                "Failed to create external VFS directory link {} -> {}: {}",
                link.display(),
                target.display(),
                source
            ))
        })
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = (link, target);
        Err(Error::Other(
            "External VFS roots are unsupported on this platform".to_string(),
        ))
    }
}

pub(super) fn prepare_external_vfs_root(plan: &PatchExecutionPlan) -> Result<()> {
    let logical = plan.install_root.join(&plan.vfs_base_path);
    if logical == plan.vfs_destination {
        return Ok(());
    }
    if let Ok(existing_target) = std::fs::read_link(&logical) {
        let resolved_target = if existing_target.is_absolute() {
            existing_target.clone()
        } else {
            logical
                .parent()
                .unwrap_or(Path::new("."))
                .join(&existing_target)
        };
        if resolved_target == plan.vfs_destination {
            std::fs::create_dir_all(&plan.vfs_destination).map_err(|source| {
                Error::CreateDirFailed {
                    path: plan.vfs_destination.clone(),
                    source,
                }
            })?;
            return write_patch_storage_topology(
                &plan.install_root,
                &PatchStorageTopology {
                    schema_version: PatchStorageTopology::SCHEMA_VERSION,
                    vfs_link: plan.vfs_base_path.clone(),
                    external_vfs_root: plan.vfs_destination.clone(),
                },
            );
        }
        return Err(Error::Vfs(format!(
            "VFS path {} already links to {}, not requested external root {}",
            logical.display(),
            existing_target.display(),
            plan.vfs_destination.display()
        )));
    }
    if let Some(parent) = logical.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::CreateDirFailed {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    move_directory_contents(&logical, &plan.vfs_destination)?;
    if logical.exists() {
        std::fs::remove_dir_all(&logical).map_err(|source| Error::RemoveFailed {
            path: logical.clone(),
            source,
        })?;
    }
    create_directory_link(&logical, &plan.vfs_destination)?;
    write_patch_storage_topology(
        &plan.install_root,
        &PatchStorageTopology {
            schema_version: PatchStorageTopology::SCHEMA_VERSION,
            vfs_link: plan.vfs_base_path.clone(),
            external_vfs_root: plan.vfs_destination.clone(),
        },
    )
}

pub(super) fn collect_staged_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(&directory).map_err(|source| Error::ReadDirFailed {
            path: directory.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| Error::ReadDirFailed {
                path: directory.clone(),
                source,
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|source| Error::StatFailed {
                path: path.clone(),
                source,
            })?;
            if file_type.is_dir() {
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

pub(super) fn is_patch_control_path(relative: &Path) -> bool {
    relative == Path::new(PATCH_MANIFEST_NAME)
        || relative == Path::new(DELETE_FILES_MANIFEST_NAME)
        || relative.starts_with(PATCH_STAGE_DIR)
}

pub(super) fn commit_top_level_files(
    plan: &PatchExecutionPlan,
    mut callback: Option<&mut dyn FnMut(&Path, usize, usize)>,
) -> Result<()> {
    let files = collect_staged_files(&plan.stage_root)?;
    let deferred = plan.deferred_paths.iter().cloned().collect::<BTreeSet<_>>();
    let commit_files = files
        .into_iter()
        .filter_map(|source| {
            let relative = source.strip_prefix(&plan.stage_root).ok()?.to_path_buf();
            (!is_patch_control_path(&relative)).then_some((source, relative))
        })
        .collect::<Vec<_>>();
    let total = commit_files.len();
    if total > 0 {
        if let Some(callback) = callback.as_deref_mut() {
            callback(Path::new("."), 0, total);
        }
    }
    for (index, (source, relative)) in commit_files.into_iter().enumerate() {
        let target = if deferred.contains(&relative) {
            plan.install_root
                .join(PATCH_TRANSACTION_DIR)
                .join(PATCH_DEFERRED_DIR)
                .join(&relative)
        } else {
            plan.install_root.join(&relative)
        };
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|source_error| Error::CreateDirFailed {
                path: parent.to_path_buf(),
                source: source_error,
            })?;
        }
        move_path_replace_cross_volume(&source, &target)?;
        if let Some(callback) = callback.as_deref_mut() {
            callback(&relative, index + 1, total);
        }
    }
    Ok(())
}
