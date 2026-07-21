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
use md5::{Digest, Md5};

use crate::runtime::issues::{FileIssue, FileIssueKind};

pub(crate) fn execute_verify(
    path: &Path,
    logical_path: &str,
    expected_md5: &str,
    expected_size: Option<u64>,
    on_fail: Option<Box<super::types::Task>>,
    event_tx: &flume::Sender<super::types::WorkerEvent>,
) -> super::graph::TaskExecution {
    let issue = build_issue(path, logical_path, expected_md5, expected_size);
    match issue {
        None => {
            let _ = event_tx.send(super::types::WorkerEvent::Verified {
                path: logical_path.to_string(),
                ok: true,
                issue: None,
            });
            super::graph::TaskExecution::succeeded()
        }
        Some(issue) => {
            if let Some(task) = on_fail {
                let _ = event_tx.send(super::types::WorkerEvent::Retried {
                    path: logical_path.to_string(),
                    reason: format!("verification failed ({:?})", issue.kind),
                });
                return super::graph::TaskExecution::then(*task);
            }

            let _ = event_tx.send(super::types::WorkerEvent::Verified {
                path: logical_path.to_string(),
                ok: false,
                issue: Some(issue.clone()),
            });
            super::graph::TaskExecution::failed(format!("verification failed ({:?})", issue.kind))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ArtifactKey {
    path: PathBuf,
    expected_md5: String,
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

/// Batch-local proof that a path still has the metadata observed when its MD5
/// was last validated. The cache never survives the command invocation.
#[derive(Debug, Default)]
pub(crate) struct VerifiedArtifactCache {
    entries: Mutex<HashMap<ArtifactKey, CachedArtifactCheck>>,
}

impl VerifiedArtifactCache {
    pub(crate) fn build_issue(
        &self,
        path: &Path,
        logical_path: &str,
        expected_md5: &str,
        expected_size: Option<u64>,
    ) -> Option<FileIssue> {
        build_issue_impl(Some(self), path, logical_path, expected_md5, expected_size)
    }
}

pub(crate) fn build_issue(
    path: &Path,
    logical_path: &str,
    expected_md5: &str,
    expected_size: Option<u64>,
) -> Option<FileIssue> {
    build_issue_impl(None, path, logical_path, expected_md5, expected_size)
}

fn build_issue_impl(
    cache: Option<&VerifiedArtifactCache>,
    path: &Path,
    logical_path: &str,
    expected_md5: &str,
    expected_size: Option<u64>,
) -> Option<FileIssue> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => {
            return Some(FileIssue {
                path: logical_path.to_string(),
                expected_md5: expected_md5.to_string(),
                expected_size: expected_size.unwrap_or(0),
                actual_size: None,
                actual_md5: None,
                kind: FileIssueKind::Missing,
            });
        }
    };

    let normalized_md5 = expected_md5.to_ascii_lowercase();
    let key = ArtifactKey {
        path: path.to_path_buf(),
        expected_md5: normalized_md5.clone(),
        expected_size,
    };
    let stamp = ArtifactStamp::from_metadata(&metadata);
    let cacheable = stamp.modified_nanos.is_some();
    if cacheable {
        if let Some(cached) = cache.and_then(|cache| {
            cache
                .entries
                .lock()
                .unwrap()
                .get(&key)
                .filter(|cached| cached.stamp == stamp)
                .cloned()
        }) {
            return cached.issue;
        }
    }

    let issue = if expected_size.is_some_and(|expected| metadata.len() != expected) {
        Some(FileIssue {
            path: logical_path.to_string(),
            expected_md5: expected_md5.to_string(),
            expected_size: expected_size.unwrap_or(metadata.len()),
            actual_size: Some(metadata.len()),
            actual_md5: None,
            kind: FileIssueKind::SizeMismatch,
        })
    } else {
        match file_md5(path) {
            Ok(actual_md5) if actual_md5 == normalized_md5 => None,
            Ok(actual_md5) => Some(FileIssue {
                path: logical_path.to_string(),
                expected_md5: expected_md5.to_string(),
                expected_size: expected_size.unwrap_or(metadata.len()),
                actual_size: Some(metadata.len()),
                actual_md5: Some(actual_md5),
                kind: FileIssueKind::Md5Mismatch,
            }),
            Err(_) => Some(FileIssue {
                path: logical_path.to_string(),
                expected_md5: expected_md5.to_string(),
                expected_size: expected_size.unwrap_or(metadata.len()),
                actual_size: Some(metadata.len()),
                actual_md5: None,
                kind: FileIssueKind::Md5Mismatch,
            }),
        }
    };

    if cacheable {
        if let Some(cache) = cache {
            cache.entries.lock().unwrap().insert(
                key,
                CachedArtifactCheck {
                    stamp,
                    issue: issue.clone(),
                },
            );
        }
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
    expected_md5: &str,
    expected_size: u64,
    is_cancelled: impl Fn() -> bool,
) -> CandidateVerification {
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
    let mut hasher = Md5::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        if is_cancelled() {
            return CandidateVerification::Cancelled;
        }
        let read = match file.read(&mut buffer) {
            Ok(read) => read,
            Err(_) => return CandidateVerification::Invalid,
        };
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual_md5 = crate::to_hex(&hasher.finalize());
    if actual_md5 == expected_md5.to_ascii_lowercase() {
        CandidateVerification::Valid
    } else {
        CandidateVerification::Invalid
    }
}

pub(crate) fn file_md5(path: &Path) -> Result<String> {
    let mut file = open_sequential_read(path).map_err(|e| Error::OpenFileFailed {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(crate::to_hex(&hasher.finalize()))
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
    use md5::{Digest, Md5};
    use std::fs;

    #[test]
    fn cached_mismatch_is_invalidated_when_file_metadata_changes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("artifact.bin");
        fs::write(&path, b"x").unwrap();
        let cache = VerifiedArtifactCache::default();
        assert!(cache
            .build_issue(&path, "artifact.bin", "invalid", Some(2))
            .is_some());

        fs::write(&path, b"ok").unwrap();
        let expected = crate::to_hex(&Md5::digest(b"ok"));
        assert!(cache
            .build_issue(&path, "artifact.bin", &expected, Some(2))
            .is_none());
    }
}
