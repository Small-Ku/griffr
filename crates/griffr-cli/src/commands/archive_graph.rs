use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use griffr_common::api::types::GameFileEntry;
use griffr_common::runtime::task_pool::{NodeId, Task, TaskGraphBuilder};

fn normalize_relative_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_ascii_lowercase()
}

fn archive_task_path(
    task: &Task,
    install_path: &Path,
    expected_files: &BTreeMap<String, GameFileEntry>,
) -> Option<String> {
    let target = task.target_path()?;
    let relative = target.strip_prefix(install_path).ok()?;
    let normalized = normalize_relative_path(relative);
    expected_files
        .contains_key(&normalized)
        .then_some(normalized)
}

/// Returns archive destinations whose final writer is a separate file task.
/// Full-package archive commits omit these staged files so VFS work can run in
/// parallel without a second writer racing the same destination.
pub(crate) fn owned_archive_paths(
    tasks: &[Task],
    install_path: &Path,
    expected_files: &BTreeMap<String, GameFileEntry>,
) -> Arc<BTreeSet<String>> {
    Arc::new(
        tasks
            .iter()
            .filter_map(|task| archive_task_path(task, install_path, expected_files))
            .collect(),
    )
}

/// Adds file tasks to an archive command graph using explicit path ownership.
/// Full-package callers pass `false` after excluding owned paths from archive
/// commit. Patch callers pass `true`, because patch entry and delete semantics
/// must finish before a task may replace an overlapping output.
pub(crate) fn add_file_tasks(
    graph: &mut TaskGraphBuilder,
    tasks: Vec<Task>,
    archive_nodes: &[NodeId],
    install_path: &Path,
    expected_files: &BTreeMap<String, GameFileEntry>,
    wait_for_archive_overlap: bool,
) -> Result<(usize, usize)> {
    let mut independent = 0usize;
    let mut archive_dependent = 0usize;
    for task in tasks {
        let overlaps_archive = archive_task_path(&task, install_path, expected_files).is_some();
        if wait_for_archive_overlap && overlaps_archive && !archive_nodes.is_empty() {
            graph.add_task(task, archive_nodes.iter().copied())?;
            archive_dependent = archive_dependent.saturating_add(1);
        } else {
            graph.add_root(task);
            independent = independent.saturating_add(1);
        }
    }
    Ok((independent, archive_dependent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use griffr_common::runtime::task_pool::{ArchiveRetention, ArchiveSource, TransferClass};

    fn verify_task(path: &Path, logical_path: &str) -> Task {
        Task::Verify {
            path: path.to_path_buf(),
            logical_path: logical_path.to_string(),
            expected_md5: "00000000000000000000000000000000".to_string(),
            expected_size: Some(1),
            on_fail: None,
        }
    }

    fn expected() -> BTreeMap<String, GameFileEntry> {
        BTreeMap::from([(
            "data/vfs/a.bin".to_string(),
            GameFileEntry {
                path: "Data/VFS/a.bin".to_string(),
                md5: "00000000000000000000000000000000".to_string(),
                size: 1,
            },
        )])
    }

    fn archive_task(root: &Path, expected: &BTreeMap<String, GameFileEntry>) -> Task {
        Task::OpenArchive {
            base_name: "pack".to_string(),
            source: ArchiveSource::Remote(Vec::new()),
            dest: root.to_path_buf(),
            retention: ArchiveRetention::Ephemeral,
            password: None,
            patch_options: Default::default(),
            expected_files: Arc::new(expected.clone()),
            excluded_commit_paths: Arc::new(BTreeSet::new()),
        }
    }

    #[test]
    fn full_package_assigns_overlapping_path_to_file_task() {
        let root = Path::new("game");
        let expected = expected();
        let file_task = verify_task(&root.join("Data/VFS/a.bin"), "a.bin");

        let owned = owned_archive_paths(&[file_task], root, &expected);

        assert_eq!(
            owned.as_ref(),
            &BTreeSet::from(["data/vfs/a.bin".to_string()])
        );
    }

    #[test]
    fn patch_overlap_waits_for_archive() {
        let root = Path::new("game");
        let expected = expected();
        let mut graph = TaskGraphBuilder::new();
        let archive = graph.add_root(archive_task(root, &expected));
        let counts = add_file_tasks(
            &mut graph,
            vec![verify_task(&root.join("Data/VFS/a.bin"), "a.bin")],
            &[archive],
            root,
            &expected,
            true,
        )
        .unwrap();

        assert_eq!(counts, (0, 1));
        assert_eq!(graph.build_checked().unwrap().summary().total_nodes, 2);
    }

    #[test]
    fn non_archive_file_task_is_independent() {
        let root = Path::new("game");
        let mut graph = TaskGraphBuilder::new();
        let archive = graph.add_root(Task::Download {
            url: "https://example.invalid/archive".to_string(),
            dest: root.join("archive"),
            logical_path: "archive".to_string(),
            expected_md5: String::new(),
            expected_size: None,
            retry_count: 0,
            transfer_class: TransferClass::General,
            resume: None,
        });
        let counts = add_file_tasks(
            &mut graph,
            vec![verify_task(&root.join("Data/VFS/b.bin"), "b.bin")],
            &[archive],
            root,
            &BTreeMap::new(),
            true,
        )
        .unwrap();
        assert_eq!(counts, (1, 0));
    }
}
