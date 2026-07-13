use crate::api::protocol::MIN_USER_AGENT;
use crate::runtime::{ProgressLane, ProgressSender};
use std::path::PathBuf;

const DEFAULT_PARALLELISM_FALLBACK: usize = 4;
const DEFAULT_MAX_RETRIES: u32 = 3;
pub const DEFAULT_PROGRESS_BUFFER_BYTES: usize = 256 * 1024;

const MIN_IO_SLOTS: usize = 2;
const MAX_IO_SLOTS: usize = 16;
const MIN_CPU_SLOTS: usize = 1;
const MAX_CPU_SLOTS: usize = 16;
const MIN_EXTRACT_SLOTS: usize = 1;
const MAX_EXTRACT_SLOTS: usize = 4;
const MIN_MATERIALIZATION_IO_SLOTS: usize = 4;
const MAX_MATERIALIZATION_IO_SLOTS: usize = 24;
const MAX_VFS_REPAIR_IO_SLOTS: usize = 6;

use crate::runtime::issues::FileIssue;

#[derive(Debug, Clone)]
pub enum Task {
    InstallArchive {
        base_name: String,
        dest: PathBuf,
        cleanup: bool,
        password: Option<String>,
        parts: Vec<ArchivePart>,
    },
    Download {
        url: String,
        dest: PathBuf,
        logical_path: String,
        expected_md5: String,
        expected_size: Option<u64>,
        retry_count: u32,
    },
    Verify {
        path: PathBuf,
        logical_path: String,
        expected_md5: String,
        expected_size: Option<u64>,
        on_fail: Option<Box<Task>>,
    },
    EnsureFile {
        dest: PathBuf,
        logical_path: String,
        expected_md5: String,
        expected_size: u64,
        source_candidates: Vec<PathBuf>,
        download_url: Option<String>,
        allow_copy_fallback: bool,
        prefer_reuse: bool,
        retry_count: u32,
    },
    Extract {
        base_name: String,
        volumes: Vec<PathBuf>,
        dest: PathBuf,
        cleanup: bool,
        password: Option<String>,
    },
    ApplyExtractedVfsPatchManifest {
        install_root: PathBuf,
    },
    ApplyDeleteManifest {
        install_root: PathBuf,
    },
    Hardlink {
        src: PathBuf,
        dest: PathBuf,
    },
}

#[derive(Debug, Clone)]
pub struct ArchivePart {
    pub sequence: u64,
    pub url: String,
    pub dest: PathBuf,
    pub logical_path: String,
    pub expected_md5: String,
    pub expected_size: u64,
}

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
            Self::Downloaded { path, bytes } => Some(TaskOutcome::Downloaded { path, bytes }),
            Self::Verified { path, ok, issue } => Some(TaskOutcome::Verified { path, ok, issue }),
            Self::Extracted { path } => Some(TaskOutcome::Extracted { path }),
            Self::Hardlinked { path } => Some(TaskOutcome::Hardlinked { path }),
            Self::Copied { path } => Some(TaskOutcome::Copied { path }),
            Self::Failed { path, reason } => Some(TaskOutcome::Failed { path, reason }),
            Self::DownloadStarted { .. }
            | Self::DownloadedBytes { .. }
            | Self::Retried { .. }
            | Self::ExtractedBytes { .. }
            | Self::ArchiveCommitProgress { .. }
            | Self::PatchProgress { .. }
            | Self::DeleteProgress { .. } => None,
        }
    }
}

/// Maps task-pool facts onto frontend-neutral progress lanes for one batch.
///
/// A disabled sender keeps every lane unset so non-interactive callers pay no
/// aggregation or allocation cost for transient progress updates.
#[derive(Clone, Default)]
pub struct TaskProgress {
    pub(crate) sender: ProgressSender,
    pub(crate) verify: Option<(ProgressLane, u64)>,
    pub(crate) download: Option<ProgressLane>,
    pub(crate) extract: Option<ProgressLane>,
    pub(crate) commit: Option<ProgressLane>,
    pub(crate) patch: Option<ProgressLane>,
    pub(crate) delete: Option<ProgressLane>,
}

impl TaskProgress {
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn new(sender: ProgressSender) -> Self {
        Self {
            sender,
            ..Self::default()
        }
    }

    pub fn with_verify(mut self, lane: ProgressLane, total: usize) -> Self {
        if self.sender.is_enabled() {
            self.verify = Some((lane, total as u64));
        }
        self
    }

    pub fn with_download(mut self, lane: ProgressLane) -> Self {
        if self.sender.is_enabled() {
            self.download = Some(lane);
        }
        self
    }

    pub fn with_extract(mut self, lane: ProgressLane) -> Self {
        if self.sender.is_enabled() {
            self.extract = Some(lane);
        }
        self
    }

    pub fn with_commit(mut self, lane: ProgressLane) -> Self {
        if self.sender.is_enabled() {
            self.commit = Some(lane);
        }
        self
    }

    pub fn with_patch(mut self, lane: ProgressLane) -> Self {
        if self.sender.is_enabled() {
            self.patch = Some(lane);
        }
        self
    }

    pub fn with_delete(mut self, lane: ProgressLane) -> Self {
        if self.sender.is_enabled() {
            self.delete = Some(lane);
        }
        self
    }
}

#[derive(Debug, Clone)]
pub struct TaskPoolConfig {
    pub io_slots: usize,
    pub cpu_slots: usize,
    pub extract_slots: usize,
    pub max_retries: u32,
    pub user_agent: String,
    pub extraction_progress_buffer_bytes: usize,
    pub download_progress_buffer_bytes: usize,
}

impl TaskPoolConfig {
    pub fn with_progress_buffers(
        extraction_progress_buffer_bytes: usize,
        download_progress_buffer_bytes: usize,
    ) -> Self {
        Self {
            extraction_progress_buffer_bytes,
            download_progress_buffer_bytes,
            ..Self::default()
        }
    }

    pub fn with_download_progress_buffer(download_progress_buffer_bytes: usize) -> Self {
        Self {
            download_progress_buffer_bytes,
            ..Self::default()
        }
    }

    pub fn with_extract_slots(extract_slots: usize) -> Self {
        Self {
            extract_slots: extract_slots.max(MIN_EXTRACT_SLOTS),
            ..Self::default()
        }
    }

    pub fn for_file_reuse() -> Self {
        Self {
            io_slots: available_parallelism().clamp(MIN_IO_SLOTS, MAX_IO_SLOTS),
            ..Self::default()
        }
    }

    pub fn for_file_materialization() -> Self {
        Self {
            io_slots: available_parallelism()
                .clamp(MIN_MATERIALIZATION_IO_SLOTS, MAX_MATERIALIZATION_IO_SLOTS),
            ..Self::default()
        }
    }

    pub fn with_vfs_repair_limits(mut self) -> Self {
        self.io_slots = self.io_slots.min(MAX_VFS_REPAIR_IO_SLOTS);
        self
    }
}

impl Default for TaskPoolConfig {
    fn default() -> Self {
        let cpus = available_parallelism();
        Self {
            io_slots: (cpus * 2).clamp(MIN_IO_SLOTS, MAX_IO_SLOTS),
            cpu_slots: cpus.clamp(MIN_CPU_SLOTS, MAX_CPU_SLOTS),
            extract_slots: (cpus / 2).clamp(MIN_EXTRACT_SLOTS, MAX_EXTRACT_SLOTS),
            max_retries: DEFAULT_MAX_RETRIES,
            user_agent: MIN_USER_AGENT.to_owned(),
            extraction_progress_buffer_bytes: DEFAULT_PROGRESS_BUFFER_BYTES,
            download_progress_buffer_bytes: DEFAULT_PROGRESS_BUFFER_BYTES,
        }
    }
}

fn available_parallelism() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(DEFAULT_PARALLELISM_FALLBACK)
}

#[derive(Debug)]
pub struct TaskPoolResult {
    pub outcomes: Vec<TaskOutcome>,
}

pub struct TaskPoolRunner {
    pub(crate) ctx: crate::runtime::task_pool::scheduler::WorkerContext,
    pub(crate) event_rx: flume::Receiver<WorkerEvent>,
}

#[cfg(test)]
mod tests {
    use super::{TaskOutcome, WorkerEvent};

    #[test]
    fn transient_worker_progress_is_not_retained_as_an_outcome() {
        assert!(WorkerEvent::DownloadedBytes {
            path: "asset.bin".to_string(),
            bytes: 64,
            total_bytes: 128,
        }
        .into_outcome()
        .is_none());
        assert!(WorkerEvent::PatchProgress {
            path: "patch.json".to_string(),
            completed: 1,
            total: 2,
        }
        .into_outcome()
        .is_none());
    }

    #[test]
    fn durable_worker_facts_become_task_outcomes() {
        assert!(matches!(
            WorkerEvent::Downloaded {
                path: "asset.bin".to_string(),
                bytes: 128,
            }
            .into_outcome(),
            Some(TaskOutcome::Downloaded { path, bytes })
                if path == "asset.bin" && bytes == 128
        ));
    }
}
