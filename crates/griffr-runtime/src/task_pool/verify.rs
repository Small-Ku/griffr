use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_SEQUENTIAL_SCAN;

use crate::error::{Error, Result};
use crate::issues::{FileIssue, FileIssueKind};
use crate::ContentHash;

use super::blocking_buffer::with_blocking_io_buffer;

pub(crate) fn run_verify(
    path: &Path,
    logical_path: &str,
    expected_hash: &ContentHash,
    expected_size: Option<u64>,
    on_fail: Option<Box<super::types::Task>>,
    event_tx: &flume::Sender<super::types::WorkerEvent>,
) -> super::graph::TaskRun {
    let issue = build_issue(path, logical_path, expected_hash, expected_size);
    match issue {
        None => {
            let _ = event_tx.send(super::types::WorkerEvent::verified(
                logical_path.to_string(),
                true,
                None,
            ));
            super::graph::TaskRun::succeeded()
        }
        Some(issue) => {
            if let Some(task) = on_fail {
                let _ = event_tx.send(super::types::WorkerEvent::Retried {
                    path: logical_path.to_string(),
                    reason: format!("verification failed ({:?})", issue.kind),
                });
                return super::graph::TaskRun::then(*task);
            }

            let _ = event_tx.send(super::types::WorkerEvent::verified(
                logical_path.to_string(),
                false,
                Some(issue.clone()),
            ));
            super::graph::TaskRun::failed(format!("verification failed ({:?})", issue.kind))
        }
    }
}

pub(crate) fn run_metadata_verify(
    path: &Path,
    logical_path: &str,
    expected_hash: &ContentHash,
    expected_size: u64,
    on_fail: Option<Box<super::types::Task>>,
    event_tx: &flume::Sender<super::types::WorkerEvent>,
) -> super::graph::TaskRun {
    let issue = build_metadata_issue(path, logical_path, expected_hash, expected_size);
    match issue {
        None => {
            let _ = event_tx.send(super::types::WorkerEvent::verified(
                logical_path.to_string(),
                true,
                None,
            ));
            super::graph::TaskRun::succeeded()
        }
        Some(issue) => {
            if let Some(task) = on_fail {
                let _ = event_tx.send(super::types::WorkerEvent::Retried {
                    path: logical_path.to_string(),
                    reason: format!("metadata verification failed ({:?})", issue.kind),
                });
                return super::graph::TaskRun::then(*task);
            }
            let _ = event_tx.send(super::types::WorkerEvent::verified(
                logical_path.to_string(),
                false,
                Some(issue.clone()),
            ));
            super::graph::TaskRun::failed(format!(
                "metadata verification failed ({:?})",
                issue.kind
            ))
        }
    }
}

pub(crate) fn build_metadata_issue(
    path: &Path,
    logical_path: &str,
    expected_hash: impl Into<ContentHash>,
    expected_size: u64,
) -> Option<FileIssue> {
    let expected_hash = expected_hash.into();
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => {
            return Some(FileIssue {
                path: logical_path.to_string(),
                expected_hash: expected_hash.clone(),
                expected_size,
                actual_size: None,
                actual_hash: None,
                kind: FileIssueKind::Missing,
            });
        }
    };
    (metadata.len() != expected_size).then(|| FileIssue {
        path: logical_path.to_string(),
        expected_hash: expected_hash.clone(),
        expected_size,
        actual_size: Some(metadata.len()),
        actual_hash: None,
        kind: FileIssueKind::SizeMismatch,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ArtifactKey {
    path: PathBuf,
    expected_hash: ContentHash,
    expected_size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArtifactStamp {
    len: u64,
    modified_nanos: Option<u128>,
}

#[derive(Debug, Clone)]
struct CachedArtifactCheck {
    stamp: ArtifactStamp,
    issue: Option<FileIssue>,
}

impl ArtifactStamp {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        let modified_nanos = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos());
        Self {
            len: metadata.len(),
            modified_nanos,
        }
    }
}

/// Batch-local proof that a path still has the metadata observed when its
/// expected content hash was last validated. The cache never survives the
/// command invocation.
#[derive(Debug, Default)]
pub(crate) struct VerifiedArtifactCache {
    entries: Mutex<HashMap<ArtifactKey, CachedArtifactCheck>>,
}

impl VerifiedArtifactCache {
    pub(crate) fn build_issue(
        &self,
        path: &Path,
        logical_path: &str,
        expected_hash: impl Into<ContentHash>,
        expected_size: Option<u64>,
    ) -> Option<FileIssue> {
        let expected_hash = expected_hash.into();
        build_issue_impl(
            Some(self),
            path,
            logical_path,
            &expected_hash,
            expected_size,
        )
    }
}

pub(crate) fn build_issue(
    path: &Path,
    logical_path: &str,
    expected_hash: impl Into<ContentHash>,
    expected_size: Option<u64>,
) -> Option<FileIssue> {
    let expected_hash = expected_hash.into();
    build_issue_impl(None, path, logical_path, &expected_hash, expected_size)
}

fn build_issue_impl(
    cache: Option<&VerifiedArtifactCache>,
    path: &Path,
    logical_path: &str,
    expected_hash: &ContentHash,
    expected_size: Option<u64>,
) -> Option<FileIssue> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => {
            return Some(FileIssue {
                path: logical_path.to_string(),
                expected_hash: expected_hash.clone(),
                expected_size: expected_size.unwrap_or(0),
                actual_size: None,
                actual_hash: None,
                kind: FileIssueKind::Missing,
            });
        }
    };

    let cache_insert = if let Some(cache) = cache {
        let stamp = ArtifactStamp::from_metadata(&metadata);
        if stamp.modified_nanos.is_some() {
            let key = ArtifactKey {
                path: path.to_path_buf(),
                expected_hash: expected_hash.clone(),
                expected_size,
            };
            if let Some(cached) = cache
                .entries
                .lock()
                .unwrap()
                .get(&key)
                .filter(|cached| cached.stamp == stamp)
                .cloned()
            {
                return cached.issue;
            }
            Some((cache, key, stamp))
        } else {
            None
        }
    } else {
        None
    };

    let issue = if expected_size.is_some_and(|expected| metadata.len() != expected) {
        Some(FileIssue {
            path: logical_path.to_string(),
            expected_hash: expected_hash.clone(),
            expected_size: expected_size.unwrap_or(metadata.len()),
            actual_size: Some(metadata.len()),
            actual_hash: None,
            kind: FileIssueKind::SizeMismatch,
        })
    } else {
        match file_hash(path, expected_hash) {
            Ok(actual_hash) if actual_hash == *expected_hash => None,
            Ok(actual_hash) => Some(FileIssue {
                path: logical_path.to_string(),
                expected_hash: expected_hash.clone(),
                expected_size: expected_size.unwrap_or(metadata.len()),
                actual_size: Some(metadata.len()),
                actual_hash: Some(actual_hash),
                kind: FileIssueKind::HashMismatch,
            }),
            Err(_) => Some(FileIssue {
                path: logical_path.to_string(),
                expected_hash: expected_hash.clone(),
                expected_size: expected_size.unwrap_or(metadata.len()),
                actual_size: Some(metadata.len()),
                actual_hash: None,
                kind: FileIssueKind::HashMismatch,
            }),
        }
    };

    if let Some((cache, key, stamp)) = cache_insert {
        cache.entries.lock().unwrap().insert(
            key,
            CachedArtifactCheck {
                stamp,
                issue: issue.clone(),
            },
        );
    }
    issue
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CandidateVerification {
    Valid,
    Invalid,
    Cancelled,
}

pub(crate) fn verify_candidate_cancellable(
    path: &Path,
    expected_hash: impl Into<ContentHash>,
    expected_size: u64,
    is_cancelled: impl Fn() -> bool,
) -> CandidateVerification {
    let expected_hash = expected_hash.into();
    if is_cancelled() {
        return CandidateVerification::Cancelled;
    }
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() && metadata.len() == expected_size => {}
        Ok(_) | Err(_) => return CandidateVerification::Invalid,
    }
    let mut file = match open_sequential_read(path) {
        Ok(file) => file,
        Err(_) => return CandidateVerification::Invalid,
    };
    let mut hasher = expected_hash.hasher();
    let read_failure = with_blocking_io_buffer(|buffer| loop {
        if is_cancelled() {
            return Some(CandidateVerification::Cancelled);
        }
        let read = match file.read(buffer) {
            Ok(read) => read,
            Err(_) => return Some(CandidateVerification::Invalid),
        };
        if read == 0 {
            return None;
        }
        hasher.update(&buffer[..read]);
    });
    if let Some(result) = read_failure {
        return result;
    }
    if hasher.finalize() == expected_hash {
        CandidateVerification::Valid
    } else {
        CandidateVerification::Invalid
    }
}

pub(crate) fn file_hash(path: &Path, expected_hash: &ContentHash) -> Result<ContentHash> {
    let mut file = open_sequential_read(path).map_err(|e| Error::IoAt {
        action: "open file",
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut hasher = expected_hash.hasher();
    with_blocking_io_buffer(|buffer| -> std::io::Result<()> {
        loop {
            let n = file.read(buffer)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }
        Ok(())
    })?;
    Ok(hasher.finalize())
}

pub(crate) fn file_md5(path: &Path) -> Result<String> {
    let placeholder = ContentHash::Md5(String::new());
    match file_hash(path, &placeholder)? {
        ContentHash::Md5(value) => Ok(value),
        ContentHash::Crc64Xz(_) => unreachable!("MD5 placeholder selected MD5 hasher"),
    }
}

fn open_sequential_read(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    options.custom_flags(FILE_FLAG_SEQUENTIAL_SCAN);
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::VerifiedArtifactCache;
    use crate::ContentHash;
    use md5::{Digest, Md5};
    use std::fs;

    #[test]
    fn cached_mismatch_is_invalidated_when_file_metadata_changes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("artifact.bin");
        fs::write(&path, b"x").unwrap();
        let cache = VerifiedArtifactCache::default();
        let expected = ContentHash::md5("00000000000000000000000000000000").unwrap();
        assert!(cache
            .build_issue(&path, "artifact.bin", &expected, Some(2))
            .is_some());

        fs::write(&path, b"ok").unwrap();
        let expected = ContentHash::md5(griffr_core::to_hex(&Md5::digest(b"ok"))).unwrap();
        assert!(cache
            .build_issue(&path, "artifact.bin", &expected, Some(2))
            .is_none());
    }

    #[test]
    fn metadata_check_accepts_same_size_without_hashing_content() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("artifact.bin");
        fs::write(&path, b"bad").unwrap();
        let expected = ContentHash::md5(griffr_core::to_hex(&Md5::digest(b"ok!"))).unwrap();

        assert!(super::build_metadata_issue(&path, "artifact.bin", &expected, 3).is_none());
        assert!(super::build_issue(&path, "artifact.bin", &expected, Some(3)).is_some());
    }

    #[test]
    fn crc64_xz_verification_uses_the_expected_algorithm() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("artifact.bin");
        fs::write(&path, b"123456789").unwrap();
        let expected = ContentHash::crc64_xz_decimal("11051210869376104954").unwrap();
        assert!(super::build_issue(&path, "artifact.bin", &expected, Some(9)).is_none());
    }
}
