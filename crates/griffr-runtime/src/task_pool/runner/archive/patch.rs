use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::error::Result;
use crate::task_pool::fs_ops::{
    apply_patch_deletes, apply_patch_entry, clean_patch_apply, commit_deferred_patch_files,
    prepare_patch_apply, release_patch_base,
};
use crate::task_pool::graph::{GraphExpansion, TaskRun};
use crate::task_pool::types::{ArchiveWork, PatchApplyWork, Task, WorkerEvent};
use crate::{entry_dependency_indices, entry_wave_indices, ArtifactSource, PlannedPatchSource};

pub(crate) fn schedule_patch_apply(
    archive: Arc<ArchiveWork>,
    patch: Arc<PatchApplyWork>,
) -> TaskRun {
    let result: Result<GraphExpansion> = (|| {
        let plan = patch.plan();
        let dependencies = entry_dependency_indices(plan)?;
        let waves = entry_wave_indices(plan)?;
        let mut expansion = GraphExpansion::new();
        let prepare = expansion.add_root(Task::PreparePatchApply {
            patch: patch.clone(),
        });
        let mut entry_nodes = vec![None; plan.entries.len()];
        let mut base_consumers = BTreeMap::<PathBuf, Vec<_>>::new();

        for wave in waves {
            for entry_index in wave {
                let mut prerequisites = vec![prepare];
                prerequisites.extend(
                    dependencies[entry_index]
                        .iter()
                        .filter_map(|index| entry_nodes[*index]),
                );
                let node = expansion.add_task(
                    Task::ApplyPatchEntry {
                        patch: patch.clone(),
                        entry_index,
                    },
                    prerequisites,
                )?;
                entry_nodes[entry_index] = Some(node);
                if let PlannedPatchSource::Hdiff { base, .. } = &plan.entries[entry_index].source {
                    base_consumers.entry(base.clone()).or_default().push(node);
                }
            }
        }

        let mut final_entry_nodes = entry_nodes.into_iter().flatten().collect::<Vec<_>>();
        for (base, consumers) in base_consumers {
            let release = expansion.add_task(
                Task::ReleasePatchBase {
                    patch: patch.clone(),
                    base,
                },
                consumers,
            )?;
            final_entry_nodes.push(release);
        }
        if final_entry_nodes.is_empty() {
            final_entry_nodes.push(prepare);
        }
        let deletes = expansion.add_task(
            Task::ApplyPatchDeletes {
                patch: patch.clone(),
            },
            final_entry_nodes,
        )?;
        let deferred = expansion.add_task(
            Task::CommitPatchDeferred {
                patch: patch.clone(),
            },
            [deletes],
        )?;
        expansion.add_task(Task::CleanPatchApply { patch, archive }, [deferred])?;
        Ok(expansion)
    })();

    match result {
        Ok(expansion) => TaskRun::expand(expansion),
        Err(error) => TaskRun::failed(error.to_string()),
    }
}

pub(crate) fn run_prepare_patch_apply(
    patch: Arc<PatchApplyWork>,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    let total = patch.entry_count();
    if total > 0 {
        let _ = event_tx.send(WorkerEvent::progress(
            crate::ProgressPhase::Patch,
            String::new(),
            0,
            total as u64,
            false,
        ));
    }
    // Source-directed extraction already committed ordinary top-level files
    // and reported their commit progress. Patch preparation only relocates
    // deferred staged inputs into the private recovery area; do not reset the
    // public commit lane with a second 0..N sequence here.
    match prepare_patch_apply(patch.plan(), None) {
        Ok(()) => TaskRun::succeeded(),
        Err(error) => TaskRun::failed(error.to_string()),
    }
}

pub(crate) fn run_apply_patch_entry(
    patch: Arc<PatchApplyWork>,
    entry_index: usize,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    let Some(entry) = patch.entry(entry_index) else {
        return TaskRun::failed(format!("patch entry index {entry_index} is out of range"));
    };
    match apply_patch_entry(patch.plan(), entry_index, patch.verification_cache()) {
        Ok(proof) => {
            let (finished, report_progress) = patch.finish_entry();
            let logical = patch.plan().vfs_base_path.join(&entry.name);
            let path = logical.to_string_lossy().replace('\\', "/");
            if proof.source() != ArtifactSource::Existing {
                let _ = event_tx.send(WorkerEvent::changed(path.clone()));
            }
            let _ = event_tx.send(WorkerEvent::committed(proof));
            if report_progress {
                let _ = event_tx.send(WorkerEvent::progress(
                    crate::ProgressPhase::Patch,
                    path,
                    finished as u64,
                    patch.entry_count() as u64,
                    false,
                ));
            }
            TaskRun::succeeded()
        }
        Err(error) => TaskRun::failed(error.to_string()),
    }
}

pub(crate) fn run_release_patch_base(patch: Arc<PatchApplyWork>, base: PathBuf) -> TaskRun {
    match release_patch_base(patch.plan(), &base) {
        Ok(()) => TaskRun::succeeded(),
        Err(error) => TaskRun::failed(error.to_string()),
    }
}

pub(crate) fn run_apply_patch_deletes(
    patch: Arc<PatchApplyWork>,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    let mut on_delete = |path: &std::path::Path, finished: usize, total: usize| {
        let normalized = path.to_string_lossy().replace('\\', "/");
        if finished > 0 {
            let _ = event_tx.send(WorkerEvent::changed(normalized.clone()));
        }
        if crate::task_pool::types::should_report_item_progress(finished, total) {
            let _ = event_tx.send(WorkerEvent::progress(
                crate::ProgressPhase::Delete,
                normalized,
                finished as u64,
                total as u64,
                false,
            ));
        }
    };
    match apply_patch_deletes(patch.plan(), Some(&mut on_delete)) {
        Ok(()) => TaskRun::succeeded(),
        Err(error) => TaskRun::failed(error.to_string()),
    }
}

pub(crate) fn run_commit_patch_deferred(
    patch: Arc<PatchApplyWork>,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    match commit_deferred_patch_files(patch.plan()) {
        Ok(()) => {
            // Deferred files become visible only here, after every patch output
            // and delete has succeeded. Report their changes at the actual
            // commit point rather than during private staging preparation.
            for path in &patch.plan().deferred_paths {
                let _ = event_tx.send(WorkerEvent::changed(
                    path.to_string_lossy().replace('\\', "/"),
                ));
            }
            TaskRun::succeeded()
        }
        Err(error) => TaskRun::failed(error.to_string()),
    }
}

pub(crate) fn run_clean_patch_apply(
    patch: Arc<PatchApplyWork>,
    archive: Arc<ArchiveWork>,
) -> TaskRun {
    if let Err(error) = clean_patch_apply(patch.plan()) {
        return TaskRun::failed(error.to_string());
    }
    archive.prepared.lock().unwrap().take();
    let mut expansion = GraphExpansion::new();
    match expansion.add_root_with_tokens(
        Task::CleanupArchive {
            work: archive.clone(),
        },
        archive.all_tokens(),
    ) {
        Ok(_) => TaskRun::expand(expansion),
        Err(error) => TaskRun::failed(error.to_string()),
    }
}
