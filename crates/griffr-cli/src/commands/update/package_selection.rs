use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::ui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UpdatePackageKind {
    Patch,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ArchiveAcquireMode {
    DownloadIfMissing,
    RequireExisting,
}

pub(super) fn strip_url_query(s: &str) -> &str {
    s.split('?').next().unwrap_or(s)
}

pub(super) fn archive_base_from_url(url: &str) -> Option<String> {
    let filename = url.split('/').next_back()?;
    let filename = strip_url_query(filename);

    if let Some(idx) = filename.rfind(".zip.") {
        let suffix = &filename[(idx + ".zip.".len())..];
        if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
            return Some(filename[..idx].to_string());
        }
    }

    if let Some(stem) = filename.strip_suffix(".zip") {
        return Some(stem.to_string());
    }

    None
}

pub(super) fn choose_update_package(
    version_info: &griffr_common::api::types::GetLatestGameResponse,
    current_version: Option<&str>,
) -> Result<UpdatePackageKind> {
    let patch_matches_installed_version = current_version
        .zip(Some(version_info.request_version.as_str()))
        .is_some_and(|(current, requested)| !current.is_empty() && current == requested);

    if version_info.has_patch_package() && patch_matches_installed_version {
        return Ok(UpdatePackageKind::Patch);
    }

    if version_info.has_full_package() {
        return Ok(UpdatePackageKind::Full);
    }

    if version_info.has_patch_package() {
        anyhow::bail!(
            "Patch package was returned for request version '{}' but the installed version is {:?}",
            version_info.request_version,
            current_version
        );
    }

    anyhow::bail!(
        "Update is available but the API returned neither patch nor full package archives"
    )
}

pub(super) fn describe_update_package_selection(
    version_info: &griffr_common::api::types::GetLatestGameResponse,
    current_version: Option<&str>,
    package_kind: UpdatePackageKind,
    force_full_package: bool,
) -> String {
    if force_full_package {
        return "Using full package (--full-package set; patch selection bypassed).".to_string();
    }

    match package_kind {
        UpdatePackageKind::Patch => {
            let installed = current_version.unwrap_or("<unknown>");
            format!(
                "Using patch package: installed version '{}' matches request_version '{}'.",
                installed, version_info.request_version
            )
        }
        UpdatePackageKind::Full => {
            if version_info.patch.is_some() {
                let installed = current_version.unwrap_or("<unknown>");
                format!(
                    "Using full package: patch request_version '{}' does not match installed version '{}'.",
                    version_info.request_version, installed
                )
            } else {
                "Using full package: API did not provide a compatible patch package.".to_string()
            }
        }
    }
}

pub(super) fn selected_archive_plan(
    version_info: &griffr_common::api::types::GetLatestGameResponse,
    package_kind: UpdatePackageKind,
) -> Option<(&'static str, usize, u64)> {
    match package_kind {
        UpdatePackageKind::Patch => version_info.patch.as_ref().map(|patch| {
            let count = patch.patches.len();
            let total_size = patch.patches.iter().map(|p| p.size()).sum();
            ("patch", count, total_size)
        }),
        UpdatePackageKind::Full => version_info.pkg.as_ref().map(|pkg| {
            let count = pkg.packs.len();
            let total_size = pkg.packs.iter().map(|p| p.size()).sum();
            ("full", count, total_size)
        }),
    }
}

pub(super) fn build_update_dry_run_plan(
    install_path: &Path,
    current_version: &str,
    version_info: &griffr_common::api::types::GetLatestGameResponse,
    package_kind: UpdatePackageKind,
    reuse_paths: &[PathBuf],
    use_predownload: bool,
    predownload_stage_dir: Option<&Path>,
    skip_verify: bool,
    skip_vfs: bool,
    keep_pack_archives: bool,
    force_full_package: bool,
) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(format!(
        "Would update {} from {} to {} using {:?}",
        install_path.display(),
        current_version,
        version_info.version,
        package_kind
    ));
    lines.push(describe_update_package_selection(
        version_info,
        Some(current_version),
        package_kind,
        force_full_package,
    ));

    if !reuse_paths.is_empty() {
        lines.push(format!(
            "Would apply update via local file reuse from: {}",
            reuse_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    } else if let Some((label, archive_count, total_size)) =
        selected_archive_plan(version_info, package_kind)
    {
        lines.push(format!(
            "Would download {label} archive parts: {archive_count} ({})",
            ui::format_bytes(total_size)
        ));
        if use_predownload && package_kind == UpdatePackageKind::Patch {
            if let Some(stage_dir) = predownload_stage_dir {
                lines.push(format!(
                    "Would reuse matching staged predownload archives from {} before downloading missing patch parts.",
                    stage_dir.display()
                ));
            }
        }
        if keep_pack_archives {
            lines.push("Would keep downloaded package archives after extraction.".to_string());
        } else {
            lines.push("Would delete package archives after successful extraction.".to_string());
        }
    } else {
        lines.push("Would download update archives based on API response.".to_string());
    }

    if skip_verify {
        lines.push("Would skip post-update integrity verification (--skip-verify).".to_string());
    } else {
        lines.push("Would run post-update integrity verification.".to_string());
    }

    if skip_vfs {
        lines.push("Would skip VFS resource sync (--skip-vfs).".to_string());
    } else {
        lines.push(
            "Would probe the target's launcher resource-index API and sync VFS resources when available."
                .to_string(),
        );
    }

    lines
}
