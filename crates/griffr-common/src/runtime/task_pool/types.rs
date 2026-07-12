use crate::api::protocol::MIN_USER_AGENT;
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

use crate::runtime::issues::FileIssue;

#[derive(Debug, Clone)]
pub enum Task {
    InstallArchive {
        source_dir: PathBuf,
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
        source_dir: PathBuf,
        base_name: String,
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
    pub url: String,
    pub dest: PathBuf,
    pub logical_path: String,
    pub expected_md5: String,
    pub expected_size: u64,
}

#[derive(Debug, Clone)]
pub enum ProgressEvent {
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
    pub events: Vec<ProgressEvent>,
}

pub struct TaskPoolRunner {
    pub(crate) ctx: crate::runtime::task_pool::scheduler::WorkerContext,
    pub(crate) event_rx: flume::Receiver<ProgressEvent>,
}
