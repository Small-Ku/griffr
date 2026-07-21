use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::download::extractor::ArchiveIndex;
use crate::error::{Error, Result};
use crate::runtime::task_pool::fs_ops::{
    parse_delete_files_manifest, path_safety::parse_safe_relative_path, storage_volume_group_key,
};
use crate::runtime::task_pool::verify::VerifiedArtifactCache;
use crate::runtime::{
    DELETE_FILES_MANIFEST_NAME, PATCH_DIFF_STAGE_DIR, PATCH_FILES_STAGE_DIR, PATCH_MANIFEST_NAME,
    PATCH_STAGE_DIR,
};

use super::{
    available_space, read_patch_storage_layout, space_use::simulate_space_peaks, PatchApplyOptions,
    PatchCheckReport, PatchPlan, PlannedPatchEntry, PlannedPatchSource,
};

fn normalized_archive_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn archive_payload_path(stage_subdir: &str, raw: &str) -> Result<PathBuf> {
    let relative = parse_safe_relative_path("patch archive payload", raw)?;
    let stage_subdir_path = Path::new(stage_subdir);
    if relative.starts_with(PATCH_STAGE_DIR) {
        return Ok(relative);
    }
    if relative.starts_with(stage_subdir_path) {
        return Ok(Path::new(PATCH_STAGE_DIR).join(relative));
    }
    Ok(Path::new(PATCH_STAGE_DIR)
        .join(stage_subdir_path)
        .join(relative))
}

fn is_patch_archive_control_path(path: &Path) -> bool {
    path == Path::new(PATCH_MANIFEST_NAME)
        || path == Path::new(DELETE_FILES_MANIFEST_NAME)
        || path.starts_with(PATCH_STAGE_DIR)
}

fn metadata_len(path: &Path) -> u64 {
    std::fs::metadata(path)
        .ok()
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn directory_size(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    let mut total = 0u64;
    let mut pending = vec![path.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).map_err(|source| Error::ReadDirFailed {
            path: directory.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| Error::ReadDirFailed {
                path: directory.clone(),
                source,
            })?;
            let entry_path = entry.path();
            let metadata =
                std::fs::symlink_metadata(&entry_path).map_err(|source| Error::StatFailed {
                    path: entry_path.clone(),
                    source,
                })?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                pending.push(entry_path);
            } else if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    Ok(total)
}

fn directory_is_empty(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(true);
    }
    let mut entries = std::fs::read_dir(path).map_err(|source| Error::ReadDirFailed {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(entries.next().is_none())
}

fn same_link_target(link: &Path, target: &Path) -> bool {
    std::fs::read_link(link)
        .ok()
        .map(|existing| {
            let resolved = if existing.is_absolute() {
                existing
            } else {
                link.parent().unwrap_or(Path::new(".")).join(existing)
            };
            resolved == target
        })
        .unwrap_or(false)
}

fn verify_space_requirements(
    install_root: &Path,
    install_bytes: u64,
    install_available: Option<u64>,
    external: Option<(&Path, u64, Option<u64>)>,
    work: Option<(&Path, u64, Option<u64>)>,
) -> Result<()> {
    let mut groups = vec![(
        install_root.to_path_buf(),
        install_bytes,
        install_available,
        vec!["install"],
    )];
    for (path, required, available, label) in external
        .map(|(path, required, available)| (path, required, available, "VFS"))
        .into_iter()
        .chain(work.map(|(path, required, available)| (path, required, available, "work")))
    {
        if required == 0 {
            continue;
        }
        let volume_key = storage_volume_group_key(path);
        if let Some(group) = groups
            .iter_mut()
            .find(|(group_path, ..)| storage_volume_group_key(group_path) == volume_key)
        {
            // Each role already reports the simulated peak for this physical
            // volume, so aliases use max rather than double-counting it.
            group.1 = group.1.max(required);
            group.3.push(label);
        } else {
            groups.push((path.to_path_buf(), required, available, vec![label]));
        }
    }

    for (path, required, available, labels) in groups {
        if available.is_some_and(|available| available < required) {
            return Err(Error::Vfs(format!(
                "The patch check needs about {} bytes of peak free space for {} data on the volume that contains {}, but only {} bytes are free",
                required,
                labels.join(" + "),
                path.display(),
                available.unwrap_or_default()
            )));
        }
    }
    Ok(())
}

pub fn check_patch_archives(
    volumes: Vec<PathBuf>,
    install_root: &Path,
    password: Option<&str>,
    options: &PatchApplyOptions,
) -> Result<PatchCheckReport> {
    let options = options.resolved_for_install(install_root)?;
    let extractor = crate::download::extractor::MultiVolumeExtractor::new(volumes)?;
    let archive_index = extractor.read_patch_payload(password)?;
    let stage_root = options
        .work_dir
        .as_deref()
        .unwrap_or_else(|| install_root.parent().unwrap_or(install_root))
        .join(".griffr-check");
    let (_, report) = build_patch_plan(install_root, &stage_root, &archive_index, &options)?;
    Ok(report)
}

pub(crate) fn build_patch_plan(
    install_root: &Path,
    stage_root: &Path,
    archive_index: &ArchiveIndex,
    options: &PatchApplyOptions,
) -> Result<(PatchPlan, PatchCheckReport)> {
    build_patch_plan_with_cache(
        install_root,
        stage_root,
        archive_index,
        options,
        &VerifiedArtifactCache::default(),
    )
}

pub(crate) fn build_patch_plan_with_cache(
    install_root: &Path,
    stage_root: &Path,
    archive_index: &ArchiveIndex,
    options: &PatchApplyOptions,
    verification_cache: &VerifiedArtifactCache,
) -> Result<(PatchPlan, PatchCheckReport)> {
    let mut options = options.resolved_for_install(install_root)?;
    if let Some(storage_layout) = read_patch_storage_layout(install_root)? {
        match options.external_vfs_root.as_ref() {
            Some(requested) if requested != &storage_layout.external_vfs_root => {
                return Err(Error::Config(format!(
                    "Install already manages VFS storage at {}; requested external root {} does not match",
                    storage_layout.external_vfs_root.display(),
                    requested.display()
                )));
            }
            Some(_) => {}
            None => options.external_vfs_root = Some(storage_layout.external_vfs_root),
        }
    }
    let manifest = archive_index
        .patch_manifest
        .as_ref()
        .ok_or_else(|| Error::Vfs(format!("Patch archive is missing {PATCH_MANIFEST_NAME}")))?;
    let vfs_base_path =
        parse_safe_relative_path("patch.json vfs_base_path", manifest.vfs_base_path.trim())?;
    let logical_vfs_destination = install_root.join(&vfs_base_path);
    let vfs_destination = options
        .external_vfs_root
        .clone()
        .unwrap_or_else(|| logical_vfs_destination.clone());
    if options.external_vfs_root.is_some() {
        if vfs_destination == logical_vfs_destination || vfs_destination.starts_with(install_root) {
            return Err(Error::Vfs(format!(
                "External VFS root {} must be outside the install root and differ from {}",
                vfs_destination.display(),
                logical_vfs_destination.display()
            )));
        }
        if !directory_is_empty(&vfs_destination)?
            && !same_link_target(&logical_vfs_destination, &vfs_destination)
        {
            return Err(Error::Vfs(format!(
                "External VFS root {} is not empty and is not the current target of {}",
                vfs_destination.display(),
                logical_vfs_destination.display()
            )));
        }
    }
    let delete_paths =
        parse_delete_files_manifest(archive_index.delete_manifest.as_deref().unwrap_or_default())?;
    let delete_set = delete_paths.iter().cloned().collect::<BTreeSet<_>>();
    let archive_entries = &archive_index.entries;
    let top_level_growth = archive_entries
        .iter()
        .filter_map(|(name, size)| {
            let relative = PathBuf::from(name);
            if is_patch_archive_control_path(&relative) {
                return None;
            }
            let existing = metadata_len(&install_root.join(&relative));
            Some(size.saturating_sub(existing))
        })
        .sum::<u64>();
    let mut entries = Vec::with_capacity(manifest.files.len());
    let mut missing = Vec::new();
    let mut final_delta: i128 = 0;

    for entry in &manifest.files {
        let relative = parse_safe_relative_path("patch.json file name", &entry.name)?;
        let destination = vfs_destination.join(&relative);
        let logical_destination = logical_vfs_destination.join(&relative);
        let logical = relative.to_string_lossy().replace('\\', "/");
        let existing_path = if options.external_vfs_root.is_some() {
            &logical_destination
        } else {
            &destination
        };
        let existing_size = metadata_len(existing_path);
        final_delta += i128::from(entry.size) - i128::from(existing_size);

        let source = if verification_cache
            .build_issue(existing_path, &logical, &entry.md5, Some(entry.size))
            .is_none()
        {
            PlannedPatchSource::AlreadyPresent
        } else if let Some(local_path) = entry.effective_local_path() {
            let archive_path = archive_payload_path(PATCH_FILES_STAGE_DIR, local_path)?;
            if !archive_entries.contains_key(&normalized_archive_path(&archive_path)) {
                missing.push(format!(
                    "{} (missing local payload {})",
                    entry.name,
                    archive_path.display()
                ));
                continue;
            }
            PlannedPatchSource::Local {
                payload: archive_path,
            }
        } else {
            let mut selected = None;
            let mut failures = Vec::new();
            for diff in &entry.patch {
                let base_relative =
                    parse_safe_relative_path("patch.json base_file", diff.effective_base_file())?;
                let verified_base = logical_vfs_destination.join(&base_relative);
                let planned_base = vfs_destination.join(&base_relative);
                let base_logical = base_relative.to_string_lossy().replace('\\', "/");
                let payload =
                    archive_payload_path(PATCH_DIFF_STAGE_DIR, diff.effective_patch_path())?;
                if verification_cache
                    .build_issue(
                        &verified_base,
                        &base_logical,
                        &diff.base_md5,
                        Some(diff.base_size),
                    )
                    .is_some()
                {
                    failures.push(format!("{} (base mismatch)", base_relative.display()));
                    continue;
                }
                if !archive_entries.contains_key(&normalized_archive_path(&payload)) {
                    failures.push(format!("{} (payload missing)", payload.display()));
                    continue;
                }
                selected = Some(PlannedPatchSource::Hdiff {
                    base: planned_base,
                    payload,
                    base_md5: diff.base_md5.clone(),
                    base_size: diff.base_size,
                });
                break;
            }
            match selected {
                Some(source) => source,
                None => {
                    missing.push(format!(
                        "{} ({})",
                        entry.name,
                        if failures.is_empty() {
                            "no patch candidates".to_string()
                        } else {
                            failures.join("; ")
                        }
                    ));
                    continue;
                }
            }
        };
        entries.push(PlannedPatchEntry {
            name: entry.name.clone(),
            destination,
            expected_md5: entry.md5.clone(),
            expected_size: entry.size,
            source,
        });
    }

    if !missing.is_empty() {
        return Err(Error::Vfs(format!(
            "The patch check found entries that cannot be repaired: {}",
            missing.join(", ")
        )));
    }

    let logical_outputs = entries
        .iter()
        .map(|entry| {
            install_root
                .join(&vfs_base_path)
                .join(Path::new(&entry.name))
        })
        .collect::<BTreeSet<_>>();
    let conflicting_deletes = delete_paths
        .iter()
        .filter(|relative| logical_outputs.contains(&install_root.join(relative)))
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    if !conflicting_deletes.is_empty() {
        return Err(Error::Vfs(format!(
            "Delete manifest conflicts with patch outputs: {}",
            conflicting_deletes.join(", ")
        )));
    }

    let vfs_growth = final_delta.max(0) as u64;
    let relocating_vfs_bytes = if options.external_vfs_root.is_some()
        && !same_link_target(&logical_vfs_destination, &vfs_destination)
    {
        directory_size(&logical_vfs_destination)?
    } else {
        0
    };
    let delete_bytes = delete_set
        .iter()
        .map(|relative| metadata_len(&install_root.join(relative)))
        .sum::<u64>();
    let final_growth = top_level_growth
        .saturating_add(vfs_growth)
        .saturating_sub(delete_bytes);

    let deferred_paths = [PathBuf::from("config.ini")]
        .into_iter()
        .filter(|path| archive_entries.contains_key(&normalized_archive_path(path)))
        .collect::<Vec<_>>();
    let plan = PatchPlan {
        schema_version: PatchPlan::SCHEMA_VERSION,
        install_root: install_root.to_path_buf(),
        stage_root: stage_root.to_path_buf(),
        vfs_base_path,
        vfs_destination,
        work_dir: options.work_dir.clone(),
        entries,
        delete_paths,
        deferred_paths,
    };
    plan.validate()?;
    let peaks = simulate_space_peaks(
        &plan,
        archive_entries,
        archive_index.total_uncompressed_bytes,
        relocating_vfs_bytes,
    )?;
    let install_peak = peaks.install;
    let vfs_peak = if options.external_vfs_root.is_some() {
        peaks.vfs
    } else {
        install_peak
    };
    let work_bytes = if options.work_dir.is_some() {
        peaks.work
    } else {
        0
    };
    let available_install_bytes = available_space(install_root)?;
    let available_vfs_bytes = match options.external_vfs_root.as_deref() {
        Some(path) => available_space(path)?,
        None => available_install_bytes,
    };
    let available_work_bytes = match options.work_dir.as_deref() {
        Some(path) => available_space(path)?,
        None => available_install_bytes,
    };
    verify_space_requirements(
        install_root,
        install_peak,
        available_install_bytes,
        options
            .external_vfs_root
            .as_deref()
            .map(|path| (path, vfs_peak, available_vfs_bytes)),
        options
            .work_dir
            .as_deref()
            .map(|path| (path, work_bytes, available_work_bytes)),
    )?;

    let report = PatchCheckReport {
        archive_uncompressed_bytes: archive_index.total_uncompressed_bytes,
        estimated_final_growth_bytes: final_growth,
        estimated_install_peak_bytes: install_peak,
        estimated_vfs_peak_bytes: vfs_peak,
        estimated_work_bytes: work_bytes,
        available_install_bytes,
        available_vfs_bytes,
        available_work_bytes,
        patch_entries: plan.entries.len(),
        delete_entries: plan.delete_paths.len(),
    };
    Ok((plan, report))
}
