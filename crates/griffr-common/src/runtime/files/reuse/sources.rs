use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use rapidhash::RapidHashSet as HashSet;

use crate::api::types::GameFileEntry;
use crate::config::GameId;
use crate::error::{Error, Result};
use crate::runtime::{
    detect_local_install, is_griffr_private_path, is_launcher_metadata_path,
    normalize_logical_path, LocalInstall, GAME_FILES_NAME,
};

/// Inspect explicit reuse paths, reject incompatible games, and omit the
/// destination itself. Reused bytes are always checked against the target
/// manifest, so source version and channel do not decide file eligibility.
pub async fn inspect_reuse_installations(
    game_id: &GameId,
    destination: &Path,
    source_paths: &[PathBuf],
) -> Result<Vec<LocalInstall>> {
    let destination_key = install_path_key(destination);
    let mut seen = HashSet::default();
    let mut sources = Vec::new();

    for source_path in source_paths {
        let source = detect_local_install(source_path)
            .await
            .map_err(|error| Error::Message {
                context: "Configuration error: ",
                detail: format!(
                    "Failed to inspect reuse source {}: {error}",
                    source_path.display()
                ),
            })?;
        let source_game_id = source.require_known_game()?;
        if &source_game_id != game_id {
            return Err(Error::Message {
                context: "Configuration error: ",
                detail: format!(
                    "Reuse source {} is {}, expected {}",
                    source.install_path.display(),
                    source_game_id,
                    game_id
                ),
            });
        }
        let source_key = install_path_key(&source.install_path);
        if source_key != destination_key && seen.insert(source_key) {
            sources.push(source);
        }
    }

    Ok(sources)
}

fn install_path_key(path: &Path) -> String {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let normalized = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
}

/// Resolve compatible reuse inputs to install roots. Every candidate file is
/// verified against the target manifest before it can be linked or copied.
pub async fn resolve_file_reuse_roots(
    game_id: &GameId,
    destination: &Path,
    source_paths: &[PathBuf],
) -> Result<Vec<PathBuf>> {
    Ok(
        inspect_reuse_installations(game_id, destination, source_paths)
            .await?
            .into_iter()
            .map(|source| source.install_path)
            .collect(),
    )
}

/// Read the launcher-managed manifest for the currently installed release.
/// Missing metadata returns `Ok(None)` so callers can fall back to archives.
pub async fn read_local_game_files(install_path: &Path) -> Result<Option<Vec<GameFileEntry>>> {
    let path = install_path.join(GAME_FILES_NAME);
    let encrypted = match compio::fs::read(&path).await {
        Ok(data) => data,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(Error::IoAt {
                action: "read local game_files manifest from",
                path,
                source,
            })
        }
    };
    crate::api::client::parse_game_files(&encrypted).map(Some)
}

fn obsolete_game_files<'a>(
    current: &'a [GameFileEntry],
    target: &[GameFileEntry],
) -> Result<Vec<(&'a GameFileEntry, PathBuf)>> {
    let target_paths = super::ensure::validated_game_file_entries(target)?
        .into_iter()
        .map(|(_, relative)| super::ensure::normalized_relative_path(&relative))
        .collect::<HashSet<_>>();
    let mut seen = HashSet::default();
    let mut obsolete = Vec::new();

    for entry in current {
        let relative = crate::runtime::task_pool::fs_ops::path_safety::parse_safe_relative_path(
            "local game_files entry",
            &entry.path,
        )?;
        if is_launcher_metadata_path(&relative.to_string_lossy())
            || is_griffr_private_path(&relative)
        {
            continue;
        }
        let normalized = super::ensure::normalized_relative_path(&relative);
        if !seen.insert(normalized.clone()) {
            return Err(Error::Message {
                context: "File reuse planning error: ",
                detail: format!(
                    "Local game_files manifest contains duplicate path {}",
                    entry.path
                ),
            });
        }
        if target_paths.contains(&normalized) {
            continue;
        }
        obsolete.push((entry, relative));
    }
    obsolete.sort_by(|left, right| left.1.cmp(&right.1));
    Ok(obsolete)
}

fn blocking_obsolete_game_files<'a>(
    current: &'a [GameFileEntry],
    target: &[GameFileEntry],
) -> Result<(Vec<(&'a GameFileEntry, PathBuf)>, Vec<PathBuf>)> {
    let target_paths = super::ensure::validated_game_file_entries(target)?
        .into_iter()
        .map(|(_, relative)| (super::ensure::normalized_relative_path(&relative), relative))
        .collect::<Vec<_>>();
    let mut blocking = Vec::new();
    let mut target_dirs = Vec::new();
    let mut seen_dirs = HashSet::default();

    for (entry, relative) in obsolete_game_files(current, target)? {
        let normalized = super::ensure::normalized_relative_path(&relative);
        let mut blocks_target = false;
        for (target_normalized, target_relative) in &target_paths {
            if super::ensure::logical_path_is_ancestor(&normalized, target_normalized) {
                blocks_target = true;
            } else if super::ensure::logical_path_is_ancestor(target_normalized, &normalized) {
                blocks_target = true;
                if seen_dirs.insert(target_normalized.clone()) {
                    target_dirs.push(target_relative.clone());
                }
            }
        }
        if blocks_target {
            blocking.push((entry, relative));
        }
    }

    blocking.sort_by(|left, right| left.1.cmp(&right.1));
    target_dirs.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    Ok((blocking, target_dirs))
}

fn blocking_owned_directories(
    blocking: &[(&GameFileEntry, PathBuf)],
    target_dirs: &[PathBuf],
) -> Vec<PathBuf> {
    let targets = target_dirs
        .iter()
        .map(|path| super::ensure::normalized_relative_path(path))
        .collect::<Vec<_>>();
    let mut seen = HashSet::default();
    let mut directories = Vec::new();

    for (_, relative) in blocking {
        let mut parent = relative.parent();
        while let Some(directory) = parent {
            let normalized = super::ensure::normalized_relative_path(directory);
            let belongs_to_target = targets.iter().any(|target| {
                normalized != *target
                    && super::ensure::logical_path_is_ancestor(target, &normalized)
            });
            if belongs_to_target && seen.insert(normalized) {
                directories.push(directory.to_path_buf());
            }
            parent = directory.parent();
        }
    }

    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    directories
}

fn canonical_path_key(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
}

fn ensure_cleanup_path_is_contained(
    canonical_install_root: &Path,
    target_path: &Path,
) -> Result<()> {
    match std::fs::symlink_metadata(target_path) {
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(Error::IoAt {
                action: "query file metadata/stat for",
                path: target_path.to_path_buf(),
                source,
            })
        }
    }
    let parent = target_path.parent().ok_or_else(|| Error::Message {
        context: "File cleanup error: ",
        detail: format!("Cleanup path has no parent: {}", target_path.display()),
    })?;
    let canonical_parent = std::fs::canonicalize(parent).map_err(|source| Error::IoAt {
        action: "resolve cleanup parent directory for",
        path: target_path.to_path_buf(),
        source,
    })?;
    let root_key = canonical_path_key(canonical_install_root);
    let parent_key = canonical_path_key(&canonical_parent);
    let root_prefix = if root_key.ends_with('/') {
        root_key.clone()
    } else {
        format!("{root_key}/")
    };
    let contained = parent_key == root_key || parent_key.starts_with(&root_prefix);
    if !contained {
        return Err(Error::Message {
            context: "File cleanup error: ",
            detail: format!(
                "Refusing to clean {} because its parent resolves outside install root {}",
                target_path.display(),
                canonical_install_root.display()
            ),
        });
    }
    Ok(())
}

async fn remove_verified_obsolete_game_files(
    install_path: &Path,
    obsolete: Vec<(&GameFileEntry, PathBuf)>,
    task_pool_runner: &mut crate::runtime::task_pool::TaskPoolRunner,
    fail_on_modified: bool,
) -> Result<super::types::ObsoleteFileCleanupSummary> {
    if obsolete.is_empty() {
        return Ok(super::types::ObsoleteFileCleanupSummary::default());
    }

    let canonical_install_root =
        std::fs::canonicalize(install_path).map_err(|source| Error::IoAt {
            action: "resolve install root for cleanup",
            path: install_path.to_path_buf(),
            source,
        })?;
    for (_, relative) in &obsolete {
        ensure_cleanup_path_is_contained(&canonical_install_root, &install_path.join(relative))?;
    }

    let tasks = obsolete
        .iter()
        .map(
            |(entry, relative)| crate::runtime::task_pool::Task::Verify {
                path: install_path.join(relative),
                logical_path: entry.path.clone(),
                expected_md5: entry.md5.clone(),
                expected_size: Some(entry.size),
                on_fail: None,
            },
        )
        .collect::<Vec<_>>();
    let task_progress =
        crate::runtime::task_pool::TaskProgress::new(crate::runtime::ProgressSender::disabled())
            .with_verify(
                crate::runtime::ProgressLane::FILE_ENSURE_VERIFY,
                tasks.len(),
            );
    let result = task_pool_runner
        .run_batch(tasks, task_progress)
        .map_err(|error| Error::Message {
            context: "Task pool error: ",
            detail: format!("Failed to verify obsolete launcher-owned files: {error}"),
        })?;
    // A plain Verify task reports a mismatch as a failed graph node. That is
    // expected here: only verified old bytes may be deleted, while every other
    // outcome is retained and inspected below.
    let verified = result
        .outcomes
        .into_iter()
        .filter_map(|outcome| match outcome {
            crate::runtime::task_pool::TaskOutcome::Verified { path, ok: true, .. } => {
                Some(normalize_logical_path(&path))
            }
            _ => None,
        })
        .collect::<HashSet<_>>();

    let mut summary = super::types::ObsoleteFileCleanupSummary::default();
    let mut removable_paths = Vec::new();
    let mut modified_paths = Vec::new();
    for (entry, relative) in obsolete {
        let path = install_path.join(relative);
        if verified.contains(&normalize_logical_path(&entry.path)) {
            removable_paths.push(path);
            continue;
        }

        match compio::fs::metadata(&path).await {
            Ok(_) => {
                summary.retained_modified_files = summary.retained_modified_files.saturating_add(1);
                modified_paths.push(entry.path.clone());
                tracing::warn!(
                    "Keeping obsolete manifest path {} because local content no longer matches the previous manifest",
                    entry.path
                );
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(source) => {
                return Err(Error::IoAt {
                    action: "inspect obsolete launcher-owned file",
                    path,
                    source,
                })
            }
        }
    }
    if fail_on_modified && !modified_paths.is_empty() {
        return Err(Error::Message {
            context: "File topology conflict: ",
            detail: format!(
                "Cannot replace {} modified obsolete path(s): {}",
                modified_paths.len(),
                modified_paths.join(", ")
            ),
        });
    }

    for path in removable_paths {
        match compio::fs::remove_file(&path).await {
            Ok(()) => summary.removed_files = summary.removed_files.saturating_add(1),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(source) => {
                return Err(Error::IoAt {
                    action: "remove obsolete launcher-owned file",
                    path,
                    source,
                })
            }
        }
    }
    Ok(summary)
}

/// Remove old launcher-owned files that block target file paths because one
/// path is an ancestor of the other. Modified blockers and non-empty target
/// directories stop the update without deleting unowned contents.
pub async fn remove_blocking_obsolete_game_files(
    install_path: &Path,
    current: &[GameFileEntry],
    target: &[GameFileEntry],
    task_pool_runner: &mut crate::runtime::task_pool::TaskPoolRunner,
) -> Result<super::types::ObsoleteFileCleanupSummary> {
    let (blocking, target_dirs) = blocking_obsolete_game_files(current, target)?;
    let owned_directories = blocking_owned_directories(&blocking, &target_dirs);
    let summary =
        remove_verified_obsolete_game_files(install_path, blocking, task_pool_runner, true).await?;
    for relative in owned_directories {
        crate::runtime::remove_empty_dir(install_path.join(relative)).await?;
    }
    for relative in target_dirs {
        crate::runtime::remove_empty_dir(install_path.join(relative)).await?;
    }
    Ok(summary)
}

/// Remove files owned by the previous launcher manifest that are absent from
/// the target manifest and still match their previous hash and size. Modified
/// or user-created paths are retained.
pub async fn remove_obsolete_game_files(
    install_path: &Path,
    current: &[GameFileEntry],
    target: &[GameFileEntry],
    task_pool_runner: &mut crate::runtime::task_pool::TaskPoolRunner,
) -> Result<super::types::ObsoleteFileCleanupSummary> {
    remove_verified_obsolete_game_files(
        install_path,
        obsolete_game_files(current, target)?,
        task_pool_runner,
        false,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str) -> GameFileEntry {
        GameFileEntry {
            path: path.to_string(),
            md5: "00".to_string(),
            size: 1,
        }
    }

    #[test]
    fn obsolete_paths_use_previous_launcher_ownership_only() {
        let current = vec![
            entry("Data/keep.bin"),
            entry("Data/remove.bin"),
            entry("config.ini"),
        ];
        let target = vec![entry("data\\KEEP.bin")];

        let obsolete = obsolete_game_files(&current, &target).unwrap();
        let paths = obsolete
            .into_iter()
            .map(|(_, path)| path)
            .collect::<Vec<_>>();

        assert_eq!(paths, vec![PathBuf::from("Data/remove.bin")]);
    }

    #[test]
    fn blocking_paths_cover_file_directory_transitions_only() {
        let current = vec![
            entry("OldFile"),
            entry("OldDir/file.bin"),
            entry("Later/remove.bin"),
        ];
        let target = vec![entry("OldFile/child.bin"), entry("OldDir")];

        let (blocking, target_dirs) = blocking_obsolete_game_files(&current, &target).unwrap();
        let paths = blocking
            .into_iter()
            .map(|(_, path)| path)
            .collect::<Vec<_>>();

        assert_eq!(
            paths,
            vec![PathBuf::from("OldDir/file.bin"), PathBuf::from("OldFile")]
        );
        assert_eq!(target_dirs, vec![PathBuf::from("OldDir")]);
    }

    #[test]
    fn blocking_directory_cleanup_is_limited_to_owned_parent_chains() {
        let owned = entry("OldDir/nested/file.bin");
        let blocking = vec![(&owned, PathBuf::from("OldDir/nested/file.bin"))];
        let target_dirs = vec![PathBuf::from("OldDir")];

        assert_eq!(
            blocking_owned_directories(&blocking, &target_dirs),
            vec![PathBuf::from("OldDir/nested")]
        );
    }

    #[compio::test]
    async fn obsolete_cleanup_removes_unchanged_and_retains_modified_files() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("Data")).unwrap();
        std::fs::write(temp.path().join("Data/remove.bin"), b"old").unwrap();
        std::fs::write(temp.path().join("Data/keep.bin"), b"new").unwrap();
        let current = vec![
            GameFileEntry {
                path: "Data/remove.bin".to_string(),
                md5: "149603e6c03516362a8da23f624db945".to_string(),
                size: 3,
            },
            GameFileEntry {
                path: "Data/keep.bin".to_string(),
                md5: "149603e6c03516362a8da23f624db945".to_string(),
                size: 3,
            },
        ];
        let mut runner = crate::runtime::task_pool::TaskPoolRunner::new(
            crate::runtime::task_pool::TaskPoolConfig::for_file_ensure(),
        )
        .unwrap();

        let summary = remove_obsolete_game_files(temp.path(), &current, &[], &mut runner)
            .await
            .unwrap();

        assert_eq!(summary.removed_files, 1);
        assert_eq!(summary.retained_modified_files, 1);
        assert!(!temp.path().join("Data/remove.bin").exists());
        assert!(temp.path().join("Data/keep.bin").exists());
    }

    #[compio::test]
    async fn blocking_cleanup_prepares_file_directory_transitions() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("OldDir")).unwrap();
        std::fs::write(temp.path().join("OldDir/file.bin"), b"old").unwrap();
        std::fs::write(temp.path().join("OldFile"), b"old").unwrap();
        let current = vec![
            GameFileEntry {
                path: "OldDir/file.bin".to_string(),
                md5: "149603e6c03516362a8da23f624db945".to_string(),
                size: 3,
            },
            GameFileEntry {
                path: "OldFile".to_string(),
                md5: "149603e6c03516362a8da23f624db945".to_string(),
                size: 3,
            },
        ];
        let target = vec![entry("OldDir"), entry("OldFile/child.bin")];
        let mut runner = crate::runtime::task_pool::TaskPoolRunner::new(
            crate::runtime::task_pool::TaskPoolConfig::for_file_ensure(),
        )
        .unwrap();

        let summary =
            remove_blocking_obsolete_game_files(temp.path(), &current, &target, &mut runner)
                .await
                .unwrap();

        assert_eq!(summary.removed_files, 2);
        assert!(!temp.path().join("OldDir").exists());
        assert!(!temp.path().join("OldFile").exists());
    }

    #[compio::test]
    async fn blocking_cleanup_removes_empty_directory_chain_before_target_file() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("OldDir/nested")).unwrap();
        std::fs::write(temp.path().join("OldDir/nested/file.bin"), b"old").unwrap();
        let current = vec![GameFileEntry {
            path: "OldDir/nested/file.bin".to_string(),
            md5: "149603e6c03516362a8da23f624db945".to_string(),
            size: 3,
        }];
        let target = vec![entry("OldDir")];
        let mut runner = crate::runtime::task_pool::TaskPoolRunner::new(
            crate::runtime::task_pool::TaskPoolConfig::for_file_ensure(),
        )
        .unwrap();

        let summary =
            remove_blocking_obsolete_game_files(temp.path(), &current, &target, &mut runner)
                .await
                .unwrap();

        assert_eq!(summary.removed_files, 1);
        assert!(!temp.path().join("OldDir").exists());
    }

    #[compio::test]
    async fn blocking_cleanup_preserves_unowned_empty_directories() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("OldDir/nested")).unwrap();
        std::fs::create_dir_all(temp.path().join("OldDir/user-empty")).unwrap();
        std::fs::write(temp.path().join("OldDir/nested/file.bin"), b"old").unwrap();
        let current = vec![GameFileEntry {
            path: "OldDir/nested/file.bin".to_string(),
            md5: "149603e6c03516362a8da23f624db945".to_string(),
            size: 3,
        }];
        let target = vec![entry("OldDir")];
        let mut runner = crate::runtime::task_pool::TaskPoolRunner::new(
            crate::runtime::task_pool::TaskPoolConfig::for_file_ensure(),
        )
        .unwrap();

        let error =
            remove_blocking_obsolete_game_files(temp.path(), &current, &target, &mut runner)
                .await
                .unwrap_err();

        assert!(error.to_string().contains("non-empty directory"));
        assert!(temp.path().join("OldDir/user-empty").exists());
    }

    #[compio::test]
    async fn blocking_cleanup_rejects_modified_obsolete_files_before_deleting_peers() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("OldFile"), b"changed").unwrap();
        std::fs::write(temp.path().join("OtherFile"), b"old").unwrap();
        let current = vec![
            GameFileEntry {
                path: "OldFile".to_string(),
                md5: "149603e6c03516362a8da23f624db945".to_string(),
                size: 3,
            },
            GameFileEntry {
                path: "OtherFile".to_string(),
                md5: "149603e6c03516362a8da23f624db945".to_string(),
                size: 3,
            },
        ];
        let target = vec![entry("OldFile/child.bin"), entry("OtherFile/child.bin")];
        let mut runner = crate::runtime::task_pool::TaskPoolRunner::new(
            crate::runtime::task_pool::TaskPoolConfig::for_file_ensure(),
        )
        .unwrap();

        let error =
            remove_blocking_obsolete_game_files(temp.path(), &current, &target, &mut runner)
                .await
                .unwrap_err();

        assert!(error.to_string().contains("modified obsolete"));
        assert!(temp.path().join("OldFile").exists());
        assert!(temp.path().join("OtherFile").exists());
    }

    #[cfg(unix)]
    #[compio::test]
    async fn obsolete_cleanup_rejects_parent_symlink_escape() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("install");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("file.bin"), b"old").unwrap();
        symlink(&outside, install.join("Linked")).unwrap();
        let current = vec![GameFileEntry {
            path: "Linked/file.bin".to_string(),
            md5: "149603e6c03516362a8da23f624db945".to_string(),
            size: 3,
        }];
        let mut runner = crate::runtime::task_pool::TaskPoolRunner::new(
            crate::runtime::task_pool::TaskPoolConfig::for_file_ensure(),
        )
        .unwrap();

        let error = remove_obsolete_game_files(&install, &current, &[], &mut runner)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("outside install root"));
        assert!(outside.join("file.bin").exists());
    }

    #[test]
    fn obsolete_paths_reject_unsafe_or_ambiguous_local_manifests() {
        let error = obsolete_game_files(&[entry("../outside.bin")], &[]).unwrap_err();
        assert!(error.to_string().contains("Invalid path"));

        let error = obsolete_game_files(&[entry("Data/file.bin"), entry("data/./FILE.bin")], &[])
            .unwrap_err();
        assert!(error.to_string().contains("duplicate path"));

        let current = [entry(".griffr\\state.json")];
        let obsolete = obsolete_game_files(&current, &[]).unwrap();
        assert!(obsolete.is_empty());
    }
}
