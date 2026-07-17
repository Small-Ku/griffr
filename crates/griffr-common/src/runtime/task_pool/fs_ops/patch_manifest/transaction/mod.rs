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
    commit_deferred_files, delete_unreferenced_paths_before_patch, entry_waves, final_output_paths,
    release_base_if_unused,
};
#[cfg(test)]
use operations::ordered_entries;

pub(crate) fn execute_patch_transaction(
    plan: &PatchExecutionPlan,
    _report: Option<&PatchPreflightReport>,
    commit_callback: Option<&mut dyn FnMut(&Path, usize, usize)>,
    mut patch_callback: Option<&mut dyn FnMut(&str, usize, usize)>,
    delete_callback: Option<&mut dyn FnMut(&Path, usize, usize)>,
    patch_slots: usize,
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
    let waves = entry_waves(plan)?;
    let total = waves.iter().map(Vec::len).sum::<usize>();
    if total > 0 {
        if let Some(callback) = patch_callback.as_deref_mut() {
            callback("", 0, total);
        }
    }
    let mut completed = 0usize;
    let patch_slots = patch_slots.max(1);
    for wave in waves {
        for chunk in wave.chunks(patch_slots) {
            let results = std::thread::scope(|scope| {
                chunk
                    .iter()
                    .map(|entry| {
                        scope.spawn(move || {
                            apply_planned_entry(plan, entry).map_err(|error| error.to_string())
                        })
                    })
                    .collect::<Vec<_>>()
                    .into_iter()
                    .map(|handle| {
                        handle.join().unwrap_or_else(|_| {
                            Err("patch worker panicked while applying an entry".to_string())
                        })
                    })
                    .collect::<Vec<_>>()
            });

            for (entry, result) in chunk.iter().zip(results) {
                result.map_err(|error| {
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
    patch_slots: usize,
) -> Result<()> {
    let plan = read_patch_execution_plan(install_root)?;
    execute_patch_transaction(
        &plan,
        None,
        commit_callback,
        patch_callback,
        delete_callback,
        patch_slots,
    )
}

#[cfg(test)]
mod tests;
