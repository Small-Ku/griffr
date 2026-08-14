use std::path::Path;

#[cfg(windows)]
use crate::error::Error;
use crate::error::Result;

/// Reserves the final allocation without changing the file's logical EOF.
///
/// Windows uses FILE_ALLOCATION_INFO so large sequential writes do not grow the
/// allocation one cluster at a time. Other platforms keep their existing write
/// behavior until an equivalent allocation contract is selected explicitly.
#[cfg(windows)]
pub(crate) fn preallocate_file<T>(file: &T, path: &Path, bytes: u64) -> Result<()>
where
    T: std::os::windows::io::AsRawHandle + ?Sized,
{
    use std::ffi::c_void;
    use std::mem::size_of;
    use windows_sys::Win32::Storage::FileSystem::{
        FileAllocationInfo, SetFileInformationByHandle, FILE_ALLOCATION_INFO,
    };

    if bytes == 0 {
        return Ok(());
    }
    let allocation_size = i64::try_from(bytes).map_err(|_| Error::IoAt {
        action: "write to file",
        path: path.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("allocation size {bytes} exceeds Windows LARGE_INTEGER range"),
        ),
    })?;
    let info = FILE_ALLOCATION_INFO {
        AllocationSize: allocation_size,
    };
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle() as _,
            FileAllocationInfo,
            &info as *const FILE_ALLOCATION_INFO as *const c_void,
            size_of::<FILE_ALLOCATION_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(Error::IoAt {
            action: "write to file",
            path: path.to_path_buf(),
            source: std::io::Error::last_os_error(),
        });
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) fn preallocate_file<T>(file: &T, path: &Path, bytes: u64) -> Result<()>
where
    T: std::os::fd::AsRawFd + ?Sized,
{
    if bytes == 0 {
        return Ok(());
    }

    let length = libc::off_t::try_from(bytes).map_err(|_| crate::error::Error::IoAt {
        action: "write to file",
        path: path.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("allocation size {bytes} exceeds Linux off_t range"),
        ),
    })?;

    loop {
        // SAFETY: fallocate only borrows this valid file descriptor for the duration
        // of the call. KEEP_SIZE reserves blocks without extending logical EOF.
        let result =
            unsafe { libc::fallocate(file.as_raw_fd(), libc::FALLOC_FL_KEEP_SIZE, 0, length) };
        if result == 0 {
            return Ok(());
        }

        let source = std::io::Error::last_os_error();
        match source.raw_os_error() {
            Some(libc::EINTR) => continue,
            // Some filesystems do not implement KEEP_SIZE. Preserve the former
            // no-op behavior there instead of making downloads unusable.
            Some(libc::EOPNOTSUPP | libc::ENOSYS | libc::EINVAL) => {
                return Ok(());
            }
            _ => {
                return Err(crate::error::Error::IoAt {
                    action: "write to file",
                    path: path.to_path_buf(),
                    source,
                });
            }
        }
    }
}

#[cfg(all(not(windows), not(target_os = "linux")))]
pub(crate) fn preallocate_file<T: ?Sized>(_file: &T, _path: &Path, _bytes: u64) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn preallocation_preserves_logical_eof() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("allocated.bin");
        let mut file = std::fs::File::create(&path).unwrap();

        preallocate_file(&file, &path, 1024 * 1024).unwrap();
        assert_eq!(file.metadata().unwrap().len(), 0);

        file.write_all(b"griffr").unwrap();
        assert_eq!(file.metadata().unwrap().len(), 6);
    }
}
