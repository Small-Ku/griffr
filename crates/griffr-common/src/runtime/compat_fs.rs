use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CopyStats {
    pub files: usize,
    pub bytes: u64,
}

async fn run_blocking<T: Send + 'static>(
    label: &'static str,
    task: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    compio::runtime::spawn_blocking(task)
        .await
        .map_err(|_| anyhow::anyhow!("{label} task panicked"))?
}

pub async fn directory_has_entries(path: impl Into<PathBuf>) -> Result<bool> {
    let path = path.into();
    run_blocking("directory scan", move || {
        let mut entries = std::fs::read_dir(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        Ok(entries.next().is_some())
    })
    .await
}

pub async fn list_files_with_extension(
    path: impl Into<PathBuf>,
    extension: impl Into<String>,
) -> Result<Vec<PathBuf>> {
    let path = path.into();
    let extension = extension.into();
    run_blocking("directory listing", move || {
        let mut targets = Vec::new();
        for entry in std::fs::read_dir(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?
        {
            let entry = entry.with_context(|| format!("Failed to read {}", path.display()))?;
            let entry_path = entry.path();
            if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false)
                && entry_path
                    .extension()
                    .is_some_and(|ext| ext == OsStr::new(&extension))
            {
                targets.push(entry_path);
            }
        }
        targets.sort();
        Ok(targets)
    })
    .await
}

pub async fn remove_dir_all(path: impl Into<PathBuf>) -> Result<()> {
    let path = path.into();
    run_blocking("recursive delete", move || {
        std::fs::remove_dir_all(&path)?;
        Ok(())
    })
    .await
}

pub async fn read_link(path: impl Into<PathBuf>) -> Result<PathBuf> {
    let path = path.into();
    run_blocking("read link", move || {
        std::fs::read_link(&path).with_context(|| format!("Failed to read link {}", path.display()))
    })
    .await
}

pub async fn dir_size(path: impl Into<PathBuf>) -> Result<u64> {
    let path = path.into();
    run_blocking("directory size", move || dir_size_sync(&path)).await
}

pub async fn copy_dir_recursive(
    source: impl Into<PathBuf>,
    target: impl Into<PathBuf>,
) -> Result<CopyStats> {
    let source = source.into();
    let target = target.into();
    run_blocking("recursive copy", move || {
        copy_dir_recursive_sync(&source, &target)
    })
    .await
}

pub async fn collect_files_recursive(path: impl Into<PathBuf>) -> Result<Vec<PathBuf>> {
    let path = path.into();
    run_blocking("recursive walk", move || {
        collect_files_recursive_sync(&path)
    })
    .await
}

pub async fn remove_empty_dirs_recursive(path: impl Into<PathBuf>) -> Result<()> {
    let path = path.into();
    run_blocking("empty dir cleanup", move || {
        remove_empty_dirs_recursive_sync(&path)
    })
    .await
}

fn dir_size_sync(path: &Path) -> Result<u64> {
    let mut total_size = 0u64;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_file() {
            total_size += metadata.len();
        } else if metadata.is_dir() {
            total_size += dir_size_sync(&entry.path())?;
        }
    }
    Ok(total_size)
}

fn copy_dir_recursive_sync(source: &Path, target: &Path) -> Result<CopyStats> {
    if !source.is_dir() {
        anyhow::bail!("Source directory not found: {}", source.display());
    }
    std::fs::create_dir_all(target)
        .with_context(|| format!("Failed to create {}", target.display()))?;
    let mut stats = CopyStats::default();
    copy_dir_recursive_inner_sync(source, target, &mut stats)?;
    Ok(stats)
}

fn copy_dir_recursive_inner_sync(
    source: &Path,
    target: &Path,
    stats: &mut CopyStats,
) -> Result<()> {
    let entries = std::fs::read_dir(source)
        .with_context(|| format!("Failed to read {}", source.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("Failed to enumerate {}", source.display()))?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());

        if source_path.is_dir() {
            std::fs::create_dir_all(&target_path)
                .with_context(|| format!("Failed to create {}", target_path.display()))?;
            copy_dir_recursive_inner_sync(&source_path, &target_path, stats)?;
        } else if source_path.is_file() {
            std::fs::copy(&source_path, &target_path).with_context(|| {
                format!(
                    "Failed to copy {} -> {}",
                    source_path.display(),
                    target_path.display()
                )
            })?;
            let len = std::fs::metadata(&source_path)
                .with_context(|| format!("Failed to stat {}", source_path.display()))?
                .len();
            stats.files += 1;
            stats.bytes += len;
        }
    }
    Ok(())
}

fn collect_files_recursive_sync(root: &Path) -> Result<Vec<PathBuf>> {
    let mut stack = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("Failed to read {}", dir.display()))?
        {
            let entry = entry.with_context(|| format!("Failed to read {}", dir.display()))?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                files.push(path);
            }
        }
    }
    Ok(files)
}

fn remove_empty_dirs_recursive_sync(root: &Path) -> Result<()> {
    if !root.is_dir() {
        return Ok(());
    }
    let mut dirs = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        dirs.push(dir.clone());
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("Failed to read {}", dir.display()))?
        {
            let entry = entry.with_context(|| format!("Failed to read {}", dir.display()))?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for dir in dirs {
        if dir == root {
            continue;
        }
        if std::fs::read_dir(&dir)
            .with_context(|| format!("Failed to read {}", dir.display()))?
            .next()
            .is_none()
        {
            let _ = std::fs::remove_dir(&dir);
        }
    }
    Ok(())
}
