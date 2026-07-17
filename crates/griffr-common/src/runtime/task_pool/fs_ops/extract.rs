use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::error::{Error, Result};
use crate::runtime::task_pool::verify::file_md5;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
};

pub(crate) fn make_extract_staging_dir(
    dest: &Path,
    base_name: &str,
    work_dir: Option<&Path>,
) -> Result<PathBuf> {
    static EXTRACT_STAGING_COUNTER: AtomicUsize = AtomicUsize::new(0);
    let counter = EXTRACT_STAGING_COUNTER.fetch_add(1, Ordering::Relaxed);
    let parent = work_dir.unwrap_or_else(|| dest.parent().unwrap_or(dest));
    Ok(parent.join(format!(".griffr.extract.{}.{}", base_name, counter)))
}

#[derive(Debug, Clone)]
pub(crate) struct CommitFileJob {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub logical_path: PathBuf,
}

pub(crate) fn collect_staged_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(&directory).map_err(|source| Error::ReadDirFailed {
            path: directory.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| Error::ReadDirFailed {
                path: directory.clone(),
                source,
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|source| Error::StatFailed {
                path: path.clone(),
                source,
            })?;
            if file_type.is_dir() {
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn commit_file_job(job: &CommitFileJob) -> Result<()> {
    if let Some(parent) = job.destination.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::CreateDirFailed {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    if job.destination.is_dir() {
        std::fs::remove_dir_all(&job.destination).map_err(|source| Error::RemoveFailed {
            path: job.destination.clone(),
            source,
        })?;
    }
    move_path_replace_cross_volume(&job.source, &job.destination).map_err(|error| {
        Error::Other(format!(
            "Failed to move extracted file {} -> {}: {error}",
            job.source.display(),
            job.destination.display()
        ))
    })
}

pub(crate) fn commit_file_jobs(
    jobs: Vec<CommitFileJob>,
    commit_slots: usize,
    mut progress_callback: Option<&mut dyn FnMut(&Path, usize, usize)>,
) -> Result<()> {
    let total = jobs.len();
    if total > 0 {
        if let Some(callback) = progress_callback.as_deref_mut() {
            callback(Path::new("."), 0, total);
        }
    }
    let mut completed = 0usize;
    let commit_slots = commit_slots.max(1);
    for chunk in jobs.chunks(commit_slots) {
        let results = std::thread::scope(|scope| {
            chunk
                .iter()
                .map(|job| {
                    scope.spawn(move || commit_file_job(job).map_err(|error| error.to_string()))
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .unwrap_or_else(|_| Err("archive commit worker panicked".to_string()))
                })
                .collect::<Vec<_>>()
        });

        let mut failures = Vec::new();
        for (job, result) in chunk.iter().zip(results) {
            match result {
                Ok(()) => {
                    completed = completed.saturating_add(1);
                    if let Some(callback) = progress_callback.as_deref_mut() {
                        callback(&job.logical_path, completed, total);
                    }
                }
                Err(error) => failures.push(format!(
                    "{}: {}",
                    job.logical_path.display(),
                    error
                )),
            }
        }
        if !failures.is_empty() {
            return Err(Error::Other(format!(
                "Archive commit failed: {}",
                failures.join("; ")
            )));
        }
    }
    Ok(())
}

pub(crate) fn commit_staged_extract(
    staging_root: &Path,
    dest_root: &Path,
    commit_slots: usize,
    progress_callback: Option<&mut dyn FnMut(&Path, usize, usize)>,
) -> Result<()> {
    let jobs = collect_staged_files(staging_root)?
        .into_iter()
        .map(|source| {
            let logical_path = source.strip_prefix(staging_root)?.to_path_buf();
            Ok(CommitFileJob {
                destination: dest_root.join(&logical_path),
                source,
                logical_path,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    commit_file_jobs(jobs, commit_slots, progress_callback)?;
    std::fs::remove_dir_all(staging_root).map_err(|source| Error::RemoveFailed {
        path: staging_root.to_path_buf(),
        source,
    })
}

pub(crate) fn move_path_replace(src: &Path, dest: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        let mut src_wide: Vec<u16> = src.as_os_str().encode_wide().collect();
        src_wide.push(0);
        let mut dest_wide: Vec<u16> = dest.as_os_str().encode_wide().collect();
        dest_wide.push(0);
        let moved = unsafe {
            MoveFileExW(
                src_wide.as_ptr(),
                dest_wide.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if moved == 0 {
            return Err(Error::RenameFailed {
                src: src.to_path_buf(),
                dest: dest.to_path_buf(),
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        if dest.is_dir() {
            std::fs::remove_dir_all(dest).map_err(|e| Error::RemoveFailed {
                path: dest.to_path_buf(),
                source: e,
            })?;
        }
        std::fs::rename(src, dest).map_err(|e| Error::RenameFailed {
            src: src.to_path_buf(),
            dest: dest.to_path_buf(),
            source: e,
        })?;
        Ok(())
    }
}

pub(crate) fn move_path_replace_cross_volume(src: &Path, dest: &Path) -> Result<()> {
    match move_path_replace(src, dest) {
        Ok(()) => return Ok(()),
        Err(Error::RenameFailed { .. }) => {}
        Err(error) => return Err(error),
    }

    let source_metadata = std::fs::metadata(src).map_err(|source| Error::StatFailed {
        path: src.to_path_buf(),
        source,
    })?;
    if !source_metadata.is_file() {
        return Err(Error::Other(format!(
            "Cross-volume replacement only supports files: {}",
            src.display()
        )));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::CreateDirFailed {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let temp = super::reuse::make_temp_write_path(dest)?;
    let _ = std::fs::remove_file(&temp);
    std::fs::copy(src, &temp).map_err(|source| Error::CopyFailed {
        src: src.to_path_buf(),
        dest: temp.clone(),
        source,
    })?;
    let copied_metadata = std::fs::metadata(&temp).map_err(|source| Error::StatFailed {
        path: temp.clone(),
        source,
    })?;
    if copied_metadata.len() != source_metadata.len() || file_md5(src)? != file_md5(&temp)? {
        let _ = std::fs::remove_file(&temp);
        return Err(Error::Other(format!(
            "Cross-volume copy verification failed for {} -> {}",
            src.display(),
            dest.display()
        )));
    }
    move_path_replace(&temp, dest)?;
    std::fs::remove_file(src).map_err(|source| Error::RemoveFailed {
        path: src.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::commit_staged_extract;
    use crate::runtime::DELETE_FILES_MANIFEST_NAME;

    #[test]
    fn commit_staged_extract_keeps_delete_manifest_for_follow_up_task() {
        let temp = tempfile::tempdir().unwrap();
        let dest_root = temp.path().join("install");
        let staging_root = temp.path().join("staging");
        std::fs::create_dir_all(&staging_root).unwrap();
        std::fs::write(staging_root.join("payload.txt"), b"updated payload").unwrap();
        std::fs::write(
            staging_root.join(DELETE_FILES_MANIFEST_NAME),
            "Endfield_Data/Plugins/x86_64/libHAPI.dll\n",
        )
        .unwrap();

        let mut progress = Vec::new();
        let mut on_progress = |path: &Path, completed: usize, total: usize| {
            progress.push((path.to_path_buf(), completed, total));
        };
        commit_staged_extract(&staging_root, &dest_root, 2, Some(&mut on_progress)).unwrap();

        assert_eq!(progress.first().map(|item| (item.1, item.2)), Some((0, 2)));
        assert_eq!(progress.last().map(|item| (item.1, item.2)), Some((2, 2)));
        assert_eq!(
            std::fs::read_to_string(dest_root.join("payload.txt")).unwrap(),
            "updated payload"
        );
        assert!(dest_root.join(DELETE_FILES_MANIFEST_NAME).exists());
    }
}
