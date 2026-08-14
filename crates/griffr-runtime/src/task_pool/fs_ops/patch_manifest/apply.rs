use std::path::Path;
#[cfg(feature = "hdiff-patch")]
use std::path::PathBuf;
#[cfg(feature = "hdiff-patch")]
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::error::{Error, Result};

use crate::task_pool::verify::build_issue;
use griffr_hypergryph_api::ResourcePatchEntry;

#[cfg(feature = "hdiff-patch")]
use super::super::artifact::commit_observed_artifact;
use super::super::artifact::{commit_verified_artifact, verify_artifact};
#[cfg(feature = "hdiff-patch")]
use super::super::extract::copy_file_with_md5;
use super::super::path_safety::parse_safe_relative_path;
#[cfg(feature = "hdiff-patch")]
use super::super::reuse::make_temp_write_path;
use super::resolve_patch_stage_path;
#[cfg(feature = "hdiff-patch")]
use crate::ArtifactDigest;
use crate::{
    ArtifactExpectation, ArtifactProof, ArtifactSource, PATCH_DIFF_STAGE_DIR, PATCH_FILES_STAGE_DIR,
};

fn manifest_path<'a>(alternate: Option<&'a str>, primary: &'a str) -> &'a str {
    alternate
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .unwrap_or(primary)
}

fn apply_local_patch_entry(
    install_root: &Path,
    stage_root: &Path,
    dest_root: &Path,
    logical_root: &Path,
    entry: &ResourcePatchEntry,
    local_path: &str,
) -> Result<ArtifactProof> {
    let source_path = resolve_patch_stage_path(
        install_root,
        stage_root,
        PATCH_FILES_STAGE_DIR,
        "patch.json local_path",
        local_path,
    )?;
    let dest_relative = parse_safe_relative_path("patch.json file name", &entry.name)?;
    let dest_path = dest_root.join(&dest_relative);
    let logical_path = logical_root
        .join(&dest_relative)
        .to_string_lossy()
        .replace('\\', "/");

    if !source_path.is_file() {
        return Err(Error::Message {
            context: "VFS error: ",
            detail: format!(
                "patch.json local payload is missing for {}: {}",
                entry.name,
                source_path.display()
            ),
        });
    }
    let expectation = ArtifactExpectation::new(&logical_path, &entry.md5, Some(entry.size));
    commit_verified_artifact(
        &source_path,
        &dest_path,
        &expectation,
        ArtifactSource::LocalPatch,
    )
}

#[cfg(feature = "hdiff-patch")]
fn make_patch_work_path(work_dir: &Path, destination: &Path) -> Result<PathBuf> {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    std::fs::create_dir_all(work_dir).map_err(|source| Error::IoAt {
        action: "create directory",
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

#[cfg(feature = "hdiff-patch")]
pub(super) fn apply_hdiff_patch(
    base_path: &Path,
    patch_path: &Path,
    dest_path: &Path,
    logical_path: &str,
    expected_md5: &str,
    expected_size: u64,
    work_dir: Option<&Path>,
) -> Result<ArtifactProof> {
    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::IoAt {
            action: "create directory",
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
        return Err(Error::Message {
            context: "Extraction error: ",
            detail: format!(
                "hdiffpatch-rs failed to apply {} using base {}",
                patch_path.display(),
                base_path.display()
            ),
        });
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
        let expectation = ArtifactExpectation::new(logical_path, expected_md5, Some(expected_size));
        let digest = ArtifactDigest::new(copied.bytes, copied.md5);
        let proof = match commit_observed_artifact(
            &local_temp,
            dest_path,
            &expectation,
            ArtifactSource::HdiffPatch,
            &digest,
        ) {
            Ok(proof) => proof,
            Err(error) => {
                let _ = std::fs::remove_file(&temp_path);
                let _ = std::fs::remove_file(&local_temp);
                return Err(error);
            }
        };
        let _ = std::fs::remove_file(&temp_path);
        return Ok(proof);
    }
    let expectation = ArtifactExpectation::new(logical_path, expected_md5, Some(expected_size));
    match commit_verified_artifact(
        &temp_path,
        dest_path,
        &expectation,
        ArtifactSource::HdiffPatch,
    ) {
        Ok(proof) => Ok(proof),
        Err(error) => {
            let _ = std::fs::remove_file(&temp_path);
            Err(error)
        }
    }
}

#[cfg(not(feature = "hdiff-patch"))]
pub(super) fn apply_hdiff_patch(
    _base_path: &Path,
    _patch_path: &Path,
    _dest_path: &Path,
    _logical_path: &str,
    _expected_md5: &str,
    _expected_size: u64,
    _work_dir: Option<&Path>,
) -> Result<ArtifactProof> {
    Err(Error::Message {
        context: "Patch support unavailable: ",
        detail: "griffr-runtime was built without the `hdiff-patch` feature".to_string(),
    })
}

fn apply_patch_entry(
    install_root: &Path,
    stage_root: &Path,
    dest_root: &Path,
    logical_root: &Path,
    entry: &ResourcePatchEntry,
) -> Result<ArtifactProof> {
    let dest_relative = parse_safe_relative_path("patch.json file name", &entry.name)?;
    let dest_path = dest_root.join(&dest_relative);
    let logical_path = logical_root
        .join(&dest_relative)
        .to_string_lossy()
        .replace('\\', "/");
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
        .map_err(|err| Error::Message {
            context: "",
            detail: format!(
                "Failed to patch {} from base {}: {err}",
                entry.name,
                base_relative.display()
            ),
        });
    }

    if candidate_failures.is_empty() {
        return Err(Error::Message {
            context: "VFS error: ",
            detail: format!(
                "patch.json entry {} has no applicable patch candidates",
                entry.name
            ),
        });
    }

    Err(Error::Message {
        context: "VFS error: ",
        detail: format!(
            "patch.json entry {} has no verified base file to patch: {}",
            entry.name,
            candidate_failures.join("; ")
        ),
    })
}

pub(super) fn apply_vfs_patch_entry(
    install_root: &Path,
    stage_root: &Path,
    dest_root: &Path,
    logical_root: &Path,
    entry: &ResourcePatchEntry,
) -> Result<ArtifactProof> {
    let dest_relative = parse_safe_relative_path("patch.json file name", &entry.name)?;
    let dest_path = dest_root.join(&dest_relative);
    let logical_path = logical_root
        .join(&dest_relative)
        .to_string_lossy()
        .replace('\\', "/");
    let expectation = ArtifactExpectation::new(&logical_path, &entry.md5, Some(entry.size));
    if build_issue(&dest_path, &logical_path, &entry.md5, Some(entry.size)).is_none() {
        return verify_artifact(&dest_path, &expectation, ArtifactSource::Existing);
    }

    if let Some(local_path) = entry
        .local_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        return apply_local_patch_entry(
            install_root,
            stage_root,
            dest_root,
            logical_root,
            entry,
            local_path,
        );
    }
    apply_patch_entry(install_root, stage_root, dest_root, logical_root, entry)
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
