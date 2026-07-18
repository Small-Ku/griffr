use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use crate::api::protocol::MIN_USER_AGENT;

const DEFAULT_PARALLELISM_FALLBACK: usize = 4;
const DEFAULT_MAX_RETRIES: u32 = 3;
const MIN_DISPATCHER_THREADS: usize = 2;
const MAX_DISPATCHER_THREADS: usize = 8;
const MIN_NETWORK_SLOTS: usize = 4;
const MAX_NETWORK_SLOTS: usize = 12;
const MIN_CPU_SLOTS: usize = 1;
const MAX_CPU_SLOTS: usize = 12;
const MIN_BLOCKING_SLOTS: usize = 2;
const MAX_BLOCKING_SLOTS: usize = 8;
const MIN_BLOCKING_POOL_LIMIT: usize = 8;
const MAX_BLOCKING_POOL_LIMIT: usize = 32;
pub(crate) const BLOCKING_POOL_INTERNAL_RESERVE: usize = 4;
const MIN_EXTRACT_SLOTS: usize = 1;
const MAX_EXTRACT_SLOTS: usize = 2;
const MIN_EXTRACT_SHARDS: usize = 1;
const MAX_EXTRACT_SHARDS: usize = 4;
const DEFAULT_VOLUME_WRITE_RESERVATION_DELAY: Duration = Duration::from_millis(15);

pub const DEFAULT_PROGRESS_BUFFER_BYTES: usize = 256 * 1024;
pub const DEFAULT_REUSE_PIPELINE_WINDOW: usize = 64;
pub const DEFAULT_VOLUME_READ_LIMIT: usize = 16;
pub const DEFAULT_VOLUME_WRITE_LIMIT: usize = 16;
pub const DEFAULT_VOLUME_METADATA_LIMIT: usize = 128;
pub const DEFAULT_VOLUME_STREAMING_PRESSURE_LIMIT: usize = 32;
pub const DEFAULT_VOLUME_STREAMING_MODE: VolumeStreamingMode = VolumeStreamingMode::Mixed;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeStreamingMode {
    /// Rotational or otherwise latency-sensitive media: one streaming direction
    /// or metadata mutation owns the volume at a time.
    Exclusive,
    /// Solid-state media: bounded readers and writers may run concurrently.
    Mixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeIoPolicy {
    pub read_limit: usize,
    pub write_limit: usize,
    pub metadata_limit: usize,
    /// Combined streaming pressure. A mixed-mode read consumes one unit and a
    /// mixed-mode write consumes one unit; a same-volume copy consumes both.
    pub streaming_pressure_limit: usize,
    pub streaming_mode: VolumeStreamingMode,
}

impl VolumeIoPolicy {
    pub const fn new(
        read_limit: usize,
        write_limit: usize,
        metadata_limit: usize,
        streaming_pressure_limit: usize,
        streaming_mode: VolumeStreamingMode,
    ) -> Self {
        assert!(read_limit > 0);
        assert!(write_limit > 0);
        assert!(metadata_limit > 0);
        assert!(streaming_pressure_limit > 0);
        Self {
            read_limit,
            write_limit,
            metadata_limit,
            streaming_pressure_limit,
            streaming_mode,
        }
    }
}

impl Default for VolumeIoPolicy {
    fn default() -> Self {
        Self::new(
            DEFAULT_VOLUME_READ_LIMIT,
            DEFAULT_VOLUME_WRITE_LIMIT,
            DEFAULT_VOLUME_METADATA_LIMIT,
            DEFAULT_VOLUME_STREAMING_PRESSURE_LIMIT,
            DEFAULT_VOLUME_STREAMING_MODE,
        )
    }
}

#[derive(Debug, Clone)]
pub struct TaskPoolConfig {
    /// Small compio executor dedicated to native async I/O completions.
    pub dispatcher_threads: usize,
    /// Shared network capacity across general, archive, and VFS transfers.
    pub network_slots: usize,
    /// Maximum CPU-heavy tasks admitted to Dispatcher::dispatch_blocking().
    pub cpu_slots: usize,
    /// Maximum non-CPU blocking tasks admitted to Dispatcher::dispatch_blocking().
    pub blocking_slots: usize,
    /// Shared compio AsyncifyPool limit. This includes Griffr blocking jobs and
    /// compio operations that need a blocking fallback, so it must retain headroom.
    pub blocking_pool_limit: usize,
    /// Maximum concurrent archive extraction transactions.
    pub extract_slots: usize,
    /// Default physical-volume admission policy. The defaults allow bounded
    /// mixed streaming reads/writes plus a separate metadata lane.
    pub default_volume_policy: VolumeIoPolicy,
    /// Overrides keyed by the stable physical-volume identity resolved from a path.
    pub volume_policies: BTreeMap<String, VolumeIoPolicy>,
    /// Maximum number of source files that may be verified while their reuse
    /// metadata commit is still queued or running.
    pub reuse_pipeline_window: usize,
    /// Once a streaming writer has waited this long, mixed-mode admission keeps
    /// one pressure unit available for it instead of stopping all new readers.
    pub volume_write_reservation_delay: Duration,
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

    pub fn with_volume_policy(mut self, path: impl AsRef<Path>, policy: VolumeIoPolicy) -> Self {
        let key = super::super::fs_ops::storage_volume_group_key(path.as_ref());
        self.volume_policies.insert(key, policy);
        self
    }

    pub(crate) fn volume_policy(&self, volume: &str) -> VolumeIoPolicy {
        self.volume_policies
            .get(volume)
            .copied()
            .unwrap_or(self.default_volume_policy)
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
        let cpu_slots = cpus.saturating_sub(1).clamp(MIN_CPU_SLOTS, MAX_CPU_SLOTS);
        let blocking_slots = (cpus / 2).clamp(MIN_BLOCKING_SLOTS, MAX_BLOCKING_SLOTS);
        let blocking_pool_limit = cpu_slots
            .saturating_add(blocking_slots)
            .saturating_add(BLOCKING_POOL_INTERNAL_RESERVE)
            .clamp(MIN_BLOCKING_POOL_LIMIT, MAX_BLOCKING_POOL_LIMIT);
        Self {
            dispatcher_threads: cpus.clamp(MIN_DISPATCHER_THREADS, MAX_DISPATCHER_THREADS),
            network_slots: (cpus * 2).clamp(MIN_NETWORK_SLOTS, MAX_NETWORK_SLOTS),
            cpu_slots,
            blocking_slots,
            blocking_pool_limit,
            extract_slots: (cpus / 4).clamp(MIN_EXTRACT_SLOTS, MAX_EXTRACT_SLOTS),
            default_volume_policy: VolumeIoPolicy::default(),
            volume_policies: BTreeMap::new(),
            reuse_pipeline_window: DEFAULT_REUSE_PIPELINE_WINDOW,
            volume_write_reservation_delay: DEFAULT_VOLUME_WRITE_RESERVATION_DELAY,
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
