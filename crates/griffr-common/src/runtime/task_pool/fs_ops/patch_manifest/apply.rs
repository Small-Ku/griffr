use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::error::{Error, Result};

use crate::api::types::ResourcePatchEntry;
use crate::runtime::task_pool::verify::build_issue;

use super::super::extract::{copy_file_with_md5, move_path_replace};
use super::super::path_safety::parse_safe_relative_path;
use super::super::reuse::make_temp_write_path;
use super::resolve_patch_stage_path;
use crate::runtime::{PATCH_DIFF_STAGE_DIR, PATCH_FILES_STAGE_DIR};

fn manifest_path<'a>(alternate: Option<&'a str>, primary: &'a str) -> &'a str {
    alternate
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .unwrap_or(primary)
}

pub(super) fn verify_patch_output(
    path: &Path,
    logical_path: &str,
    expected_md5: &str,
    expected_size: u64,
) -> Result<()> {
    if let Some(issue) = build_issue(path, logical_path, expected_md5, Some(expected_size)) {
        return Err(Error::Vfs(format!(
            "Patch output {} failed verification: kind={:?} expected_size={} actual_size={:?} expected_md5={} actual_md5={:?}",
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

fn apply_local_patch_entry(
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
            "Failed to apply local patch payload {} -> {}: {e}",
            source_path.display(),
            dest_path.display()
        ))
    })?;
    verify_patch_output(&dest_path, &logical_path, &entry.md5, entry.size)
}

fn make_patch_work_path(work_dir: &Path, destination: &Path) -> Result<PathBuf> {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    std::fs::create_dir_all(work_dir).map_err(|source| Error::CreateDirFailed {
        path: work_dir.to_path_buf(),
        source,
    })?;
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("patch-output");
    Ok(work_dir.join(format!(".{file_name}.griffr-patch-{counter}.tmp")))
}

pub(super) fn apply_hdiff_patch(
    base_path: &Path,
    patch_path: &Path,
    dest_path: &Path,
    logical_path: &str,
    expected_md5: &str,
    expected_size: u64,
    work_dir: Option<&Path>,
) -> Result<()> {
    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::CreateDirFailed {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let temp_path = match work_dir {
        Some(work_dir) => make_patch_work_path(work_dir, dest_path)?,
        None => make_temp_write_path(dest_path)?,
    };
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
    if work_dir.is_some() {
        let local_temp = make_temp_write_path(dest_path)?;
        let _ = std::fs::remove_file(&local_temp);
        let copied = match copy_file_with_md5(&temp_path, &local_temp) {
            Ok(copied) => copied,
            Err(error) => {
                let _ = std::fs::remove_file(&temp_path);
                let _ = std::fs::remove_file(&local_temp);
                return Err(error);
            }
        };
        if copied.bytes != expected_size || copied.md5 != expected_md5.to_ascii_lowercase() {
            let _ = std::fs::remove_file(&temp_path);
            let _ = std::fs::remove_file(&local_temp);
            return Err(Error::Vfs(format!(
                "Patch output {} failed inline copy verification: expected size/md5 {}/{}, got {}/{}",
                logical_path,
                expected_size,
                expected_md5,
                copied.bytes,
                copied.md5
            )));
        }
        if let Err(error) = move_path_replace(&local_temp, dest_path) {
            let _ = std::fs::remove_file(&temp_path);
            let _ = std::fs::remove_file(&local_temp);
            return Err(error);
        }
        let _ = std::fs::remove_file(&temp_path);
        return Ok(());
    }
    if let Err(err) = verify_patch_output(&temp_path, logical_path, expected_md5, expected_size) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }
    if let Err(error) = move_path_replace(&temp_path, dest_path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(Error::Other(format!(
            "Failed to replace patched file {} -> {}: {error}",
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
        let base_relative_raw = manifest_path(diff.base_file_path.as_deref(), &diff.base_file);
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

        let patch_relative_raw = manifest_path(diff.patch_path.as_deref(), &diff.patch);
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
            None,
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

pub(super) fn apply_vfs_patch_entry(
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
        return apply_local_patch_entry(install_root, stage_root, dest_root, entry, local_path);
    }
    apply_patch_entry(install_root, stage_root, dest_root, entry)
}

#[cfg(test)]
mod tests {
    use super::manifest_path;

    #[test]
    fn manifest_path_falls_back_when_alternate_is_empty() {
        assert_eq!(manifest_path(Some(""), "primary/path"), "primary/path");
        assert_eq!(manifest_path(Some("  "), "primary/path"), "primary/path");
    }

    #[test]
    fn manifest_path_prefers_non_empty_alternate() {
        assert_eq!(
            manifest_path(Some(" alternate/path "), "primary/path"),
            "alternate/path"
        );
    }
}
