use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::runtime::patch_transaction::{
    PatchExecutionPlan, PlannedPatchEntry, PlannedPatchSource, PATCH_DEFERRED_DIR,
    PATCH_TRANSACTION_DIR,
};
use crate::runtime::task_pool::verify::build_issue;

use super::super::super::extract::move_path_replace_cross_volume;
use super::super::apply::{apply_hdiff_patch, verify_patch_output};
use super::filesystem::remove_path_if_exists;

pub(super) fn apply_planned_entry(
    plan: &PatchExecutionPlan,
    entry: &PlannedPatchEntry,
) -> Result<()> {
    if build_issue(
        &entry.destination,
        &entry.name,
        &entry.expected_md5,
        Some(entry.expected_size),
    )
    .is_none()
    {
        return Ok(());
    }
    match &entry.source {
        PlannedPatchSource::AlreadyPresent => Err(Error::Vfs(format!(
            "Patch output {} no longer matches its completed state",
            entry.destination.display()
        ))),
        PlannedPatchSource::Local { payload } => {
            let source = plan.stage_root.join(payload);
            if !source.is_file() {
                return Err(Error::Vfs(format!(
                    "Missing local patch payload {} for {}",
                    source.display(),
                    entry.name
                )));
            }
            if let Some(parent) = entry.destination.parent() {
                std::fs::create_dir_all(parent).map_err(|source_error| Error::CreateDirFailed {
                    path: parent.to_path_buf(),
                    source: source_error,
                })?;
            }
            move_path_replace_cross_volume(&source, &entry.destination)?;
            verify_patch_output(
                &entry.destination,
                &entry.name,
                &entry.expected_md5,
                entry.expected_size,
            )
        }
        PlannedPatchSource::Hdiff {
            base,
            payload,
            base_md5,
            base_size,
        } => {
            let base_logical = relative_install_path(plan, base)
                .unwrap_or_else(|| base.to_path_buf())
                .to_string_lossy()
                .replace('\\', "/");
            if let Some(issue) = build_issue(base, &base_logical, base_md5, Some(*base_size)) {
                return Err(Error::Vfs(format!(
                    "Patch base {} failed verification before applying {}: {:?}",
                    base.display(),
                    entry.name,
                    issue.kind
                )));
            }
            let payload_path = plan.stage_root.join(payload);
            if !payload_path.is_file() {
                return Err(Error::Vfs(format!(
                    "Missing HDiff payload {} for {}",
                    payload_path.display(),
                    entry.name
                )));
            }
            apply_hdiff_patch(
                base,
                &payload_path,
                &entry.destination,
                &entry.name,
                &entry.expected_md5,
                entry.expected_size,
                plan.work_dir.as_deref(),
            )?;
            remove_path_if_exists(&payload_path)
        }
    }
}

pub(super) fn logical_vfs_root(plan: &PatchExecutionPlan) -> PathBuf {
    plan.install_root.join(&plan.vfs_base_path)
}

pub(super) fn physical_delete_path(plan: &PatchExecutionPlan, relative: &Path) -> PathBuf {
    if plan.vfs_destination != logical_vfs_root(plan) {
        if let Ok(vfs_relative) = relative.strip_prefix(&plan.vfs_base_path) {
            return plan.vfs_destination.join(vfs_relative);
        }
    }
    plan.install_root.join(relative)
}

pub(super) fn relative_install_path(plan: &PatchExecutionPlan, path: &Path) -> Option<PathBuf> {
    if let Ok(vfs_relative) = path.strip_prefix(&plan.vfs_destination) {
        return Some(plan.vfs_base_path.join(vfs_relative));
    }
    path.strip_prefix(&plan.install_root)
        .ok()
        .map(Path::to_path_buf)
}

pub(super) fn final_output_paths(plan: &PatchExecutionPlan) -> BTreeSet<PathBuf> {
    plan.entries
        .iter()
        .map(|entry| plan.vfs_base_path.join(&entry.name))
        .collect()
}

pub(super) fn entry_waves(plan: &PatchExecutionPlan) -> Result<Vec<Vec<&PlannedPatchEntry>>> {
    let mut destination_writers = BTreeMap::new();
    for (index, entry) in plan.entries.iter().enumerate() {
        let logical_destination = plan
            .install_root
            .join(&plan.vfs_base_path)
            .join(&entry.name);
        let aliases = [entry.destination.clone(), logical_destination]
            .into_iter()
            .collect::<BTreeSet<_>>();
        for destination in aliases {
            if destination_writers
                .insert(destination.clone(), index)
                .is_some()
            {
                return Err(Error::Vfs(format!(
                    "Patch plan has multiple writers for {}",
                    destination.display()
                )));
            }
        }
    }

    let mut outgoing = vec![BTreeSet::<usize>::new(); plan.entries.len()];
    let mut indegree = vec![0usize; plan.entries.len()];
    for (consumer_index, entry) in plan.entries.iter().enumerate() {
        let PlannedPatchSource::Hdiff { base, .. } = &entry.source else {
            continue;
        };
        let Some(&writer_index) = destination_writers.get(base) else {
            continue;
        };
        if writer_index == consumer_index {
            continue;
        }
        // The consumer must run before a writer destructively replaces its base.
        if outgoing[consumer_index].insert(writer_index) {
            indegree[writer_index] = indegree[writer_index].saturating_add(1);
        }
    }

    let mut ready = indegree
        .iter()
        .enumerate()
        .filter_map(|(index, degree)| (*degree == 0).then_some(index))
        .collect::<BTreeSet<_>>();
    let mut waves = Vec::new();
    let mut completed = 0usize;
    while !ready.is_empty() {
        let wave = ready.iter().copied().collect::<Vec<_>>();
        ready.clear();
        completed = completed.saturating_add(wave.len());
        for index in &wave {
            for dependent in outgoing[*index].iter().copied() {
                indegree[dependent] = indegree[dependent].saturating_sub(1);
                if indegree[dependent] == 0 {
                    ready.insert(dependent);
                }
            }
        }
        waves.push(
            wave.into_iter()
                .map(|index| &plan.entries[index])
                .collect(),
        );
    }
    if completed != plan.entries.len() {
        let blocked = indegree
            .iter()
            .enumerate()
            .filter_map(|(index, degree)| {
                (*degree > 0).then_some(plan.entries[index].name.as_str())
            })
            .collect::<Vec<_>>();
        return Err(Error::Vfs(format!(
            "Patch dependency graph contains a destructive overwrite cycle: {}",
            blocked.join(", ")
        )));
    }
    Ok(waves)
}

pub(super) fn ordered_entries(plan: &PatchExecutionPlan) -> Result<Vec<&PlannedPatchEntry>> {
    Ok(entry_waves(plan)?.into_iter().flatten().collect())
}

pub(super) fn delete_unreferenced_paths_before_patch(plan: &PatchExecutionPlan) -> Result<()> {
    let bases = plan
        .entries
        .iter()
        .filter_map(|entry| match &entry.source {
            PlannedPatchSource::Hdiff { base, .. } => Some(base.clone()),
            PlannedPatchSource::AlreadyPresent | PlannedPatchSource::Local { .. } => None,
        })
        .collect::<BTreeSet<_>>();
    let outputs = plan
        .entries
        .iter()
        .map(|entry| entry.destination.clone())
        .collect::<BTreeSet<_>>();
    for relative in &plan.delete_paths {
        let path = physical_delete_path(plan, relative);
        if !bases.contains(&path) && !outputs.contains(&path) {
            remove_path_if_exists(&path)?;
        }
    }
    Ok(())
}

pub(super) fn release_base_if_unused(
    plan: &PatchExecutionPlan,
    base: &Path,
    remaining: &mut BTreeMap<PathBuf, usize>,
    delete_set: &BTreeSet<PathBuf>,
    outputs: &BTreeSet<PathBuf>,
) -> Result<()> {
    let Some(count) = remaining.get_mut(base) else {
        return Ok(());
    };
    *count = count.saturating_sub(1);
    if *count != 0 {
        return Ok(());
    }
    let Some(relative) = relative_install_path(plan, base) else {
        return Ok(());
    };
    if delete_set.contains(&relative) && !outputs.contains(&relative) {
        remove_path_if_exists(base)?;
    }
    Ok(())
}

pub(super) fn apply_remaining_deletes(
    plan: &PatchExecutionPlan,
    mut callback: Option<&mut dyn FnMut(&Path, usize, usize)>,
) -> Result<()> {
    let total = plan.delete_paths.len();
    if total > 0 {
        if let Some(callback) = callback.as_deref_mut() {
            callback(Path::new("."), 0, total);
        }
    }
    let outputs = final_output_paths(plan);
    for (index, relative) in plan.delete_paths.iter().enumerate() {
        if !outputs.contains(relative) {
            remove_path_if_exists(&physical_delete_path(plan, relative))?;
        }
        if let Some(callback) = callback.as_deref_mut() {
            callback(relative, index + 1, total);
        }
    }
    Ok(())
}

pub(super) fn commit_deferred_files(plan: &PatchExecutionPlan) -> Result<()> {
    let deferred_root = plan
        .install_root
        .join(PATCH_TRANSACTION_DIR)
        .join(PATCH_DEFERRED_DIR);
    for relative in &plan.deferred_paths {
        let source = deferred_root.join(relative);
        if !source.is_file() {
            continue;
        }
        let target = plan.install_root.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|source_error| Error::CreateDirFailed {
                path: parent.to_path_buf(),
                source: source_error,
            })?;
        }
        move_path_replace_cross_volume(&source, &target)?;
    }
    Ok(())
}

pub(super) fn cleanup_staging(plan: &PatchExecutionPlan) -> Result<()> {
    if plan.stage_root.exists() {
        std::fs::remove_dir_all(&plan.stage_root).map_err(|source| Error::RemoveFailed {
            path: plan.stage_root.clone(),
            source,
        })?;
    }
    Ok(())
}

pub(super) fn cleanup_transaction(plan: &PatchExecutionPlan) -> Result<()> {
    let transaction_root = plan.install_root.join(PATCH_TRANSACTION_DIR);
    if transaction_root.exists() {
        std::fs::remove_dir_all(&transaction_root).map_err(|source| Error::RemoveFailed {
            path: transaction_root,
            source,
        })?;
    }
    Ok(())
}
