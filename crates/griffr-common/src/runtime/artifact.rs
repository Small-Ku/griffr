use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::api::types::GameFileEntry;

use super::ContentHash;

/// Expected final content for one path in the install tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactExpectation {
    logical_path: String,
    expected_hash: ContentHash,
    expected_size: Option<u64>,
}

/// Declares authoritative expected content for one physical output path.
/// Claims survive task moves and execution so later integrity closure does not
/// assign the same path to another manifest provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactClaim {
    path: PathBuf,
    expectation: ArtifactExpectation,
}

impl ArtifactClaim {
    pub fn new(path: PathBuf, expectation: ArtifactExpectation) -> Self {
        Self { path, expectation }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn expectation(&self) -> &ArtifactExpectation {
        &self.expectation
    }

    pub fn matches_game_file_path(&self, install_root: &Path, entry: &GameFileEntry) -> bool {
        paths_resolve_to_same_file(&self.path, &install_root.join(&entry.path))
    }

    pub fn expects_game_file(&self, entry: &GameFileEntry) -> bool {
        self.expectation.expected_hash
            == ContentHash::from(&entry.md5)
            && self.expectation.expected_size == Some(entry.size)
    }
}
impl ArtifactExpectation {
    pub fn new(
        logical_path: impl AsRef<str>,
        expected_hash: impl Into<ContentHash>,
        expected_size: Option<u64>,
    ) -> Self {
        Self {
            logical_path: logical_path.as_ref().replace('\\', "/"),
            expected_hash: expected_hash.into(),
            expected_size,
        }
    }

    pub fn md5(
        logical_path: impl AsRef<str>,
        expected_md5: impl AsRef<str>,
        expected_size: Option<u64>,
    ) -> crate::error::Result<Self> {
        Ok(Self::new(
            logical_path,
            ContentHash::md5(expected_md5)?,
            expected_size,
        ))
    }

    pub fn from_game_file(entry: &GameFileEntry) -> Self {
        Self::new(
            &entry.path,
            ContentHash::from(&entry.md5),
            Some(entry.size),
        )
    }

    pub fn logical_path(&self) -> &str {
        &self.logical_path
    }

    pub fn expected_hash(&self) -> &ContentHash {
        &self.expected_hash
    }

    /// Compatibility accessor for Hypergryph/Gryphline manifest-only code.
    pub fn expected_md5(&self) -> &str {
        match &self.expected_hash {
            ContentHash::Md5(value) => value,
            ContentHash::Crc64Xz(_) => panic!("expected MD5 artifact, found CRC64-XZ"),
        }
    }

    pub fn expected_size(&self) -> Option<u64> {
        self.expected_size
    }

    pub(crate) fn accepts_digest(&self, digest: &ArtifactDigest) -> bool {
        self.expected_size
            .is_none_or(|expected| expected == digest.bytes)
            && self.expected_hash == digest.hash
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArtifactDigest {
    pub(crate) bytes: u64,
    pub(crate) hash: ContentHash,
}

impl ArtifactDigest {
    pub(crate) fn new(bytes: u64, hash: impl Into<ContentHash>) -> Self {
        Self {
            bytes,
            hash: hash.into(),
        }
    }
}

/// Describes how the bytes for a committed final path were produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactSource {
    Existing,
    Archive,
    ArchiveRepair,
    Download,
    ReuseHardlink,
    ReuseCopy,
    LocalPatch,
    HdiffPatch,
}

/// Command-local proof that final content was verified before or while it was
/// committed. The metadata stamp prevents a later integrity closure from
/// trusting the proof after another process changes the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactProof {
    path: PathBuf,
    expectation: ArtifactExpectation,
    source: ArtifactSource,
    observed_size: u64,
    modified_nanos: Option<u128>,
}

impl ArtifactProof {
    pub(crate) fn from_verified_path(
        path: &Path,
        expectation: ArtifactExpectation,
        source: ArtifactSource,
    ) -> std::io::Result<Self> {
        let metadata = std::fs::metadata(path)?;
        if !metadata.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("artifact path is not a file: {}", path.display()),
            ));
        }
        if let Some(expected_size) = expectation.expected_size() {
            if metadata.len() != expected_size {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "artifact path {} has size {}, expected {}",
                        path.display(),
                        metadata.len(),
                        expected_size
                    ),
                ));
            }
        }
        let modified_nanos = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos());
        Ok(Self {
            path: path.to_path_buf(),
            expectation,
            source,
            observed_size: metadata.len(),
            modified_nanos,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn logical_path(&self) -> &str {
        self.expectation.logical_path()
    }

    pub fn expected_hash(&self) -> &ContentHash {
        self.expectation.expected_hash()
    }

    pub fn expected_md5(&self) -> &str {
        self.expectation.expected_md5()
    }

    pub fn expected_size(&self) -> Option<u64> {
        self.expectation.expected_size()
    }

    pub fn observed_size(&self) -> u64 {
        self.observed_size
    }

    pub fn source(&self) -> ArtifactSource {
        self.source
    }

    pub fn matches_game_file(&self, install_root: &Path, entry: &GameFileEntry) -> bool {
        let manifest_path = install_root.join(&entry.path);
        paths_resolve_to_same_file(&self.path, &manifest_path)
            && self.expectation.expected_hash
                == ContentHash::from(&entry.md5)
            && self.expectation.expected_size == Some(entry.size)
    }

    pub fn is_current(&self) -> bool {
        let Ok(metadata) = std::fs::metadata(&self.path) else {
            return false;
        };
        if !metadata.is_file() || metadata.len() != self.observed_size {
            return false;
        }
        let current_modified = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos());
        self.modified_nanos.is_some() && current_modified == self.modified_nanos
    }
}

fn paths_resolve_to_same_file(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => physical_path_key(left) == physical_path_key(right),
    }
}

pub(crate) fn physical_path_key(path: &Path) -> String {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|current| current.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };

    // Canonicalize the longest existing ancestor so a not-yet-created file
    // below a directory symlink compares equal to its concrete external path.
    let mut ancestor = absolute.as_path();
    let mut suffix = Vec::new();
    let canonical_ancestor = loop {
        match std::fs::canonicalize(ancestor) {
            Ok(path) => break path,
            Err(_) => {
                let Some(name) = ancestor.file_name() else {
                    break ancestor.to_path_buf();
                };
                suffix.push(name.to_os_string());
                let Some(parent) = ancestor.parent() else {
                    break ancestor.to_path_buf();
                };
                ancestor = parent;
            }
        }
    };

    let mut resolved = canonical_ancestor;
    for component in suffix.into_iter().rev() {
        resolved.push(component);
    }

    // Remove lexical `.` and `..` components left below a missing ancestor.
    let mut normalized = PathBuf::new();
    for component in resolved.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    let normalized = normalized.to_string_lossy().replace('\\', "/");
    if cfg!(any(windows, target_os = "macos")) {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn physical_keys_preserve_case_on_case_sensitive_platforms() {
        assert_ne!(
            physical_path_key(Path::new("Data")),
            physical_path_key(Path::new("data"))
        );
    }

    #[test]
    fn proof_is_invalid_after_committed_file_changes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("file.bin");
        std::fs::write(&path, b"old").unwrap();
        let proof = ArtifactProof::from_verified_path(
            &path,
            ArtifactExpectation::new("file.bin", ContentHash::Md5("unused".to_string()), Some(3)),
            ArtifactSource::Archive,
        )
        .unwrap();

        assert!(proof.is_current());
        std::fs::write(&path, b"new-size").unwrap();
        assert!(!proof.is_current());
    }

    #[cfg(unix)]
    #[test]
    fn proof_matches_manifest_path_through_external_asset_link() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let install_root = temp.path().join("install");
        let external_root = temp.path().join("external-assets");
        std::fs::create_dir_all(&install_root).unwrap();
        std::fs::create_dir_all(&external_root).unwrap();
        symlink(&external_root, install_root.join("VFS")).unwrap();
        let concrete = external_root.join("file.bin");
        std::fs::write(&concrete, b"data").unwrap();
        let proof = ArtifactProof::from_verified_path(
            &concrete,
            ArtifactExpectation::new("VFS/file.bin", "abcd", Some(4)),
            ArtifactSource::HdiffPatch,
        )
        .unwrap();

        assert!(proof.matches_game_file(
            &install_root,
            &GameFileEntry {
                path: "VFS/file.bin".to_string(),
                md5: "ABCD".to_string(),
                size: 4,
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn missing_output_below_asset_link_has_one_physical_key() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("install");
        let external = temp.path().join("external");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::create_dir_all(&external).unwrap();
        symlink(&external, install.join("Assets")).unwrap();

        assert_eq!(
            physical_path_key(&install.join("Assets/VFS/new.bin")),
            physical_path_key(&external.join("VFS/new.bin"))
        );
    }

    #[test]
    fn proof_matches_only_the_same_manifest_artifact() {
        let temp = tempfile::tempdir().unwrap();
        let install_root = temp.path().join("install");
        let path = install_root.join("Data/file.bin");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"data").unwrap();
        let proof = ArtifactProof::from_verified_path(
            &path,
            ArtifactExpectation::new("Data/file.bin", "abcd", Some(4)),
            ArtifactSource::Download,
        )
        .unwrap();

        assert!(proof.matches_game_file(
            &install_root,
            &GameFileEntry {
                path: "Data/file.bin".to_string(),
                md5: "ABCD".to_string(),
                size: 4,
            }
        ));
        assert!(!proof.matches_game_file(
            &install_root,
            &GameFileEntry {
                path: "Data/other.bin".to_string(),
                md5: "abcd".to_string(),
                size: 4,
            }
        ));
    }
}
