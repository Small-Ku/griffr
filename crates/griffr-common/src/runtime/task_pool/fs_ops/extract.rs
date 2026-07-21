use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::error::{Error, Result};
use crate::runtime::preallocate_file;
use crate::runtime::task_pool::verify::file_md5;
use md5::{Digest, Md5};
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

#[derive(Debug, Clone)]
pub(crate) struct CommitFileBatch {
    pub jobs: Vec<CommitFileJob>,
    pub estimated_bytes: u64,
    pub cross_volume: bool,
}

const CROSS_VOLUME_COMMIT_BATCH_BYTES: u64 = 384 * 1024 * 1024;
const MAX_COMMIT_BATCH_FILES: usize = 1024;

pub(crate) fn collect_staged_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(&directory).map_err(|source| Error::IoAt {
            action: "read directory",
            path: directory.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| Error::IoAt {
                action: "read directory",
                path: directory.clone(),
                source,
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|source| Error::IoAt {
                action: "query file metadata for",
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

pub(crate) fn commit_file_job(job: &CommitFileJob) -> Result<()> {
    if let Some(parent) = job.destination.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::IoAt {
            action: "create directory",
            path: parent.to_path_buf(),
            source,
        })?;
    }
    if job.destination.is_dir() {
        std::fs::remove_dir_all(&job.destination).map_err(|source| Error::IoAt {
            action: "remove file or directory",
            path: job.destination.clone(),
            source,
        })?;
    }
    move_path_replace_cross_volume(&job.source, &job.destination).map_err(|error| Error::Message {
        context: "",
        detail: format!(
            "Failed to move extracted file {} -> {}: {error}",
            job.source.display(),
            job.destination.display()
        ),
    })
}

pub(crate) fn commit_file_jobs(
    jobs: &[CommitFileJob],
    mut progress_callback: Option<&mut dyn FnMut(&Path, usize, usize)>,
) -> Result<()> {
    let total = jobs.len();
    if total > 0 {
        if let Some(callback) = progress_callback.as_deref_mut() {
            callback(Path::new("."), 0, total);
        }
    }
    for (index, job) in jobs.iter().enumerate() {
        commit_file_job(job)?;
        if let Some(callback) = progress_callback.as_deref_mut() {
            callback(&job.logical_path, index + 1, total);
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn collect_commit_jobs(
    staging_root: &Path,
    dest_root: &Path,
) -> Result<Vec<CommitFileJob>> {
    collect_commit_jobs_excluding(staging_root, dest_root, &std::collections::BTreeSet::new())
}

pub(crate) fn collect_commit_jobs_excluding(
    staging_root: &Path,
    dest_root: &Path,
    excluded_paths: &std::collections::BTreeSet<String>,
) -> Result<Vec<CommitFileJob>> {
    collect_staged_files(staging_root)?
        .into_iter()
        .filter_map(|source| {
            let logical_path = match source.strip_prefix(staging_root) {
                Ok(path) => path.to_path_buf(),
                Err(error) => return Some(Err(error.into())),
            };
            let normalized = logical_path
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase();
            if excluded_paths.contains(&normalized) {
                return None;
            }
            Some(Ok(CommitFileJob {
                destination: dest_root.join(&logical_path),
                source,
                logical_path,
            }))
        })
        .collect()
}

pub(crate) fn build_commit_batches(jobs: Vec<CommitFileJob>) -> Result<Vec<CommitFileBatch>> {
    use std::collections::BTreeMap;

    let mut groups = BTreeMap::<(String, String, PathBuf), Vec<(CommitFileJob, u64)>>::new();
    for job in jobs {
        let bytes = std::fs::metadata(&job.source)
            .map_err(|source| Error::IoAt {
                action: "query file metadata for",
                path: job.source.clone(),
                source,
            })?
            .len();
        let source_volume = super::reuse::storage_volume_group_key(&job.source);
        let destination_volume = super::reuse::storage_volume_group_key(&job.destination);
        let parent = job
            .destination
            .parent()
            .unwrap_or(job.destination.as_path())
            .to_path_buf();
        groups
            .entry((source_volume, destination_volume, parent))
            .or_default()
            .push((job, bytes));
    }

    let mut batches = Vec::new();
    for ((source_volume, destination_volume, _), group) in groups {
        let cross_volume = source_volume != destination_volume;
        let mut current = Vec::new();
        let mut current_bytes = 0u64;
        for (job, bytes) in group {
            let exceeds_bytes = cross_volume
                && !current.is_empty()
                && current_bytes.saturating_add(bytes) > CROSS_VOLUME_COMMIT_BATCH_BYTES;
            let exceeds_files = current.len() >= MAX_COMMIT_BATCH_FILES;
            if exceeds_bytes || exceeds_files {
                batches.push(CommitFileBatch {
                    jobs: std::mem::take(&mut current),
                    estimated_bytes: current_bytes,
                    cross_volume,
                });
                current_bytes = 0;
            }
            current_bytes = current_bytes.saturating_add(bytes);
            current.push(job);
        }
        if !current.is_empty() {
            batches.push(CommitFileBatch {
                jobs: current,
                estimated_bytes: current_bytes,
                cross_volume,
            });
        }
    }
    Ok(batches)
}

#[cfg(test)]
pub(crate) fn commit_staged_extract(
    staging_root: &Path,
    dest_root: &Path,
    progress_callback: Option<&mut dyn FnMut(&Path, usize, usize)>,
) -> Result<()> {
    let jobs = collect_commit_jobs(staging_root, dest_root)?;
    commit_file_jobs(&jobs, progress_callback)?;
    std::fs::remove_dir_all(staging_root).map_err(|source| Error::IoAt {
        action: "remove file or directory",
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
            return Err(Error::IoBetween {
                action: "rename file",
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
            std::fs::remove_dir_all(dest).map_err(|e| Error::IoAt {
                action: "remove file or directory",
                path: dest.to_path_buf(),
                source: e,
            })?;
        }
        std::fs::rename(src, dest).map_err(|e| Error::IoBetween {
            action: "rename file",
            src: src.to_path_buf(),
            dest: dest.to_path_buf(),
            source: e,
        })?;
        Ok(())
    }
}

pub(crate) struct CopiedFileDigest {
    pub(crate) bytes: u64,
    pub(crate) md5: String,
}

/// Copies a file while calculating MD5 from the same buffers written to the
/// destination. Callers with an expected digest can avoid a second full read.
pub(crate) fn copy_file_with_md5(src: &Path, dest: &Path) -> Result<CopiedFileDigest> {
    let mut input = File::open(src).map_err(|source| Error::IoAt {
        action: "open file",
        path: src.to_path_buf(),
        source,
    })?;
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(dest)
        .map_err(|source| Error::IoAt {
            action: "write to file",
            path: dest.to_path_buf(),
            source,
        })?;
    let copy_result = (|| -> Result<CopiedFileDigest> {
        let expected_size = input
            .metadata()
            .map_err(|source| Error::IoAt {
                action: "query file metadata for",
                path: src.to_path_buf(),
                source,
            })?
            .len();
        preallocate_file(&output, dest, expected_size)?;
        let mut hasher = Md5::new();
        let mut copied = 0u64;
        let mut buffer = vec![0u8; 1024 * 1024];
        loop {
            let read = input.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            output
                .write_all(&buffer[..read])
                .map_err(|source| Error::IoAt {
                    action: "write to file",
                    path: dest.to_path_buf(),
                    source,
                })?;
            hasher.update(&buffer[..read]);
            copied = copied.saturating_add(read as u64);
        }
        output.sync_all().map_err(|source| Error::IoAt {
            action: "write to file",
            path: dest.to_path_buf(),
            source,
        })?;
        Ok(CopiedFileDigest {
            bytes: copied,
            md5: crate::to_hex(&hasher.finalize()),
        })
    })();
    if copy_result.is_err() {
        drop(output);
        let _ = std::fs::remove_file(dest);
    }
    copy_result
}

pub(crate) fn move_path_replace_cross_volume(src: &Path, dest: &Path) -> Result<()> {
    match move_path_replace(src, dest) {
        Ok(()) => return Ok(()),
        Err(Error::IoBetween {
            action,
            src: _,
            dest: _,
            source,
        }) if action == "rename file" && source.kind() == std::io::ErrorKind::CrossesDevices => {}
        Err(error) => return Err(error),
    }

    let source_metadata = std::fs::metadata(src).map_err(|source| Error::IoAt {
        action: "query file metadata for",
        path: src.to_path_buf(),
        source,
    })?;
    if !source_metadata.is_file() {
        return Err(Error::Message {
            context: "",
            detail: format!(
                "Cross-volume replacement only supports files: {}",
                src.display()
            ),
        });
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::IoAt {
            action: "create directory",
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let temp = super::reuse::make_temp_write_path(dest)?;
    let _ = std::fs::remove_file(&temp);
    let copied = match copy_file_with_md5(src, &temp) {
        Ok(copied) => copied,
        Err(error) => {
            let _ = std::fs::remove_file(&temp);
            return Err(error);
        }
    };
    // Generic staging commits do not carry an expected checksum, so retain one
    // destination read for durability while eliminating the former source
    // re-read. Expected-checksum callers use the inline digest directly.
    if copied.bytes != source_metadata.len() || copied.md5 != file_md5(&temp)? {
        let _ = std::fs::remove_file(&temp);
        return Err(Error::Message {
            context: "",
            detail: format!(
                "Cross-volume copy verification failed for {} -> {}",
                src.display(),
                dest.display()
            ),
        });
    }
    move_path_replace(&temp, dest)?;
    std::fs::remove_file(src).map_err(|source| Error::IoAt {
        action: "remove file or directory",
        path: src.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        build_commit_batches, collect_commit_jobs, collect_commit_jobs_excluding,
        commit_staged_extract,
    };
    use crate::runtime::DELETE_FILES_MANIFEST_NAME;

    #[test]
    fn commit_job_collection_omits_paths_owned_by_other_branches() {
        let temp = tempfile::tempdir().unwrap();
        let staging_root = temp.path().join("staging");
        let dest_root = temp.path().join("install");
        std::fs::create_dir_all(staging_root.join("Data/VFS")).unwrap();
        std::fs::write(staging_root.join("Data/VFS/a.bin"), b"vfs").unwrap();
        std::fs::write(staging_root.join("game.bin"), b"game").unwrap();
        let excluded = std::collections::BTreeSet::from(["data/vfs/a.bin".to_string()]);

        let jobs = collect_commit_jobs_excluding(&staging_root, &dest_root, &excluded).unwrap();

        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].logical_path, Path::new("game.bin"));
    }

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
        let mut on_progress = |path: &Path, finished: usize, total: usize| {
            progress.push((path.to_path_buf(), finished, total));
        };
        commit_staged_extract(&staging_root, &dest_root, Some(&mut on_progress)).unwrap();

        assert_eq!(progress.first().map(|item| (item.1, item.2)), Some((0, 2)));
        assert_eq!(progress.last().map(|item| (item.1, item.2)), Some((2, 2)));
        assert_eq!(
            std::fs::read_to_string(dest_root.join("payload.txt")).unwrap(),
            "updated payload"
        );
        assert!(dest_root.join(DELETE_FILES_MANIFEST_NAME).exists());
    }
    #[test]
    fn commit_batches_keep_parent_directory_locality() {
        let temp = tempfile::tempdir().unwrap();
        let staging_root = temp.path().join("staging");
        let dest_root = temp.path().join("install");
        let first = staging_root.join("a").join("one.bin");
        let second = staging_root.join("b").join("two.bin");
        std::fs::create_dir_all(first.parent().unwrap()).unwrap();
        std::fs::create_dir_all(second.parent().unwrap()).unwrap();
        std::fs::write(&first, b"one").unwrap();
        std::fs::write(&second, b"two").unwrap();

        let jobs = collect_commit_jobs(&staging_root, &dest_root).unwrap();
        let batches = build_commit_batches(jobs).unwrap();
        assert_eq!(batches.len(), 2);
        assert!(batches.iter().all(|batch| batch.jobs.len() == 1));
    }
}
