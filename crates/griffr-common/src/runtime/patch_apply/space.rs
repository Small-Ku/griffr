use std::path::Path;

#[cfg(windows)]
use crate::error::Error;
use crate::error::Result;

#[cfg(windows)]
pub fn available_space(path: &Path) -> Result<Option<u64>> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let mut probe = path;
    while !probe.exists() {
        let Some(parent) = probe.parent() else {
            break;
        };
        if parent == probe {
            break;
        }
        probe = parent;
    }
    let mut wide: Vec<u16> = probe.as_os_str().encode_wide().collect();
    wide.push(0);
    let mut available = 0u64;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(Error::IoAt {
            action: "query file metadata/stat for",
            path: probe.to_path_buf(),
            source: std::io::Error::last_os_error(),
        });
    }
    Ok(Some(available))
}

#[cfg(unix)]
pub fn available_space(path: &Path) -> Result<Option<u64>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let mut probe = path;
    while !probe.exists() {
        let Some(parent) = probe.parent() else {
            break;
        };
        if parent == probe {
            break;
        }
        probe = parent;
    }

    let c_path =
        CString::new(probe.as_os_str().as_bytes()).map_err(|_| crate::error::Error::IoAt {
            action: "query file metadata/stat for",
            path: probe.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path contains an interior NUL byte",
            ),
        })?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: stat points to writable storage and c_path is NUL-terminated.
    if unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(crate::error::Error::IoAt {
            action: "query file metadata/stat for",
            path: probe.to_path_buf(),
            source: std::io::Error::last_os_error(),
        });
    }
    // SAFETY: statvfs returned success and initialized the structure.
    let stat = unsafe { stat.assume_init() };
    let available = u128::from(stat.f_bavail)
        .saturating_mul(u128::from(stat.f_frsize))
        .min(u128::from(u64::MAX)) as u64;
    Ok(Some(available))
}

#[cfg(not(any(windows, unix)))]
pub fn available_space(_path: &Path) -> Result<Option<u64>> {
    Ok(None)
}

#[cfg(all(test, any(windows, unix)))]
mod tests {
    use super::*;

    #[test]
    fn reads_space_for_existing_and_missing_paths() {
        let cwd = std::env::current_dir().expect("current dir");
        assert!(available_space(&cwd).unwrap().unwrap_or(0) > 0);

        let missing = cwd.join("griffr-test").join("space").join("missing");
        assert!(available_space(&missing).unwrap().unwrap_or(0) > 0);
    }
}
