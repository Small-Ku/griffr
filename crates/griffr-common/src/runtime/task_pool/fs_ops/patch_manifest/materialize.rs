use std::path::Path;

use crate::error::{Error, Result};

use crate::api::types::ResourcePatchEntry;
use crate::runtime::task_pool::verify::build_issue;

use super::super::extract::move_path_replace;
use super::super::path_safety::parse_safe_relative_path;
use super::super::reuse::make_temp_write_path;
use super::{resolve_patch_stage_path, PATCH_DIFF_STAGE_DIR, PATCH_FILES_STAGE_DIR};

fn verify_materialized_file(
    path: &Path,
    logical_path: &str,
    expected_md5: &str,
    expected_size: u64,
) -> Result<()> {
    if let Some(issue) = build_issue(path, logical_path, expected_md5, Some(expected_size)) {
        return Err(Error::Vfs(format!(
            "Materialized {} failed verification: kind={:?} expected_size={} actual_size={:?} expected_md5={} actual_md5={:?}",
            logical_path,
            issue.kind,
            issue.expected_size,
            issue.actual_size,
            issue.expected_md5,
            issue.actual_md5
        )));
    }
    Ok(())
}

fn materialize_local_patch_entry(
    install_root: &Path,
    stage_root: &Path,
    dest_root: &Path,
    entry: &ResourcePatchEntry,
    local_path: &str,
) -> Result<()> {
    let source_path = resolve_patch_stage_path(
        install_root,
        stage_root,
        PATCH_FILES_STAGE_DIR,
        "patch.json local_path",
        local_path,
    )?;
    let dest_relative = parse_safe_relative_path("patch.json file name", &entry.name)?;
    let dest_path = dest_root.join(&dest_relative);
    let logical_path = dest_relative.to_string_lossy().replace('\\', "/");

    if !source_path.is_file() {
        return Err(Error::Vfs(format!(
            "patch.json local payload is missing for {}: {}",
            entry.name,
            source_path.display()
        )));
    }
    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::CreateDirFailed {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    move_path_replace(&source_path, &dest_path).map_err(|e| {
        Error::Other(format!(
            "Failed to materialize local patch payload {} -> {}: {e}",
            source_path.display(),
            dest_path.display()
        ))
    })?;
    verify_materialized_file(&dest_path, &logical_path, &entry.md5, entry.size)
}

fn apply_hdiff_patch(
    base_path: &Path,
    patch_path: &Path,
    dest_path: &Path,
    logical_path: &str,
    expected_md5: &str,
    expected_size: u64,
) -> Result<()> {
    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::CreateDirFailed {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let temp_path = make_temp_write_path(dest_path)?;
    let _ = std::fs::remove_file(&temp_path);
    let mut patcher = hdiffpatch_rs::patchers::HDiff::new(
        base_path.to_string_lossy().into_owned(),
        patch_path.to_string_lossy().into_owned(),
        temp_path.to_string_lossy().into_owned(),
    );
    if !patcher.apply() {
        let _ = std::fs::remove_file(&temp_path);
        return Err(Error::Extraction(format!(
            "hdiffpatch-rs failed to apply {} using base {}",
            patch_path.display(),
            base_path.display()
        )));
    }
    if let Err(err) =
        verify_materialized_file(&temp_path, logical_path, expected_md5, expected_size)
    {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }
    if let Err(err) = move_path_replace(&temp_path, dest_path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(Error::Other(format!(
            "Failed to replace patched file {} -> {}: {err}",
            temp_path.display(),
            dest_path.display()
        )));
    }
    Ok(())
}

fn apply_patch_entry(
    install_root: &Path,
    stage_root: &Path,
    dest_root: &Path,
    entry: &ResourcePatchEntry,
) -> Result<()> {
    let dest_relative = parse_safe_relative_path("patch.json file name", &entry.name)?;
    let dest_path = dest_root.join(&dest_relative);
    let logical_path = dest_relative.to_string_lossy().replace('\\', "/");
    let mut candidate_failures = Vec::new();

    for diff in &entry.patch {
        let base_relative_raw = diff
            .base_file_path
            .as_deref()
            .unwrap_or(diff.base_file.as_str());
        let base_relative = parse_safe_relative_path("patch.json base_file", base_relative_raw)?;
        let base_path = dest_root.join(&base_relative);
        let base_logical_path = base_relative.to_string_lossy().replace('\\', "/");

        if let Some(issue) = build_issue(
            &base_path,
            &base_logical_path,
            &diff.base_md5,
            Some(diff.base_size),
        ) {
            candidate_failures.push(format!("{} ({:?})", base_relative.display(), issue.kind));
            continue;
        }

        let patch_relative_raw = diff.patch_path.as_deref().unwrap_or(diff.patch.as_str());
        let patch_path = resolve_patch_stage_path(
            install_root,
            stage_root,
            PATCH_DIFF_STAGE_DIR,
            "patch.json patch path",
            patch_relative_raw,
        )?;
        if !patch_path.is_file() {
            candidate_failures.push(format!(
                "{} (missing patch payload {})",
                base_relative.display(),
                patch_path.display()
            ));
            continue;
        }

        return apply_hdiff_patch(
            &base_path,
            &patch_path,
            &dest_path,
            &logical_path,
            &entry.md5,
            entry.size,
        )
        .map_err(|err| {
            Error::Other(format!(
                "Failed to patch {} from base {}: {err}",
                entry.name,
                base_relative.display()
            ))
        });
    }

    if candidate_failures.is_empty() {
        return Err(Error::Vfs(format!(
            "patch.json entry {} has no applicable patch candidates",
            entry.name
        )));
    }

    Err(Error::Vfs(format!(
        "patch.json entry {} has no verified base file to patch: {}",
        entry.name,
        candidate_failures.join("; ")
    )))
}

pub(super) fn materialize_vfs_patch_entry(
    install_root: &Path,
    stage_root: &Path,
    dest_root: &Path,
    entry: &ResourcePatchEntry,
) -> Result<()> {
    let dest_relative = parse_safe_relative_path("patch.json file name", &entry.name)?;
    let dest_path = dest_root.join(&dest_relative);
    let logical_path = dest_relative.to_string_lossy().replace('\\', "/");
    if build_issue(&dest_path, &logical_path, &entry.md5, Some(entry.size)).is_none() {
        return Ok(());
    }

    if let Some(local_path) = entry
        .local_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        return materialize_local_patch_entry(
            install_root,
            stage_root,
            dest_root,
            entry,
            local_path,
        );
    }
    apply_patch_entry(install_root, stage_root, dest_root, entry)
}
