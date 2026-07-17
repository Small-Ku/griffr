use std::path::PathBuf;

use crate::runtime::issues::FileIssue;
use crate::runtime::PatchPreflightReport;

#[derive(Debug, Clone)]
pub(crate) enum WorkerEvent {
    DownloadStarted {
        path: String,
        total_bytes: u64,
    },
    Downloaded {
        path: String,
        bytes: u64,
    },
    DownloadedBytes {
        path: String,
        bytes: u64,
        total_bytes: u64,
    },
    DownloadReset {
        path: String,
        bytes: u64,
    },
    Verified {
        path: String,
        ok: bool,
        issue: Option<FileIssue>,
    },
    Retried {
        path: String,
        reason: String,
    },
    Extracted {
        path: PathBuf,
    },
    Changed {
        path: String,
    },
    ExtractedBytes {
        path: String,
        bytes: u64,
        total_bytes: u64,
    },
    ArchiveCommitProgress {
        path: String,
        completed: usize,
        total: usize,
    },
    ArchivePreflight {
        path: String,
        report: PatchPreflightReport,
    },
    PatchProgress {
        path: String,
        completed: usize,
        total: usize,
    },
    DeleteProgress {
        path: String,
        completed: usize,
        total: usize,
    },
    Hardlinked {
        path: PathBuf,
    },
    Copied {
        path: PathBuf,
    },
    Failed {
        path: String,
        reason: String,
    },
}

#[derive(Debug, Clone)]
pub enum TaskOutcome {
    ArchivePreflight {
        path: String,
        report: PatchPreflightReport,
    },
    Downloaded {
        path: String,
        bytes: u64,
    },
    Verified {
        path: String,
        ok: bool,
        issue: Option<FileIssue>,
    },
    Extracted {
        path: PathBuf,
    },
    Changed {
        path: String,
    },
    Hardlinked {
        path: PathBuf,
    },
    Copied {
        path: PathBuf,
    },
    Failed {
        path: String,
        reason: String,
    },
}

impl WorkerEvent {
    pub(crate) fn into_outcome(self) -> Option<TaskOutcome> {
        match self {
            Self::ArchivePreflight { path, report } => {
                Some(TaskOutcome::ArchivePreflight { path, report })
            }
            Self::Downloaded { path, bytes } => Some(TaskOutcome::Downloaded { path, bytes }),
            Self::Verified { path, ok, issue } => Some(TaskOutcome::Verified { path, ok, issue }),
            Self::Extracted { path } => Some(TaskOutcome::Extracted { path }),
            Self::Changed { path } => Some(TaskOutcome::Changed { path }),
            Self::Hardlinked { path } => Some(TaskOutcome::Hardlinked { path }),
            Self::Copied { path } => Some(TaskOutcome::Copied { path }),
            Self::Failed { path, reason } => Some(TaskOutcome::Failed { path, reason }),
            Self::DownloadStarted { .. }
            | Self::DownloadedBytes { .. }
            | Self::DownloadReset { .. }
            | Self::Retried { .. }
            | Self::ExtractedBytes { .. }
            | Self::ArchiveCommitProgress { .. }
            | Self::PatchProgress { .. }
            | Self::DeleteProgress { .. } => None,
        }
    }
}

#[derive(Debug)]
pub struct TaskPoolResult {
    pub outcomes: Vec<TaskOutcome>,
}

pub struct TaskPoolRunner {
    pub(crate) ctx: crate::runtime::task_pool::scheduler::WorkerContext,
    pub(crate) event_rx: flume::Receiver<WorkerEvent>,
}
