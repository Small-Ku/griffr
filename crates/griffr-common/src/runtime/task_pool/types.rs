use crate::api::protocol::MIN_USER_AGENT;
use crate::runtime::{PatchApplyOptions, PatchPreflightReport, ProgressLane, ProgressSender};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const DEFAULT_PARALLELISM_FALLBACK: usize = 4;
const DEFAULT_MAX_RETRIES: u32 = 3;
pub const DEFAULT_PROGRESS_BUFFER_BYTES: usize = 256 * 1024;

const MIN_IO_SLOTS: usize = 2;
const MAX_IO_SLOTS: usize = 16;
const MIN_CPU_SLOTS: usize = 1;
const MAX_CPU_SLOTS: usize = 16;
const MIN_EXTRACT_SLOTS: usize = 1;
const MAX_EXTRACT_SLOTS: usize = 4;
const MIN_FILE_ENSURE_IO_SLOTS: usize = 4;
const MAX_FILE_ENSURE_IO_SLOTS: usize = 24;
const DEFAULT_VFS_IO_SLOTS: usize = 6;
const DEFAULT_ARCHIVE_IO_SLOTS: usize = 6;
const MIN_PATCH_SLOTS: usize = 1;
const MAX_PATCH_SLOTS: usize = 4;

use crate::runtime::issues::FileIssue;

/// Selects the download throttle. Local verification and reuse never use the
/// VFS CDN queue, even when a later fallback download is VFS-classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferClass {
    General,
    Vfs,
}

#[derive(Debug, Clone)]
pub struct FileEnsureTask {
    pub dest: PathBuf,
    pub logical_path: String,
    pub expected_md5: String,
    pub expected_size: u64,
    pub source_candidates: Vec<PathBuf>,
    pub download_url: Option<String>,
    pub allow_copy_fallback: bool,
    pub prefer_reuse: bool,
    pub retry_count: u32,
    pub transfer_class: TransferClass,
}

#[derive(Debug)]
pub(crate) struct ArchiveInstallGroup {
    remaining: AtomicUsize,
    failed: AtomicBool,
    continuation: Mutex<Option<Task>>,
}

impl ArchiveInstallGroup {
    pub(crate) fn new(part_count: usize, continuation: Task) -> Arc<Self> {
        Arc::new(Self {
            remaining: AtomicUsize::new(part_count),
            failed: AtomicBool::new(false),
            continuation: Mutex::new(Some(continuation)),
        })
    }

    pub(crate) fn finish_part(&self, succeeded: bool, spawned: &mut Vec<Task>) {
        if !succeeded {
            self.failed.store(true, Ordering::Release);
        }
        if self.remaining.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }
        if self.failed.load(Ordering::Acquire) {
            self.continuation.lock().unwrap().take();
            return;
        }
        if let Some(task) = self.continuation.lock().unwrap().take() {
            spawned.push(task);
        }
    }
}

#[derive(Debug, Clone)]
pub enum Task {
    InstallArchive {
        base_name: String,
        dest: PathBuf,
        cleanup: bool,
        password: Option<String>,
        patch_options: PatchApplyOptions,
        parts: Vec<ArchivePart>,
    },
    InstallArchivePart {
        part: ArchivePart,
        group: Arc<ArchiveInstallGroup>,
    },
    Download {
        url: String,
        dest: PathBuf,
        logical_path: String,
        expected_md5: String,
        expected_size: Option<u64>,
        retry_count: u32,
        transfer_class: TransferClass,
    },
    Verify {
        path: PathBuf,
        logical_path: String,
        expected_md5: String,
        expected_size: Option<u64>,
        on_fail: Option<Box<Task>>,
    },
    RepairFile {
        dest: PathBuf,
        logical_path: String,
        expected_md5: String,
        expected_size: u64,
        source_candidates: Vec<PathBuf>,
        download_url: Option<String>,
        allow_copy_fallback: bool,
        retry_count: u32,
        transfer_class: TransferClass,
    },
    ReuseFile {
        source: PathBuf,
        remaining_source_candidates: Vec<PathBuf>,
        dest: PathBuf,
        logical_path: String,
        expected_md5: String,
        expected_size: u64,
        download_url: Option<String>,
        allow_copy_fallback: bool,
        retry_count: u32,
        transfer_class: TransferClass,
    },
    Extract {
        base_name: String,
        volumes: Vec<PathBuf>,
        dest: PathBuf,
        cleanup: bool,
        password: Option<String>,
        patch_options: PatchApplyOptions,
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

impl Task {
    /// Builds a CPU-first verify/repair graph. Only explicit relink mode skips
    /// destination verification because relinking is itself the requested work.
    pub fn ensure_file(spec: FileEnsureTask) -> Self {
        let repair = Self::RepairFile {
            dest: spec.dest.clone(),
            logical_path: spec.logical_path.clone(),
            expected_md5: spec.expected_md5.clone(),
            expected_size: spec.expected_size,
            source_candidates: spec.source_candidates,
            download_url: spec.download_url,
            allow_copy_fallback: spec.allow_copy_fallback,
            retry_count: spec.retry_count,
            transfer_class: spec.transfer_class,
        };
        if spec.prefer_reuse {
            repair
        } else {
            Self::Verify {
                path: spec.dest,
                logical_path: spec.logical_path,
                expected_md5: spec.expected_md5,
                expected_size: Some(spec.expected_size),
                on_fail: Some(Box::new(repair)),
            }
        }
    }
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
    pub vfs_io_slots: usize,
    pub archive_io_slots: usize,
    pub patch_slots: usize,
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

    pub fn for_file_ensure() -> Self {
        Self {
            io_slots: available_parallelism()
                .clamp(MIN_FILE_ENSURE_IO_SLOTS, MAX_FILE_ENSURE_IO_SLOTS),
            ..Self::default()
        }
    }
}

impl Default for TaskPoolConfig {
    fn default() -> Self {
        let cpus = available_parallelism();
        Self {
            io_slots: (cpus * 2).clamp(MIN_IO_SLOTS, MAX_IO_SLOTS),
            vfs_io_slots: DEFAULT_VFS_IO_SLOTS,
            archive_io_slots: DEFAULT_ARCHIVE_IO_SLOTS,
            patch_slots: (cpus / 4).clamp(MIN_PATCH_SLOTS, MAX_PATCH_SLOTS),
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
mod tests;
