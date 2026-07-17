use std::collections::BTreeMap;
use std::path::Path;

use crate::api::protocol::MIN_USER_AGENT;

const DEFAULT_PARALLELISM_FALLBACK: usize = 4;
const DEFAULT_MAX_RETRIES: u32 = 3;
const MIN_DISPATCHER_THREADS: usize = 2;
const MAX_DISPATCHER_THREADS: usize = 4;
const MIN_NETWORK_SLOTS: usize = 4;
const MAX_NETWORK_SLOTS: usize = 12;
const MIN_CPU_WORKERS: usize = 1;
const MAX_CPU_WORKERS: usize = 12;
const MIN_BLOCKING_WORKERS: usize = 2;
const MAX_BLOCKING_WORKERS: usize = 6;
const MIN_EXTRACT_SLOTS: usize = 1;
const MAX_EXTRACT_SLOTS: usize = 2;
const MIN_EXTRACT_SHARDS: usize = 1;
const MAX_EXTRACT_SHARDS: usize = 4;

pub const DEFAULT_PROGRESS_BUFFER_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeConcurrency {
    pub read_slots: usize,
    pub write_slots: usize,
}

impl VolumeConcurrency {
    pub fn new(read_slots: usize, write_slots: usize) -> Self {
        Self {
            read_slots: read_slots.max(1),
            write_slots: write_slots.max(1),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TaskPoolConfig {
    /// Small compio executor dedicated to native async I/O completions.
    pub dispatcher_threads: usize,
    /// Shared network capacity across general, archive, and VFS transfers.
    pub network_slots: usize,
    /// Fixed CPU worker pool for hashing and preparation work.
    pub cpu_workers: usize,
    /// Fixed blocking worker pool for filesystem mutations and orchestration.
    pub blocking_workers: usize,
    /// Maximum concurrent archive extraction transactions.
    pub extract_slots: usize,
    /// Default physical-volume policy. One reader and one writer is deliberately
    /// conservative for rotational media and unknown devices.
    pub default_volume_concurrency: VolumeConcurrency,
    /// Overrides keyed by the stable physical-volume identity resolved from a path.
    pub volume_concurrency: BTreeMap<String, VolumeConcurrency>,
    pub extract_shards: usize,
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
            extract_slots: extract_slots.clamp(MIN_EXTRACT_SLOTS, MAX_EXTRACT_SLOTS),
            ..Self::default()
        }
    }

    pub fn with_volume_limit(
        mut self,
        path: impl AsRef<Path>,
        read_slots: usize,
        write_slots: usize,
    ) -> Self {
        let key = super::super::fs_ops::storage_volume_group_key(path.as_ref());
        self.volume_concurrency
            .insert(key, VolumeConcurrency::new(read_slots, write_slots));
        self
    }

    pub(crate) fn volume_limit(&self, volume: &str) -> VolumeConcurrency {
        self.volume_concurrency
            .get(volume)
            .copied()
            .unwrap_or(self.default_volume_concurrency)
    }

    pub fn for_file_reuse() -> Self {
        Self {
            network_slots: available_parallelism().clamp(MIN_NETWORK_SLOTS, MAX_NETWORK_SLOTS),
            ..Self::default()
        }
    }

    pub fn for_file_ensure() -> Self {
        Self {
            network_slots: (available_parallelism() * 2)
                .clamp(MIN_NETWORK_SLOTS, MAX_NETWORK_SLOTS),
            ..Self::default()
        }
    }
}

impl Default for TaskPoolConfig {
    fn default() -> Self {
        let cpus = available_parallelism();
        Self {
            dispatcher_threads: cpus.clamp(MIN_DISPATCHER_THREADS, MAX_DISPATCHER_THREADS),
            network_slots: (cpus * 2).clamp(MIN_NETWORK_SLOTS, MAX_NETWORK_SLOTS),
            cpu_workers: cpus.saturating_sub(1).clamp(MIN_CPU_WORKERS, MAX_CPU_WORKERS),
            blocking_workers: (cpus / 2).clamp(MIN_BLOCKING_WORKERS, MAX_BLOCKING_WORKERS),
            extract_slots: (cpus / 4).clamp(MIN_EXTRACT_SLOTS, MAX_EXTRACT_SLOTS),
            default_volume_concurrency: VolumeConcurrency::new(1, 1),
            volume_concurrency: BTreeMap::new(),
            extract_shards: (cpus / 2).clamp(MIN_EXTRACT_SHARDS, MAX_EXTRACT_SHARDS),
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
