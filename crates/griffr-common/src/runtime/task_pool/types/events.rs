use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use super::super::graph::TaskGraphSummary;
use crate::runtime::issues::FileIssue;
use crate::runtime::{PatchCheckReport, ProgressPhase};

/// Transient worker communication. Durable facts are carried only by
/// `Outcome`; progress and retries are never retained in task history.
#[derive(Debug, Clone)]
pub(crate) enum WorkerEvent {
    Progress {
        phase: ProgressPhase,
        path: String,
        finished: u64,
        total: u64,
        reset: bool,
    },
    Retried {
        path: String,
        reason: String,
    },
    Outcome(TaskOutcome),
}

impl WorkerEvent {
    pub(crate) fn progress(
        phase: ProgressPhase,
        path: String,
        finished: u64,
        total: u64,
        reset: bool,
    ) -> Self {
        Self::Progress {
            phase,
            path,
            finished,
            total,
            reset,
        }
    }

    pub(crate) fn downloaded(path: String, bytes: u64) -> Self {
        Self::Outcome(TaskOutcome::Downloaded { path, bytes })
    }

    pub(crate) fn verified(path: String, ok: bool, issue: Option<FileIssue>) -> Self {
        Self::Outcome(TaskOutcome::Verified { path, ok, issue })
    }

    pub(crate) fn changed(path: String) -> Self {
        Self::Outcome(TaskOutcome::Changed { path })
    }

    pub(crate) fn archive_check(path: String, report: PatchCheckReport) -> Self {
        Self::Outcome(TaskOutcome::ArchiveCheck { path, report })
    }

    pub(crate) fn hardlinked(path: PathBuf) -> Self {
        Self::Outcome(TaskOutcome::Hardlinked { path })
    }

    pub(crate) fn copied(path: PathBuf) -> Self {
        Self::Outcome(TaskOutcome::Copied { path })
    }

    pub(crate) fn failed(path: String, reason: String) -> Self {
        Self::Outcome(TaskOutcome::Failed { path, reason })
    }
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
    pub finished_tasks: usize,
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
