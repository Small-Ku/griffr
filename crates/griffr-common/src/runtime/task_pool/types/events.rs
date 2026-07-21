use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use super::super::graph::TaskGraphSummary;
use crate::runtime::issues::FileIssue;
use crate::runtime::PatchCheckReport;

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
    ArchiveCheck {
        path: String,
        report: PatchCheckReport,
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
    ArchiveCheck {
        path: String,
        report: PatchCheckReport,
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
            Self::ArchiveCheck { path, report } => Some(TaskOutcome::ArchiveCheck { path, report }),
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

#[derive(Debug, Clone, Default)]
pub struct VolumeTaskMetrics {
    pub read_tasks: usize,
    pub write_tasks: usize,
    pub metadata_tasks: usize,
    pub read_bytes: u64,
    pub write_bytes: u64,
    pub read_service_time: Duration,
    pub write_service_time: Duration,
    pub metadata_service_time: Duration,
}

impl VolumeTaskMetrics {
    pub fn read_bytes_per_second(&self) -> f64 {
        bytes_per_second(self.read_bytes, self.read_service_time)
    }

    pub fn write_bytes_per_second(&self) -> f64 {
        bytes_per_second(self.write_bytes, self.write_service_time)
    }
}

#[derive(Debug, Clone, Default)]
pub struct TaskPoolMetrics {
    pub completed_tasks: usize,
    pub graph: TaskGraphSummary,
    pub queue_wait_p50: Duration,
    pub queue_wait_p95: Duration,
    pub task_duration_p50: Duration,
    pub task_duration_p95: Duration,
    pub volumes: BTreeMap<String, VolumeTaskMetrics>,
}

fn bytes_per_second(bytes: u64, duration: Duration) -> f64 {
    let seconds = duration.as_secs_f64();
    if seconds > 0.0 {
        bytes as f64 / seconds
    } else {
        0.0
    }
}

#[derive(Debug)]
pub struct TaskPoolResult {
    pub outcomes: Vec<TaskOutcome>,
    pub metrics: TaskPoolMetrics,
}

pub struct TaskPoolRunner {
    pub(crate) config: super::TaskPoolConfig,
    pub(crate) dispatcher: std::sync::Arc<compio::dispatcher::Dispatcher>,
    pub(crate) event_tx: flume::Sender<WorkerEvent>,
    pub(crate) event_rx: flume::Receiver<WorkerEvent>,
}
