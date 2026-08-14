use serde::{Deserialize, Serialize};

use super::ContentHash;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileIssueKind {
    Missing,
    SizeMismatch,
    HashMismatch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileIssue {
    pub path: String,
    pub expected_hash: ContentHash,
    pub expected_size: u64,
    pub actual_size: Option<u64>,
    pub actual_hash: Option<ContentHash>,
    pub kind: FileIssueKind,
}
