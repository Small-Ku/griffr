use std::path::{Path, PathBuf};

use griffr_common::api::types::GetLatestGameResponse;
use griffr_common::runtime::{selected_archive_plan, UpdatePackageKind};

use crate::ui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ArchiveAcquireMode {
    DownloadIfMissing,
    RequireExisting,
}

pub(super) fn describe_update_package_selection(
    version_info: &GetLatestGameResponse,
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

#[allow(clippy::too_many_arguments)]
pub(super) fn build_update_dry_run_plan(
    install_path: &Path,
    current_version: &str,
    version_info: &GetLatestGameResponse,
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
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    } else if let Some(plan) = selected_archive_plan(version_info, package_kind) {
        lines.push(format!(
            "Would process {} archive parts: {} declared parts ({})",
            plan.label,
            plan.part_count,
            ui::format_bytes(plan.total_size)
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
            lines.push(
                "Would stream archive ranges during extraction, retain them, fill only missing gaps, verify each full volume, and keep the package archives."
                    .to_string(),
            );
        } else {
            lines.push(
                "Would stream required package byte ranges, verify extracted files, and remove the range cache after commit."
                    .to_string(),
            );
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
