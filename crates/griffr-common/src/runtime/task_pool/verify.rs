use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::error::{Error, Result};
use md5::{Digest, Md5};

use crate::runtime::issues::{FileIssue, FileIssueKind};

pub(crate) fn execute_verify(
    path: &Path,
    logical_path: &str,
    expected_md5: &str,
    expected_size: Option<u64>,
    on_fail: Option<Box<super::types::Task>>,
    spawned: &mut Vec<super::types::Task>,
    event_tx: &flume::Sender<super::types::ProgressEvent>,
) {
    let issue = build_issue(path, logical_path, expected_md5, expected_size);
    match issue {
        None => {
            let _ = event_tx.send(super::types::ProgressEvent::Verified {
                path: logical_path.to_string(),
                ok: true,
                issue: None,
            });
        }
        Some(issue) => {
            if let Some(task) = on_fail {
                let _ = event_tx.send(super::types::ProgressEvent::Retried {
                    path: logical_path.to_string(),
                    reason: format!("verification failed ({:?})", issue.kind),
                });
                spawned.push(*task);
                return;
            }

            let _ = event_tx.send(super::types::ProgressEvent::Verified {
                path: logical_path.to_string(),
                ok: false,
                issue: Some(issue.clone()),
            });
            let _ = event_tx.send(super::types::ProgressEvent::Failed {
                path: logical_path.to_string(),
                reason: format!("verification failed ({:?})", issue.kind),
            });
        }
    }
}

pub(crate) fn build_issue(
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

    if let Some(expected_size) = expected_size {
        if metadata.len() != expected_size {
            return Some(FileIssue {
                path: logical_path.to_string(),
                expected_md5: expected_md5.to_string(),
                expected_size,
                actual_size: Some(metadata.len()),
                actual_md5: None,
                kind: FileIssueKind::SizeMismatch,
            });
        }
    }

    let actual_md5 = match file_md5(path) {
        Ok(md5) => md5,
        Err(_) => {
            return Some(FileIssue {
                path: logical_path.to_string(),
                expected_md5: expected_md5.to_string(),
                expected_size: expected_size.unwrap_or(metadata.len()),
                actual_size: Some(metadata.len()),
                actual_md5: None,
                kind: FileIssueKind::Md5Mismatch,
            });
        }
    };
    if actual_md5 != expected_md5.to_lowercase() {
        return Some(FileIssue {
            path: logical_path.to_string(),
            expected_md5: expected_md5.to_string(),
            expected_size: expected_size.unwrap_or(metadata.len()),
            actual_size: Some(metadata.len()),
            actual_md5: Some(actual_md5),
            kind: FileIssueKind::Md5Mismatch,
        });
    }

    None
}

pub(crate) fn file_md5(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|e| Error::OpenFileFailed {
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
    Ok(format!("{:x}", hasher.finalize()))
}
