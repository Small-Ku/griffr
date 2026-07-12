use crate::api::protocol::MIN_USER_AGENT;
use std::path::PathBuf;

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

impl Default for TaskPoolConfig {
    fn default() -> Self {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        Self {
            io_slots: (cpus * 2).clamp(2, 16),
            cpu_slots: cpus.clamp(1, 16),
            extract_slots: (cpus / 2).clamp(1, 4),
            max_retries: 3,
            user_agent: MIN_USER_AGENT.to_owned(),
            extraction_progress_buffer_bytes: 256 * 1024,
            download_progress_buffer_bytes: 256 * 1024,
        }
    }
}

#[derive(Debug)]
pub struct TaskPoolResult {
    pub events: Vec<ProgressEvent>,
}

pub struct TaskPoolRunner {
    pub(crate) ctx: crate::runtime::task_pool::scheduler::WorkerContext,
    pub(crate) event_rx: flume::Receiver<ProgressEvent>,
}
