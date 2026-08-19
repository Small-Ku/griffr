use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_SEQUENTIAL_SCAN;

use crate::error::{Error, Result};
use crate::issues::{FileIssue, FileIssueKind};
use crate::ContentHash;

use super::blocking_buffer::with_blocking_io_buffer;
use super::types::Task;

const MD5_VERIFY_CHUNK_BYTES: usize = 256 * 1024;
static MD5_MANY: OnceLock<md5::Md5Many> = OnceLock::new();

fn md5_many() -> md5::Md5Many {
    *MD5_MANY.get_or_init(md5::Md5Many::new)
}

pub(crate) fn md5_verify_batch_capacity() -> usize {
    md5_verify_batch_capacity_for_lanes(md5_many().lanes())
}

fn md5_verify_batch_capacity_for_lanes(lanes: usize) -> usize {
    if lanes <= 1 {
        1
    } else {
        lanes.saturating_mul(3)
    }
}

pub(crate) fn is_md5_verify_task(task: &Task) -> bool {
    matches!(
        task,
        Task::Verify {
            expected_hash: ContentHash::Md5(_),
            ..
        }
    )
}

pub(crate) fn run_verify(
    path: &Path,
    logical_path: &str,
    expected_hash: &ContentHash,
    expected_size: Option<u64>,
    on_fail: Option<Box<super::types::Task>>,
    event_tx: &flume::Sender<super::types::WorkerEvent>,
) -> super::graph::TaskRun {
    finish_verify(
        logical_path,
        build_issue(path, logical_path, expected_hash, expected_size),
        on_fail,
        event_tx,
    )
}

fn finish_verify(
    logical_path: &str,
    issue: Option<FileIssue>,
    on_fail: Option<Box<Task>>,
    event_tx: &flume::Sender<super::types::WorkerEvent>,
) -> super::graph::TaskRun {
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

struct Md5VerifyEntry {
    file: Option<File>,
    logical_path: String,
    expected_hash: ContentHash,
    expected_size: Option<u64>,
    actual_size: Option<u64>,
    issue: Option<FileIssue>,
    on_fail: Option<Box<Task>>,
}

impl Md5VerifyEntry {
    fn from_task(task: &mut Task) -> Self {
        let Task::Verify {
            path,
            logical_path,
            expected_hash,
            expected_size,
            on_fail,
        } = task
        else {
            unreachable!("non-verify task entered MD5 verify batch")
        };
        debug_assert!(matches!(expected_hash, ContentHash::Md5(_)));

        let mut entry = Self {
            file: None,
            logical_path: logical_path.clone(),
            expected_hash: expected_hash.clone(),
            expected_size: *expected_size,
            actual_size: None,
            issue: None,
            on_fail: on_fail.take(),
        };
        let metadata = match std::fs::metadata(path.as_path()) {
            Ok(metadata) => metadata,
            Err(_) => {
                entry.issue = Some(FileIssue {
                    path: logical_path.clone(),
                    expected_hash: expected_hash.clone(),
                    expected_size: expected_size.unwrap_or(0),
                    actual_size: None,
                    actual_hash: None,
                    kind: FileIssueKind::Missing,
                });
                return entry;
            }
        };
        let actual_size = metadata.len();
        entry.actual_size = Some(actual_size);
        if expected_size.is_some_and(|expected| actual_size != expected) {
            entry.issue = Some(FileIssue {
                path: logical_path.clone(),
                expected_hash: expected_hash.clone(),
                expected_size: expected_size.unwrap_or(actual_size),
                actual_size: Some(actual_size),
                actual_hash: None,
                kind: FileIssueKind::SizeMismatch,
            });
            return entry;
        }
        match open_sequential_read(path.as_path()) {
            Ok(file) => entry.file = Some(file),
            Err(_) => entry.issue = Some(entry.hash_failure(None)),
        }
        entry
    }

    fn hash_failure(&self, actual_hash: Option<ContentHash>) -> FileIssue {
        FileIssue {
            path: self.logical_path.clone(),
            expected_hash: self.expected_hash.clone(),
            expected_size: self.expected_size.unwrap_or(self.actual_size.unwrap_or(0)),
            actual_size: self.actual_size,
            actual_hash,
            kind: FileIssueKind::HashMismatch,
        }
    }
}

pub(crate) fn run_md5_verify_batch(
    tasks: &mut [&mut Task],
    event_tx: &flume::Sender<super::types::WorkerEvent>,
) -> Vec<super::graph::TaskRun> {
    debug_assert!(tasks.len() >= 2);
    debug_assert!(tasks.iter().all(|task| is_md5_verify_task(task)));

    let engine = md5_many();
    let mut entries = tasks
        .iter_mut()
        .map(|task| Md5VerifyEntry::from_task(task))
        .collect::<Vec<_>>();
    let mut states = vec![md5::Md5State::new(); entries.len()];
    let mut buffers = entries
        .iter()
        .map(|entry| {
            let bytes = if entry.file.is_some() {
                entry
                    .actual_size
                    .unwrap_or(MD5_VERIFY_CHUNK_BYTES as u64)
                    .min(MD5_VERIFY_CHUNK_BYTES as u64)
                    .max(1) as usize
            } else {
                0
            };
            vec![0u8; bytes]
        })
        .collect::<Vec<_>>();
    let mut lengths = vec![0usize; entries.len()];

    loop {
        let mut any_read = false;
        for index in 0..entries.len() {
            lengths[index] = 0;
            let Some(file) = entries[index].file.as_mut() else {
                continue;
            };
            match file.read(&mut buffers[index]) {
                Ok(0) => entries[index].file = None,
                Ok(read) => {
                    lengths[index] = read;
                    any_read = true;
                }
                Err(_) => {
                    entries[index].file = None;
                    entries[index].issue = Some(entries[index].hash_failure(None));
                }
            }
        }
        if !any_read {
            break;
        }
        let inputs = buffers
            .iter()
            .zip(&lengths)
            .map(|(buffer, &length)| &buffer[..length])
            .collect::<Vec<_>>();
        engine.update_many(&mut states, &inputs);
    }

    let mut outputs = vec![[0u8; 16]; entries.len()];
    engine.finalize_many(&states, &mut outputs);
    entries
        .into_iter()
        .zip(outputs)
        .map(|(mut entry, digest)| {
            if entry.issue.is_none() {
                let actual_hash = ContentHash::Md5(griffr_core::to_hex(&digest));
                if actual_hash != entry.expected_hash {
                    entry.issue = Some(entry.hash_failure(Some(actual_hash)));
                }
            }
            finish_verify(&entry.logical_path, entry.issue, entry.on_fail, event_tx)
        })
        .collect()
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
    use crate::task_pool::Task;
    use crate::{ContentHash, FileIssueKind};
    use md5::{Digest, Md5};
    use std::fs;

    #[test]
    fn md5_batch_capacity_disables_batching_without_simd() {
        assert_eq!(super::md5_verify_batch_capacity_for_lanes(1), 1);
        assert_eq!(super::md5_verify_batch_capacity_for_lanes(4), 12);
        assert_eq!(super::md5_verify_batch_capacity_for_lanes(8), 24);
        assert_eq!(super::md5_verify_batch_capacity_for_lanes(16), 48);
    }

    #[test]
    fn md5_batch_verifier_handles_streaming_boundaries_and_mismatch() {
        let temp = tempfile::tempdir().unwrap();
        let payloads = [
            Vec::new(),
            vec![0x11; 55],
            vec![0x22; 64],
            vec![0x33; super::MD5_VERIFY_CHUNK_BYTES + 73],
        ];
        let mut tasks = payloads
            .iter()
            .enumerate()
            .map(|(index, payload)| {
                let path = temp.path().join(format!("{index}.bin"));
                fs::write(&path, payload).unwrap();
                let mut expected = griffr_core::to_hex(&md5::md5(payload));
                if index == payloads.len() - 1 {
                    expected = "00000000000000000000000000000000".to_string();
                }
                Task::Verify {
                    path,
                    logical_path: format!("{index}.bin"),
                    expected_hash: ContentHash::Md5(expected),
                    expected_size: Some(payload.len() as u64),
                    on_fail: None,
                }
            })
            .collect::<Vec<_>>();
        let mut task_refs = tasks.iter_mut().collect::<Vec<_>>();
        let (event_tx, event_rx) = flume::unbounded();

        let runs = super::run_md5_verify_batch(&mut task_refs, &event_tx);

        assert_eq!(runs.len(), payloads.len());
        assert!(runs[..3].iter().all(|run| run.failure_details().is_none()));
        assert!(runs[3].failure_details().is_some());
        let outcomes = event_rx
            .try_iter()
            .filter_map(|event| match event {
                super::super::types::WorkerEvent::Outcome(outcome) => Some(outcome),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(outcomes.len(), payloads.len());
        assert!(matches!(
            &outcomes[3],
            super::super::types::TaskOutcome::Verified {
                ok: false,
                issue: Some(issue),
                ..
            } if issue.kind == FileIssueKind::HashMismatch && issue.actual_hash.is_some()
        ));
    }

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
