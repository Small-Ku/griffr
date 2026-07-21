use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::error::Result;
use crate::runtime::task_pool::fs_ops::{
    apply_patch_transaction_deletes, apply_patch_transaction_entry, cleanup_patch_transaction,
    commit_patch_transaction_deferred, prepare_patch_transaction, release_patch_transaction_base,
};
use crate::runtime::task_pool::graph::{GraphExpansion, TaskExecution};
use crate::runtime::task_pool::types::{ArchiveWork, PatchTransactionWork, Task, WorkerEvent};
use crate::runtime::{entry_dependency_indices, entry_wave_indices, PlannedPatchSource};

pub(crate) fn schedule_patch_transaction(
    archive: Arc<ArchiveWork>,
    transaction: Arc<PatchTransactionWork>,
) -> TaskExecution {
    let result: Result<GraphExpansion> = (|| {
        let plan = transaction.plan();
        let dependencies = entry_dependency_indices(plan)?;
        let waves = entry_wave_indices(plan)?;
        let mut expansion = GraphExpansion::new();
        let prepare = expansion.add_root(Task::PreparePatchTransaction {
            transaction: transaction.clone(),
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
                        transaction: transaction.clone(),
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
                    transaction: transaction.clone(),
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
                transaction: transaction.clone(),
            },
            final_entry_nodes,
        )?;
        let deferred = expansion.add_task(
            Task::CommitPatchDeferred {
                transaction: transaction.clone(),
            },
            [deletes],
        )?;
        expansion.add_task(
            Task::CleanupPatchTransaction {
                transaction,
                archive,
            },
            [deferred],
        )?;
        Ok(expansion)
    })();

    match result {
        Ok(expansion) => TaskExecution::expand(expansion),
        Err(error) => TaskExecution::failed(error.to_string()),
    }
}

pub(crate) fn execute_prepare_patch_transaction(
    transaction: Arc<PatchTransactionWork>,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
    let total = transaction.entry_count();
    if total > 0 {
        let _ = event_tx.send(WorkerEvent::PatchProgress {
            path: String::new(),
            completed: 0,
            total,
        });
    }
    let mut on_commit = |path: &std::path::Path, completed: usize, total: usize| {
        let normalized = path.to_string_lossy().replace('\\', "/");
        if completed > 0 {
            let _ = event_tx.send(WorkerEvent::Changed {
                path: normalized.clone(),
            });
        }
        let _ = event_tx.send(WorkerEvent::ArchiveCommitProgress {
            path: normalized,
            completed,
            total,
        });
    };
    match prepare_patch_transaction(transaction.plan(), Some(&mut on_commit)) {
        Ok(()) => TaskExecution::succeeded(),
        Err(error) => TaskExecution::failed(error.to_string()),
    }
}

pub(crate) fn execute_apply_patch_entry(
    transaction: Arc<PatchTransactionWork>,
    entry_index: usize,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
    let Some(entry) = transaction.entry(entry_index) else {
        return TaskExecution::failed(format!("patch entry index {entry_index} is out of range"));
    };
    match apply_patch_transaction_entry(
        transaction.plan(),
        entry_index,
        transaction.verification_cache(),
    ) {
        Ok(()) => {
            let completed = transaction.finish_entry();
            let logical = transaction.plan().vfs_base_path.join(&entry.name);
            let path = logical.to_string_lossy().replace('\\', "/");
            let _ = event_tx.send(WorkerEvent::Changed { path: path.clone() });
            let _ = event_tx.send(WorkerEvent::PatchProgress {
                path,
                completed,
                total: transaction.entry_count(),
            });
            TaskExecution::succeeded()
        }
        Err(error) => TaskExecution::failed(error.to_string()),
    }
}

pub(crate) fn execute_release_patch_base(
    transaction: Arc<PatchTransactionWork>,
    base: PathBuf,
) -> TaskExecution {
    match release_patch_transaction_base(transaction.plan(), &base) {
        Ok(()) => TaskExecution::succeeded(),
        Err(error) => TaskExecution::failed(error.to_string()),
    }
}

pub(crate) fn execute_apply_patch_deletes(
    transaction: Arc<PatchTransactionWork>,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
    let mut on_delete = |path: &std::path::Path, completed: usize, total: usize| {
        let normalized = path.to_string_lossy().replace('\\', "/");
        if completed > 0 {
            let _ = event_tx.send(WorkerEvent::Changed {
                path: normalized.clone(),
            });
        }
        let _ = event_tx.send(WorkerEvent::DeleteProgress {
            path: normalized,
            completed,
            total,
        });
    };
    match apply_patch_transaction_deletes(transaction.plan(), Some(&mut on_delete)) {
        Ok(()) => TaskExecution::succeeded(),
        Err(error) => TaskExecution::failed(error.to_string()),
    }
}

pub(crate) fn execute_commit_patch_deferred(
    transaction: Arc<PatchTransactionWork>,
) -> TaskExecution {
    match commit_patch_transaction_deferred(transaction.plan()) {
        Ok(()) => TaskExecution::succeeded(),
        Err(error) => TaskExecution::failed(error.to_string()),
    }
}

pub(crate) fn execute_cleanup_patch_transaction(
    transaction: Arc<PatchTransactionWork>,
    archive: Arc<ArchiveWork>,
) -> TaskExecution {
    if let Err(error) = cleanup_patch_transaction(transaction.plan()) {
        return TaskExecution::failed(error.to_string());
    }
    archive.prepared.lock().unwrap().take();
    let mut expansion = GraphExpansion::new();
    match expansion.add_root_with_tokens(
        Task::CleanupArchive {
            work: archive.clone(),
        },
        archive.all_tokens(),
    ) {
        Ok(_) => TaskExecution::expand(expansion),
        Err(error) => TaskExecution::failed(error.to_string()),
    }
}
