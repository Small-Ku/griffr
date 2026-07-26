use std::sync::Arc;

use crate::runtime::task_pool::fs_ops::commit_staged_paths;
use crate::runtime::task_pool::graph::{GraphExpansion, TaskRun};
use crate::runtime::task_pool::types::{ArchiveWork, Task};

/// Finishes a forward-only archive run after every ordinary entry has already
/// been verified and committed by its extraction shard. Patch archives bypass
/// this helper and expand their persisted entry DAG instead.
pub(crate) fn finish_archive(
    archive: Arc<ArchiveWork>,
    staging_dir: std::path::PathBuf,
    deferred_commit_paths: Vec<std::path::PathBuf>,
) -> TaskRun {
    // Ordinary payload files were committed by their extraction shards. Only
    // deferred control files, such as delete_files.txt, may remain here. Commit
    // them after every payload shard succeeds so a failed archive cannot leave
    // an actionable control marker in the install root.
    if let Err(error) = commit_staged_paths(&staging_dir, &archive.dest, &deferred_commit_paths) {
        return TaskRun::failed(error.to_string());
    }
    archive.prepared.lock().unwrap().take();
    let mut expansion = GraphExpansion::new();
    let apply = expansion.add_root(Task::ApplyExtractedVfsPatchManifest {
        install_root: archive.dest.clone(),
    });
    match expansion.add_task_with_tokens(
        Task::CleanupArchive {
            work: archive.clone(),
        },
        [apply],
        archive.all_tokens(),
    ) {
        Ok(_) => TaskRun::expand(expansion),
        Err(error) => TaskRun::failed(error.to_string()),
    }
}
