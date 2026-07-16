use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::runtime::patch_transaction::{
    read_patch_execution_plan, write_patch_execution_plan, PatchExecutionPlan,
    PatchPreflightReport, PlannedPatchSource,
};

mod filesystem;
mod operations;

use filesystem::{commit_top_level_files, prepare_external_vfs_root};
use operations::{
    apply_planned_entry, apply_remaining_deletes, cleanup_staging, cleanup_transaction,
    commit_deferred_files, delete_unreferenced_paths_before_patch, final_output_paths,
    ordered_entries, release_base_if_unused,
};

pub(crate) fn execute_patch_transaction(
    plan: &PatchExecutionPlan,
    _report: Option<&PatchPreflightReport>,
    commit_callback: Option<&mut dyn FnMut(&Path, usize, usize)>,
    mut patch_callback: Option<&mut dyn FnMut(&str, usize, usize)>,
    delete_callback: Option<&mut dyn FnMut(&Path, usize, usize)>,
) -> Result<()> {
    plan.validate()?;
    write_patch_execution_plan(plan)?;
    prepare_external_vfs_root(plan)?;
    commit_top_level_files(plan, commit_callback)?;
    delete_unreferenced_paths_before_patch(plan)?;

    let delete_set = plan.delete_paths.iter().cloned().collect::<BTreeSet<_>>();
    let outputs = final_output_paths(plan);
    let mut remaining = BTreeMap::<PathBuf, usize>::new();
    for entry in &plan.entries {
        if let PlannedPatchSource::Hdiff { base, .. } = &entry.source {
            *remaining.entry(base.clone()).or_default() += 1;
        }
    }
    let ordered_entries = ordered_entries(plan)?;
    let total = ordered_entries.len();
    if total > 0 {
        if let Some(callback) = patch_callback.as_deref_mut() {
            callback("", 0, total);
        }
    }
    for (index, entry) in ordered_entries.into_iter().enumerate() {
        apply_planned_entry(plan, entry).map_err(|error| {
            Error::Other(format!(
                "Failed to apply patch entry {}: {}",
                entry.name, error
            ))
        })?;
        if let PlannedPatchSource::Hdiff { base, .. } = &entry.source {
            release_base_if_unused(plan, base, &mut remaining, &delete_set, &outputs)?;
        }
        if let Some(callback) = patch_callback.as_deref_mut() {
            callback(&entry.name, index + 1, total);
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
    let plan = read_patch_execution_plan(install_root)?;
    execute_patch_transaction(
        &plan,
        None,
        commit_callback,
        patch_callback,
        delete_callback,
    )
}

#[cfg(test)]
mod tests;
