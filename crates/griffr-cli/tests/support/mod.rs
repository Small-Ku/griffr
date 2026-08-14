use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    volume: u64,
    file: u64,
    links: u64,
}

impl FileIdentity {
    pub fn links(self) -> u64 {
        self.links
    }

    fn same_file(self, other: Self) -> bool {
        self.volume == other.volume && self.file == other.file
    }
}

#[cfg(unix)]
pub fn file_identity(path: &Path) -> io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::metadata(path)?;
    Ok(FileIdentity {
        volume: metadata.dev(),
        file: metadata.ino(),
        links: metadata.nlink(),
    })
}

#[cfg(windows)]
pub fn file_identity(path: &Path) -> io::Result<FileIdentity> {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let file = fs::File::open(path)?;
    let mut info = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    let result = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(FileIdentity {
        volume: u64::from(info.dwVolumeSerialNumber),
        file: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        links: u64::from(info.nNumberOfLinks),
    })
}

#[cfg(not(any(unix, windows)))]
pub fn file_identity(path: &Path) -> io::Result<FileIdentity> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "hardlink identity checks are unsupported on this platform for {}",
            path.display()
        ),
    ))
}

pub fn assert_same_hardlink(left: &Path, right: &Path, context: &str) {
    let left_identity = file_identity(left)
        .unwrap_or_else(|error| panic!("failed to inspect {}: {error}", left.display()));
    let right_identity = file_identity(right)
        .unwrap_or_else(|error| panic!("failed to inspect {}: {error}", right.display()));
    assert!(
        left_identity.same_file(right_identity),
        "{context}: expected the same physical file; left={left_identity:?} right={right_identity:?}"
    );
    assert!(
        left_identity.links() >= 2 && right_identity.links() >= 2,
        "{context}: expected hardlink count >= 2; left={left_identity:?} right={right_identity:?}"
    );
}

pub fn assert_distinct_files(left: &Path, right: &Path, context: &str) {
    let left_identity = file_identity(left)
        .unwrap_or_else(|error| panic!("failed to inspect {}: {error}", left.display()));
    let right_identity = file_identity(right)
        .unwrap_or_else(|error| panic!("failed to inspect {}: {error}", right.display()));
    assert!(
        !left_identity.same_file(right_identity),
        "{context}: expected distinct physical files; left={left_identity:?} right={right_identity:?}"
    );
}
