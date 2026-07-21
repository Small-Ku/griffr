use std::path::Path;

use crate::error::{Error, Result};

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

#[cfg(not(windows))]
pub fn available_space(_path: &Path) -> Result<Option<u64>> {
    Ok(None)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    #[ignore = "Uses host filesystem to query real free disk space"]
    fn reads_space_for_existing_and_missing_paths() {
        let cwd = std::env::current_dir().expect("current dir");
        assert!(available_space(&cwd).unwrap().unwrap_or(0) > 0);

        let missing = cwd.join("griffr-test").join("space").join("missing");
        assert!(available_space(&missing).unwrap().unwrap_or(0) > 0);
    }
}
