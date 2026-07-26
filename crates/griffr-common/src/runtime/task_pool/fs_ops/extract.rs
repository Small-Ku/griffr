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

pub(crate) fn create_extract_staging_dir(
    dest: &Path,
    base_name: &str,
    work_dir: Option<&Path>,
) -> Result<PathBuf> {
    static EXTRACT_STAGING_COUNTER: AtomicUsize = AtomicUsize::new(0);
    let parent = work_dir.unwrap_or_else(|| dest.parent().unwrap_or(dest));
    std::fs::create_dir_all(parent).map_err(|source| Error::IoAt {
        action: "create directory",
        path: parent.to_path_buf(),
        source,
    })?;
    for _ in 0..1024 {
        let counter = EXTRACT_STAGING_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".griffr.extract.{}.{}.{}",
            base_name,
            std::process::id(),
            counter
        ));
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(Error::IoAt {
                    action: "create directory",
                    path,
                    source,
                });
            }
        }
    }
    Err(Error::Message {
        context: "Task pool error: ",
        detail: format!(
            "Could not allocate a unique extraction directory under {}",
            parent.display()
        ),
    })
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

pub(crate) fn commit_staged_paths(
    staging_root: &Path,
    dest_root: &Path,
    logical_paths: &[PathBuf],
) -> Result<()> {
    let jobs = logical_paths
        .iter()
        .map(|logical_path| CommitFileJob {
            source: staging_root.join(logical_path),
            destination: dest_root.join(logical_path),
            logical_path: logical_path.clone(),
        })
        .collect::<Vec<_>>();
    commit_file_jobs(&jobs, None)?;
    std::fs::remove_dir_all(staging_root).map_err(|source| Error::IoAt {
        action: "remove file or directory",
        path: staging_root.to_path_buf(),
        source,
    })
}

pub(crate) fn move_path_replace(src: &Path, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::IoAt {
            action: "create directory",
            path: parent.to_path_buf(),
            source,
        })?;
    }
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

    use super::commit_staged_paths;
    use crate::runtime::DELETE_FILES_MANIFEST_NAME;

    #[test]
    fn deferred_archive_files_commit_after_payload_shards_finish() {
        let temp = tempfile::tempdir().unwrap();
        let dest_root = temp.path().join("install");
        let staging_root = temp.path().join("staging");
        std::fs::create_dir_all(&staging_root).unwrap();
        std::fs::write(
            staging_root.join(DELETE_FILES_MANIFEST_NAME),
            "Endfield_Data/Plugins/x86_64/libHAPI.dll\n",
        )
        .unwrap();
        std::fs::write(staging_root.join("unexpected.bin"), b"not a control file").unwrap();

        commit_staged_paths(
            &staging_root,
            &dest_root,
            &[Path::new(DELETE_FILES_MANIFEST_NAME).to_path_buf()],
        )
        .unwrap();

        assert!(dest_root.join(DELETE_FILES_MANIFEST_NAME).exists());
        assert!(!dest_root.join("unexpected.bin").exists());
        assert!(!staging_root.exists());
    }
}
