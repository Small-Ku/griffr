use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use compio::buf::BufResult;
use compio::io::{AsyncReadAt, AsyncWriteAtExt};

use crate::error::{Error, Result};

use super::preallocate_file;
use super::task_pool::fs_ops::make_temp_write_path;

const COPY_BUFFER_BYTES: usize = 1024 * 1024;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CopyStats {
    pub files: usize,
    pub bytes: u64,
}

#[derive(Debug)]
struct CopyJob {
    source: PathBuf,
    target: PathBuf,
    bytes: u64,
}

#[derive(Debug, Default)]
struct CopyPlan {
    directories: Vec<PathBuf>,
    files: Vec<CopyJob>,
}

pub(crate) async fn run_blocking<T: Send + 'static>(
    label: &'static str,
    task: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    compio::runtime::spawn_blocking(task)
        .await
        .map_err(|_| Error::TaskPool(format!("{label} task panicked")))?
}

/// Returns `true` if `path` is a directory, `false` for any other outcome
/// (not found, not a directory, permission error). Use for path probing where
/// an inaccessible path should be treated the same as absent.
pub async fn path_is_dir(path: &Path) -> bool {
    compio::fs::metadata(path).await.is_ok_and(|m| m.is_dir())
}

/// Returns `true` if `path` is a regular file, `false` for any other outcome.
/// Use for path probing where an inaccessible path should be treated as absent.
pub async fn path_is_file(path: &Path) -> bool {
    compio::fs::metadata(path).await.is_ok_and(|m| m.is_file())
}

/// Returns `Ok(true)` if `path` is a directory, `Ok(false)` if it does not
/// exist, and `Err` if the filesystem returns an unexpected error.
pub async fn path_is_dir_or_err(path: &Path) -> Result<bool> {
    match compio::fs::metadata(path).await {
        Ok(m) => Ok(m.is_dir()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(Error::StatFailed {
            path: path.to_path_buf(),
            source: e,
        }),
    }
}

pub async fn directory_has_entries(path: impl Into<PathBuf>) -> Result<bool> {
    let path = path.into();
    run_blocking("directory scan", move || {
        let mut entries = std::fs::read_dir(&path).map_err(|e| Error::ReadDirFailed {
            path: path.clone(),
            source: e,
        })?;
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
        for entry in std::fs::read_dir(&path).map_err(|e| Error::ReadDirFailed {
            path: path.clone(),
            source: e,
        })? {
            let entry = entry.map_err(|e| Error::ReadDirFailed {
                path: path.clone(),
                source: e,
            })?;
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
        std::fs::read_link(&path)
            .map_err(|e| Error::Other(format!("Failed to read link {}: {}", path.display(), e)))
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

    if !path_is_dir_or_err(&source).await? {
        return Err(Error::InvalidPath(format!(
            "Source directory not found: {}",
            source.display()
        )));
    }

    compio::fs::create_dir_all(&target)
        .await
        .map_err(|source_error| Error::CreateDirFailed {
            path: target.clone(),
            source: source_error,
        })?;

    // compio 0.19 has no async directory iterator. Keep only the namespace walk
    // on the blocking pool, then perform the potentially large file transfers
    // through positional compio I/O.
    let plan_source = source.clone();
    let plan_target = target.clone();
    let plan = run_blocking("recursive copy inventory", move || {
        collect_copy_plan_sync(&plan_source, &plan_target)
    })
    .await?;

    for directory in &plan.directories {
        compio::fs::create_dir_all(directory)
            .await
            .map_err(|source_error| Error::CreateDirFailed {
                path: directory.clone(),
                source: source_error,
            })?;
    }

    let mut stats = CopyStats::default();
    for job in plan.files {
        copy_file_async(&job.source, &job.target, job.bytes).await?;
        stats.files += 1;
        stats.bytes = stats.bytes.saturating_add(job.bytes);
    }
    Ok(stats)
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

async fn copy_file_async(source: &Path, target: &Path, expected_bytes: u64) -> Result<()> {
    if let Some(parent) = target.parent() {
        compio::fs::create_dir_all(parent)
            .await
            .map_err(|source_error| Error::CreateDirFailed {
                path: parent.to_path_buf(),
                source: source_error,
            })?;
    }

    let temp = make_temp_write_path(target)?;
    match compio::fs::remove_file(&temp).await {
        Ok(()) => {}
        Err(source_error) if source_error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source_error) => {
            return Err(Error::RemoveFailed {
                path: temp,
                source: source_error,
            })
        }
    }

    let source_metadata =
        compio::fs::metadata(source)
            .await
            .map_err(|source_error| Error::StatFailed {
                path: source.to_path_buf(),
                source: source_error,
            })?;
    let input = compio::fs::File::open(source)
        .await
        .map_err(|source_error| Error::OpenFileFailed {
            path: source.to_path_buf(),
            source: source_error,
        })?;
    let mut output = compio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .await
        .map_err(|source_error| Error::CopyFailed {
            src: source.to_path_buf(),
            dest: target.to_path_buf(),
            source: source_error,
        })?;

    let copy_result = async {
        preallocate_file(&output, &temp, expected_bytes)?;
        let mut copied = 0u64;
        let mut buffer = vec![0u8; COPY_BUFFER_BYTES];
        loop {
            let BufResult(read_result, mut returned_buffer) = input.read_at(buffer, copied).await;
            let read = read_result.map_err(|source_error| Error::CopyFailed {
                src: source.to_path_buf(),
                dest: target.to_path_buf(),
                source: source_error,
            })?;
            if read == 0 {
                break;
            }
            returned_buffer.truncate(read);
            let BufResult(write_result, mut returned_buffer) =
                output.write_all_at(returned_buffer, copied).await;
            write_result.map_err(|source_error| Error::CopyFailed {
                src: source.to_path_buf(),
                dest: target.to_path_buf(),
                source: source_error,
            })?;
            copied = copied.saturating_add(read as u64);
            returned_buffer.resize(COPY_BUFFER_BYTES, 0);
            buffer = returned_buffer;
        }

        if copied != expected_bytes {
            return Err(Error::Integrity(format!(
                "Copy size mismatch for {} -> {}: expected {} bytes, copied {}",
                source.display(),
                target.display(),
                expected_bytes,
                copied
            )));
        }
        output
            .sync_all()
            .await
            .map_err(|source_error| Error::WriteFileFailed {
                path: temp.clone(),
                source: source_error,
            })?;
        compio::fs::set_permissions(&temp, source_metadata.permissions())
            .await
            .map_err(|source_error| Error::CopyFailed {
                src: source.to_path_buf(),
                dest: target.to_path_buf(),
                source: source_error,
            })?;
        Ok(())
    }
    .await;

    let close_result = output
        .close()
        .await
        .map_err(|source_error| Error::WriteFileFailed {
            path: temp.clone(),
            source: source_error,
        });
    if let Err(error) = copy_result {
        let _ = close_result;
        let _ = compio::fs::remove_file(&temp).await;
        return Err(error);
    }
    if let Err(error) = close_result {
        let _ = compio::fs::remove_file(&temp).await;
        return Err(error);
    }
    if let Err(source_error) = compio::fs::rename(&temp, target).await {
        let _ = compio::fs::remove_file(&temp).await;
        return Err(Error::RenameFailed {
            src: temp,
            dest: target.to_path_buf(),
            source: source_error,
        });
    }
    Ok(())
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

fn collect_copy_plan_sync(source: &Path, target: &Path) -> Result<CopyPlan> {
    let mut plan = CopyPlan::default();
    collect_copy_plan_inner_sync(source, target, &mut plan)?;
    Ok(plan)
}

fn collect_copy_plan_inner_sync(source: &Path, target: &Path, plan: &mut CopyPlan) -> Result<()> {
    let entries = std::fs::read_dir(source).map_err(|source_error| Error::ReadDirFailed {
        path: source.to_path_buf(),
        source: source_error,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source_error| Error::ReadDirFailed {
            path: source.to_path_buf(),
            source: source_error,
        })?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());

        let file_type = entry
            .file_type()
            .map_err(|source_error| Error::StatFailed {
                path: source_path.clone(),
                source: source_error,
            })?;

        if file_type.is_dir() {
            plan.directories.push(target_path.clone());
            collect_copy_plan_inner_sync(&source_path, &target_path, plan)?;
        } else if file_type.is_file() {
            let bytes = entry
                .metadata()
                .map_err(|source_error| Error::StatFailed {
                    path: source_path.clone(),
                    source: source_error,
                })?
                .len();
            plan.files.push(CopyJob {
                source: source_path,
                target: target_path,
                bytes,
            });
        }
    }
    Ok(())
}

fn collect_files_recursive_sync(root: &Path) -> Result<Vec<PathBuf>> {
    let mut stack = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|e| Error::ReadDirFailed {
            path: dir.clone(),
            source: e,
        })? {
            let entry = entry.map_err(|e| Error::ReadDirFailed {
                path: dir.clone(),
                source: e,
            })?;
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
        for entry in std::fs::read_dir(&dir).map_err(|e| Error::ReadDirFailed {
            path: dir.clone(),
            source: e,
        })? {
            let entry = entry.map_err(|e| Error::ReadDirFailed {
                path: dir.clone(),
                source: e,
            })?;
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
            .map_err(|e| Error::ReadDirFailed {
                path: dir.clone(),
                source: e,
            })?
            .next()
            .is_none()
        {
            let _ = std::fs::remove_dir(&dir);
        }
    }
    Ok(())
}
