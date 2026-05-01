use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileIssueKind {
    Missing,
    SizeMismatch,
    Md5Mismatch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileIssue {
    pub path: String,
    pub expected_md5: String,
    pub expected_size: u64,
    pub actual_size: Option<u64>,
    pub actual_md5: Option<String>,
    pub kind: FileIssueKind,
}
