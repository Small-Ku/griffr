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
    use std::os::windows::fs::MetadataExt;

    let metadata = fs::metadata(path)?;
    let missing = |field: &'static str| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "Windows filesystem metadata did not expose {field} for {}",
                path.display()
            ),
        )
    };
    Ok(FileIdentity {
        volume: u64::from(
            metadata
                .volume_serial_number()
                .ok_or_else(|| missing("volume serial number"))?,
        ),
        file: metadata.file_index().ok_or_else(|| missing("file index"))?,
        links: u64::from(
            metadata
                .number_of_links()
                .ok_or_else(|| missing("hardlink count"))?,
        ),
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
