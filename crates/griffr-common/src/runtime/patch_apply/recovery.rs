use std::path::{Path, PathBuf};

use crate::api::types::{ResourcePatch, ResourcePatchEntry};
use crate::error::{Error, Result};
use crate::runtime::task_pool::fs_ops::path_safety::parse_safe_relative_path;
use crate::runtime::task_pool::verify::build_issue;
use crate::runtime::{
    DELETE_FILES_MANIFEST_NAME, PATCH_DIFF_STAGE_DIR, PATCH_FILES_STAGE_DIR, PATCH_MANIFEST_NAME,
    PATCH_STAGE_DIR,
};

use super::{
    read_patch_plan, read_predownload_stage_metadata, PlannedPatchSource, PATCH_DEFERRED_DIR,
    PATCH_PLAN_NAME, PATCH_WORK_DIR, PREDOWNLOAD_STAGE_METADATA_NAME,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchRecoveryState {
    ArchiveReady { stage_dir: PathBuf },
    ExtractedReady,
    ExtractedMissing { missing: Vec<String> },
    DeletePending,
    Idle,
    Inconsistent { reasons: Vec<String> },
}

fn resolve_stage_payload(
    install_root: &Path,
    stage_root: &Path,
    stage_subdir: &str,
    label: &str,
    raw: &str,
) -> Result<PathBuf> {
    let relative = parse_safe_relative_path(label, raw)?;
    let subdir = Path::new(stage_subdir);
    if relative.starts_with(PATCH_STAGE_DIR) {
        return Ok(install_root.join(relative));
    }
    if relative.starts_with(subdir) {
        return Ok(stage_root.join(relative));
    }
    Ok(stage_root.join(stage_subdir).join(relative))
}

fn entry_is_recoverable(
    install_root: &Path,
    stage_root: &Path,
    dest_root: &Path,
    entry: &ResourcePatchEntry,
) -> Result<bool> {
    let relative = parse_safe_relative_path("patch.json file name", &entry.name)?;
    let destination = dest_root.join(&relative);
    let logical = relative.to_string_lossy().replace('\\', "/");
    if build_issue(&destination, &logical, &entry.md5, Some(entry.size)).is_none() {
        return Ok(true);
    }
    if let Some(local_path) = entry.effective_local_path() {
        return Ok(resolve_stage_payload(
            install_root,
            stage_root,
            PATCH_FILES_STAGE_DIR,
            "patch.json local_path",
            local_path,
        )?
        .is_file());
    }
    for diff in &entry.patch {
        let base_relative =
            parse_safe_relative_path("patch.json base_file", diff.effective_base_file())?;
        let base_path = dest_root.join(&base_relative);
        let base_logical = base_relative.to_string_lossy().replace('\\', "/");
        let patch_path = resolve_stage_payload(
            install_root,
            stage_root,
            PATCH_DIFF_STAGE_DIR,
            "patch.json patch path",
            diff.effective_patch_path(),
        )?;
        if build_issue(
            &base_path,
            &base_logical,
            &diff.base_md5,
            Some(diff.base_size),
        )
        .is_none()
            && patch_path.is_file()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn get_patch_recovery_state(
    install_root: &Path,
    stage_dir: Option<&Path>,
) -> Result<PatchRecoveryState> {
    let manifest_path = install_root.join(PATCH_MANIFEST_NAME);
    let stage_root = install_root.join(PATCH_STAGE_DIR);
    let delete_manifest = install_root.join(DELETE_FILES_MANIFEST_NAME);
    let deferred = install_root.join(PATCH_WORK_DIR).join(PATCH_DEFERRED_DIR);

    let plan_path = install_root.join(PATCH_WORK_DIR).join(PATCH_PLAN_NAME);
    if plan_path.is_file() {
        let plan = read_patch_plan(install_root)?;
        let missing_stage = plan.entries.iter().any(|entry| match &entry.source {
            PlannedPatchSource::AlreadyPresent => false,
            PlannedPatchSource::Local { payload } => {
                !plan.stage_root.join(payload).is_file()
                    && build_issue(
                        &entry.destination,
                        &entry.name,
                        &entry.expected_md5,
                        Some(entry.expected_size),
                    )
                    .is_some()
            }
            PlannedPatchSource::Hdiff {
                base,
                payload,
                base_md5,
                base_size,
            } => {
                let output_missing = build_issue(
                    &entry.destination,
                    &entry.name,
                    &entry.expected_md5,
                    Some(entry.expected_size),
                )
                .is_some();
                output_missing
                    && (!plan.stage_root.join(payload).is_file()
                        || build_issue(base, &entry.name, base_md5, Some(*base_size)).is_some())
            }
        });
        return if missing_stage {
            Ok(PatchRecoveryState::ExtractedMissing {
                missing: vec![format!(
                    "Patch apply staging is missing required data at {}",
                    plan.stage_root.display()
                )],
            })
        } else {
            Ok(PatchRecoveryState::ExtractedReady)
        };
    }

    if manifest_path.is_file() {
        let manifest: ResourcePatch =
            serde_json::from_slice(&std::fs::read(&manifest_path).map_err(|source| {
                Error::IoAt {
                    action: "open file",
                    path: manifest_path.clone(),
                    source,
                }
            })?)?;
        let base = parse_safe_relative_path("patch.json vfs_base_path", &manifest.vfs_base_path)?;
        let dest_root = install_root.join(base);
        let mut missing = Vec::new();
        for entry in &manifest.files {
            if !entry_is_recoverable(install_root, &stage_root, &dest_root, entry)? {
                missing.push(entry.name.clone());
            }
        }
        if missing.is_empty() {
            return Ok(PatchRecoveryState::ExtractedReady);
        }
        return Ok(PatchRecoveryState::ExtractedMissing { missing });
    }

    if stage_root.exists() {
        return Ok(PatchRecoveryState::Inconsistent {
            reasons: vec![format!(
                "{} exists without {}",
                stage_root.display(),
                manifest_path.display()
            )],
        });
    }
    if delete_manifest.is_file() {
        return Ok(PatchRecoveryState::DeletePending);
    }
    if deferred.exists() {
        return Ok(PatchRecoveryState::Inconsistent {
            reasons: vec![format!(
                "Deferred patch files exist at {} without a patch plan",
                deferred.display()
            )],
        });
    }
    if let Some(stage_dir) = stage_dir {
        let metadata_path = stage_dir.join(PREDOWNLOAD_STAGE_METADATA_NAME);
        if metadata_path.is_file() {
            let metadata = read_predownload_stage_metadata(stage_dir)?;
            if metadata.archives_ready(stage_dir)? {
                return Ok(PatchRecoveryState::ArchiveReady {
                    stage_dir: stage_dir.to_path_buf(),
                });
            }
            return Ok(PatchRecoveryState::Inconsistent {
                reasons: vec![format!(
                    "Predownload archives under {} are missing required data",
                    stage_dir.display()
                )],
            });
        }
    }
    Ok(PatchRecoveryState::Idle)
}
