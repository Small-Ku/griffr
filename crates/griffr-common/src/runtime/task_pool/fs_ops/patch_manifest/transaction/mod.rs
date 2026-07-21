use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::runtime::patch_transaction::{
    read_patch_plan, write_patch_plan, PatchCheckReport, PatchPlan, PlannedPatchSource,
};
use crate::runtime::task_pool::verify::VerifiedArtifactCache;

mod filesystem;
mod steps;

use filesystem::{commit_top_level_files, prepare_external_vfs_root};
#[cfg(test)]
use steps::ordered_entries;
use steps::{
    apply_planned_entry, apply_remaining_deletes, cleanup_staging, cleanup_transaction,
    commit_deferred_files, delete_unreferenced_paths_before_patch, entry_waves, final_output_paths,
    release_base_if_unused,
};

pub(crate) fn prepare_patch_transaction(
    plan: &PatchPlan,
    commit_callback: Option<&mut dyn FnMut(&Path, usize, usize)>,
) -> Result<()> {
    plan.validate()?;
    write_patch_plan(plan)?;
    prepare_external_vfs_root(plan)?;
    commit_top_level_files(plan, commit_callback)?;
    delete_unreferenced_paths_before_patch(plan)
}

pub(crate) fn apply_patch_transaction_entry(
    plan: &PatchPlan,
    entry_index: usize,
    verification_cache: &VerifiedArtifactCache,
) -> Result<()> {
    let entry = plan.entries.get(entry_index).ok_or_else(|| {
        Error::TaskPool(format!("patch entry index {entry_index} is out of range"))
    })?;
    apply_planned_entry(plan, entry, verification_cache).map_err(|error| {
        Error::Other(format!(
            "Failed to apply patch entry {}: {}",
            entry.name, error
        ))
    })
}

pub(crate) fn release_patch_transaction_base(plan: &PatchPlan, base: &Path) -> Result<()> {
    let delete_set = plan.delete_paths.iter().cloned().collect::<BTreeSet<_>>();
    let outputs = final_output_paths(plan);
    let Some(relative) = steps::relative_install_path(plan, base) else {
        return Ok(());
    };
    if delete_set.contains(&relative) && !outputs.contains(&relative) {
        filesystem::remove_path_if_exists(base)?;
    }
    Ok(())
}

pub(crate) fn apply_patch_transaction_deletes(
    plan: &PatchPlan,
    callback: Option<&mut dyn FnMut(&Path, usize, usize)>,
) -> Result<()> {
    apply_remaining_deletes(plan, callback)
}

pub(crate) fn commit_patch_transaction_deferred(plan: &PatchPlan) -> Result<()> {
    commit_deferred_files(plan)
}

pub(crate) fn cleanup_patch_transaction(plan: &PatchPlan) -> Result<()> {
    cleanup_staging(plan)?;
    cleanup_transaction(plan)
}

pub(crate) fn run_patch_transaction(
    plan: &PatchPlan,
    _report: Option<&PatchCheckReport>,
    commit_callback: Option<&mut dyn FnMut(&Path, usize, usize)>,
    mut patch_callback: Option<&mut dyn FnMut(&str, usize, usize)>,
    delete_callback: Option<&mut dyn FnMut(&Path, usize, usize)>,
    verification_cache: &VerifiedArtifactCache,
) -> Result<()> {
    prepare_patch_transaction(plan, commit_callback)?;

    let delete_set = plan.delete_paths.iter().cloned().collect::<BTreeSet<_>>();
    let outputs = final_output_paths(plan);
    let mut remaining = BTreeMap::<PathBuf, usize>::new();
    for entry in &plan.entries {
        if let PlannedPatchSource::Hdiff { base, .. } = &entry.source {
            *remaining.entry(base.clone()).or_default() += 1;
        }
    }
    let waves = entry_waves(plan)?;
    let total = waves.iter().map(Vec::len).sum::<usize>();
    if total > 0 {
        if let Some(callback) = patch_callback.as_deref_mut() {
            callback("", 0, total);
        }
    }
    let mut completed = 0usize;
    for wave in waves {
        for entry in &wave {
            apply_planned_entry(plan, entry, verification_cache).map_err(|error| {
                Error::Other(format!(
                    "Failed to apply patch entry {}: {}",
                    entry.name, error
                ))
            })?;
            if let PlannedPatchSource::Hdiff { base, .. } = &entry.source {
                release_base_if_unused(plan, base, &mut remaining, &delete_set, &outputs)?;
            }
            completed = completed.saturating_add(1);
            if let Some(callback) = patch_callback.as_deref_mut() {
                let logical_path = plan.vfs_base_path.join(&entry.name);
                callback(
                    &logical_path.to_string_lossy().replace('\\', "/"),
                    completed,
                    total,
                );
            }
        }
    }
    apply_remaining_deletes(plan, delete_callback)?;
    commit_deferred_files(plan)?;
    cleanup_staging(plan)?;
    cleanup_transaction(plan)
}

pub(crate) fn resume_patch_transaction(
    install_root: &Path,
    commit_callback: Option<&mut dyn FnMut(&Path, usize, usize)>,
    patch_callback: Option<&mut dyn FnMut(&str, usize, usize)>,
    delete_callback: Option<&mut dyn FnMut(&Path, usize, usize)>,
) -> Result<()> {
    let plan = read_patch_plan(install_root)?;
    let verification_cache = VerifiedArtifactCache::default();
    run_patch_transaction(
        &plan,
        None,
        commit_callback,
        patch_callback,
        delete_callback,
        &verification_cache,
    )
}

#[cfg(test)]
mod tests;
