use std::sync::Arc;

use crate::error::{Error, Result};
use crate::runtime::task_pool::fs_ops::{
    build_commit_batches, collect_commit_jobs_excluding, commit_file_job,
};
use crate::runtime::task_pool::graph::{GraphExpansion, TaskRun};
use crate::runtime::task_pool::types::{ArchiveCommitWork, ArchiveWork, Task, WorkerEvent};
use crate::runtime::task_pool::verify::build_issue;

pub(crate) fn schedule_archive_commit(
    archive: Arc<ArchiveWork>,
    staging_dir: std::path::PathBuf,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    let result: Result<GraphExpansion> = (|| {
        let jobs = collect_commit_jobs_excluding(
            &staging_dir,
            &archive.dest,
            &archive.excluded_commit_paths,
        )?;
        let batches = build_commit_batches(jobs)?;
        let commit = ArchiveCommitWork::new(archive, staging_dir, batches);
        if commit.total_files() > 0 {
            let _ = event_tx.send(WorkerEvent::progress(
                crate::runtime::ProgressPhase::Commit,
                ".".to_string(),
                0,
                commit.total_files() as u64,
                false,
            ));
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
        Ok(expansion) => TaskRun::expand(expansion),
        Err(error) => TaskRun::failed(error.to_string()),
    }
}

pub(crate) fn run_commit_archive_batch(
    commit: Arc<ArchiveCommitWork>,
    batch_index: usize,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    let Some(batch) = commit.batch(batch_index) else {
        return TaskRun::failed(format!(
            "archive commit batch index {batch_index} is out of range"
        ));
    };
    for job in &batch.jobs {
        if let Err(error) = commit_file_job(job) {
            return TaskRun::failed(error.to_string());
        }
        let finished = commit.finish_file();
        let normalized = job.logical_path.to_string_lossy().replace('\\', "/");
        let _ = event_tx.send(WorkerEvent::changed(normalized.clone()));
        let _ = event_tx.send(WorkerEvent::progress(
            crate::runtime::ProgressPhase::Commit,
            normalized,
            finished as u64,
            commit.total_files() as u64,
            false,
        ));
    }
    TaskRun::succeeded()
}

pub(crate) fn run_verify_committed_batch(
    commit: Arc<ArchiveCommitWork>,
    batch_index: usize,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    let Some(batch) = commit.batch(batch_index) else {
        return TaskRun::failed(format!(
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
            let _ = event_tx.send(WorkerEvent::verified(
                logical.clone(),
                false,
                Some(issue.clone()),
            ));
            return TaskRun::failed(format!(
                "committed archive file {} failed verification: {:?}",
                logical, issue.kind
            ));
        }
        let _ = event_tx.send(WorkerEvent::verified(logical, true, None));
    }
    TaskRun::succeeded()
}

pub(crate) fn run_finish_archive_commit(commit: Arc<ArchiveCommitWork>) -> TaskRun {
    if commit.staging_dir.exists() {
        if let Err(source) = std::fs::remove_dir_all(&commit.staging_dir) {
            return TaskRun::failed(
                Error::IoAt {
                    action: "remove file or directory",
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
        Ok(_) => TaskRun::expand(expansion),
        Err(error) => TaskRun::failed(error.to_string()),
    }
}
