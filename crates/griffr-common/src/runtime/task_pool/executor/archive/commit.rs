use std::sync::Arc;

use crate::error::{Error, Result};
use crate::runtime::task_pool::fs_ops::{
    build_commit_batches, collect_commit_jobs_excluding, commit_file_job,
};
use crate::runtime::task_pool::graph::{GraphExpansion, TaskExecution};
use crate::runtime::task_pool::types::{ArchiveCommitWork, ArchiveWork, Task, WorkerEvent};
use crate::runtime::task_pool::verify::build_issue;

pub(crate) fn schedule_archive_commit(
    archive: Arc<ArchiveWork>,
    staging_dir: std::path::PathBuf,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
    let result: Result<GraphExpansion> = (|| {
        let jobs = collect_commit_jobs_excluding(
            &staging_dir,
            &archive.dest,
            &archive.excluded_commit_paths,
        )?;
        let batches = build_commit_batches(jobs)?;
        let commit = ArchiveCommitWork::new(archive, staging_dir, batches);
        if commit.total_files() > 0 {
            let _ = event_tx.send(WorkerEvent::ArchiveCommitProgress {
                path: ".".to_string(),
                completed: 0,
                total: commit.total_files(),
            });
        }
        let mut expansion = GraphExpansion::new();
        let mut verified = Vec::with_capacity(commit.batch_count());
        for batch_index in 0..commit.batch_count() {
            let write = expansion.add_root(Task::CommitArchiveBatch {
                commit: commit.clone(),
                batch_index,
            });
            verified.push(expansion.add_task(
                Task::VerifyCommittedBatch {
                    commit: commit.clone(),
                    batch_index,
                },
                [write],
            )?);
        }
        if verified.is_empty() {
            expansion.add_root(Task::FinishArchiveCommit { commit });
        } else {
            expansion.add_task(Task::FinishArchiveCommit { commit }, verified)?;
        }
        Ok(expansion)
    })();
    match result {
        Ok(expansion) => TaskExecution::expand(expansion),
        Err(error) => TaskExecution::failed(error.to_string()),
    }
}

pub(crate) fn execute_commit_archive_batch(
    commit: Arc<ArchiveCommitWork>,
    batch_index: usize,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
    let Some(batch) = commit.batch(batch_index) else {
        return TaskExecution::failed(format!(
            "archive commit batch index {batch_index} is out of range"
        ));
    };
    for job in &batch.jobs {
        if let Err(error) = commit_file_job(job) {
            return TaskExecution::failed(error.to_string());
        }
        let completed = commit.finish_file();
        let normalized = job.logical_path.to_string_lossy().replace('\\', "/");
        let _ = event_tx.send(WorkerEvent::Changed {
            path: normalized.clone(),
        });
        let _ = event_tx.send(WorkerEvent::ArchiveCommitProgress {
            path: normalized,
            completed,
            total: commit.total_files(),
        });
    }
    TaskExecution::succeeded()
}

pub(crate) fn execute_verify_committed_batch(
    commit: Arc<ArchiveCommitWork>,
    batch_index: usize,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
    let Some(batch) = commit.batch(batch_index) else {
        return TaskExecution::failed(format!(
            "archive verify batch index {batch_index} is out of range"
        ));
    };
    for job in &batch.jobs {
        let logical = job.logical_path.to_string_lossy().replace('\\', "/");
        let Some(expected) = commit
            .archive
            .expected_files
            .get(&logical.to_ascii_lowercase())
        else {
            continue;
        };
        if let Some(issue) = build_issue(
            &job.destination,
            &logical,
            &expected.md5,
            Some(expected.size),
        ) {
            let _ = event_tx.send(WorkerEvent::Verified {
                path: logical.clone(),
                ok: false,
                issue: Some(issue.clone()),
            });
            return TaskExecution::failed(format!(
                "committed archive file {} failed verification: {:?}",
                logical, issue.kind
            ));
        }
        let _ = event_tx.send(WorkerEvent::Verified {
            path: logical,
            ok: true,
            issue: None,
        });
    }
    TaskExecution::succeeded()
}

pub(crate) fn execute_finish_archive_commit(commit: Arc<ArchiveCommitWork>) -> TaskExecution {
    if commit.staging_dir.exists() {
        if let Err(source) = std::fs::remove_dir_all(&commit.staging_dir) {
            return TaskExecution::failed(
                Error::RemoveFailed {
                    path: commit.staging_dir.clone(),
                    source,
                }
                .to_string(),
            );
        }
    }
    commit.archive.prepared.lock().unwrap().take();
    let mut expansion = GraphExpansion::new();
    let apply = expansion.add_root(Task::ApplyExtractedVfsPatchManifest {
        install_root: commit.archive.dest.clone(),
    });
    match expansion.add_task_with_tokens(
        Task::CleanupArchive {
            work: commit.archive.clone(),
        },
        [apply],
        commit.archive.all_tokens(),
    ) {
        Ok(_) => TaskExecution::expand(expansion),
        Err(error) => TaskExecution::failed(error.to_string()),
    }
}
