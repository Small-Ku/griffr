use crate::api::protocol::MIN_USER_AGENT;

const DEFAULT_PARALLELISM_FALLBACK: usize = 4;
const DEFAULT_MAX_RETRIES: u32 = 3;
const MIN_DISPATCHER_THREADS: usize = 2;
const MAX_DISPATCHER_THREADS: usize = 4;
pub const DEFAULT_PROGRESS_BUFFER_BYTES: usize = 256 * 1024;

const MIN_IO_SLOTS: usize = 2;
const MAX_IO_SLOTS: usize = 16;
const MIN_CPU_SLOTS: usize = 1;
const MAX_CPU_SLOTS: usize = 16;
const MIN_EXTRACT_SLOTS: usize = 1;
const MAX_EXTRACT_SLOTS: usize = 4;
const MIN_EXTRACT_SHARDS: usize = 1;
const MAX_EXTRACT_SHARDS: usize = 4;
const MIN_COMMIT_SLOTS: usize = 1;
const MAX_COMMIT_SLOTS: usize = 8;
const MIN_FILE_ENSURE_IO_SLOTS: usize = 4;
const MAX_FILE_ENSURE_IO_SLOTS: usize = 24;
const DEFAULT_VFS_IO_SLOTS: usize = 6;
const DEFAULT_ARCHIVE_IO_SLOTS: usize = 6;
const MIN_PATCH_SLOTS: usize = 1;
const MAX_PATCH_SLOTS: usize = 4;

#[derive(Debug, Clone)]
pub struct TaskPoolConfig {
    pub dispatcher_threads: usize,
    pub io_slots: usize,
    pub vfs_io_slots: usize,
    pub archive_io_slots: usize,
    pub patch_slots: usize,
    pub cpu_slots: usize,
    pub extract_slots: usize,
    pub extract_shards: usize,
    pub commit_slots: usize,
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
            dispatcher_threads: cpus.clamp(MIN_DISPATCHER_THREADS, MAX_DISPATCHER_THREADS),
            io_slots: (cpus * 2).clamp(MIN_IO_SLOTS, MAX_IO_SLOTS),
            vfs_io_slots: DEFAULT_VFS_IO_SLOTS,
            archive_io_slots: DEFAULT_ARCHIVE_IO_SLOTS,
            patch_slots: (cpus / 4).clamp(MIN_PATCH_SLOTS, MAX_PATCH_SLOTS),
            cpu_slots: cpus.clamp(MIN_CPU_SLOTS, MAX_CPU_SLOTS),
            extract_slots: (cpus / 2).clamp(MIN_EXTRACT_SLOTS, MAX_EXTRACT_SLOTS),
            extract_shards: (cpus / 4).clamp(MIN_EXTRACT_SHARDS, MAX_EXTRACT_SHARDS),
            commit_slots: cpus.clamp(MIN_COMMIT_SLOTS, MAX_COMMIT_SLOTS),
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
