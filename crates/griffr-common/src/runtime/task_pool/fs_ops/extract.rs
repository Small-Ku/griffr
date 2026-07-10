use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::error::{Error, Result};
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
};

pub(crate) fn make_extract_staging_dir(dest: &Path, base_name: &str) -> Result<PathBuf> {
    static EXTRACT_STAGING_COUNTER: AtomicUsize = AtomicUsize::new(0);
    let counter = EXTRACT_STAGING_COUNTER.fetch_add(1, Ordering::Relaxed);
    let parent = dest.parent().unwrap_or(dest);
    Ok(parent.join(format!(".griffr.extract.{}.{}", base_name, counter)))
}

pub(crate) fn commit_staged_extract(staging_root: &Path, dest_root: &Path) -> Result<()> {
    commit_staged_extract_inner(staging_root, staging_root, dest_root)?;
    std::fs::remove_dir_all(staging_root).map_err(|e| Error::RemoveFailed {
        path: staging_root.to_path_buf(),
        source: e,
    })?;
    Ok(())
}

fn commit_staged_extract_inner(
    staging_root: &Path,
    current: &Path,
    dest_root: &Path,
) -> Result<()> {
    for entry in std::fs::read_dir(current).map_err(|e| Error::ReadDirFailed {
        path: current.to_path_buf(),
        source: e,
    })? {
        let entry = entry.map_err(|e| Error::ReadDirFailed {
            path: current.to_path_buf(),
            source: e,
        })?;
        let src_path = entry.path();
        let file_type = entry.file_type().map_err(|e| Error::StatFailed {
            path: src_path.clone(),
            source: e,
        })?;
        let relative = src_path.strip_prefix(staging_root)?;
        let dest_path = dest_root.join(relative);
        if file_type.is_dir() {
            std::fs::create_dir_all(&dest_path).map_err(|e| Error::CreateDirFailed {
                path: dest_path.clone(),
                source: e,
            })?;
            commit_staged_extract_inner(staging_root, &src_path, dest_root)?;
            continue;
        }
        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::CreateDirFailed {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        if dest_path.exists() && dest_path.is_dir() {
            std::fs::remove_dir_all(&dest_path).map_err(|e| Error::RemoveFailed {
                path: dest_path.clone(),
                source: e,
            })?;
        }
        move_path_replace(&src_path, &dest_path).map_err(|e| {
            Error::Other(format!(
                "Failed to move extracted file {} -> {}: {e}",
                src_path.display(),
                dest_path.display()
            ))
        })?;
    }
    Ok(())
}

pub(super) fn move_path_replace(src: &Path, dest: &Path) -> Result<()> {
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
        if dest.exists() {
            if dest.is_dir() {
                std::fs::remove_dir_all(dest).map_err(|e| Error::RemoveFailed {
                    path: dest.to_path_buf(),
                    source: e,
                })?;
            } else {
                std::fs::remove_file(dest).map_err(|e| Error::RemoveFailed {
                    path: dest.to_path_buf(),
                    source: e,
                })?;
            }
        }
        std::fs::rename(src, dest).map_err(|e| Error::RenameFailed {
            src: src.to_path_buf(),
            dest: dest.to_path_buf(),
            source: e,
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::commit_staged_extract;
    use crate::runtime::task_pool::fs_ops::delete_manifest::DELETE_FILES_MANIFEST_NAME;

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

        commit_staged_extract(&staging_root, &dest_root).unwrap();

        assert_eq!(
            std::fs::read_to_string(dest_root.join("payload.txt")).unwrap(),
            "updated payload"
        );
        assert!(dest_root.join(DELETE_FILES_MANIFEST_NAME).exists());
    }
}
