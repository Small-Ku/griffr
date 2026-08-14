use super::*;
use griffr_core::{ChannelPair, GameId};
use griffr_hypergryph_api::client::ApiClient;
use griffr_hypergryph_api::types::{GetLatestGameResponse, PackFile, PackageInfo, PatchInfo};
use griffr_runtime::task_pool::{TaskPoolConfig, TaskPoolRunner, DEFAULT_PROGRESS_BUFFER_BYTES};
use md5::Digest;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tempfile::tempdir;

use crate::{GlobalOptions, OutputFormat};
use zip::write::FileOptions;

fn test_global_options() -> GlobalOptions {
    GlobalOptions {
        dry_run: false,
        verbose: false,
        skip_verify: false,
        force_full_package: false,
        resource_policy: crate::ResourcePolicyArg::PackageOnly,
        keep_pack_archives: false,
        extraction_progress_buffer_bytes: DEFAULT_PROGRESS_BUFFER_BYTES,
        download_progress_buffer_bytes: DEFAULT_PROGRESS_BUFFER_BYTES,
        volume_read_limit: griffr_runtime::task_pool::DEFAULT_VOLUME_READ_LIMIT,
        volume_write_limit: griffr_runtime::task_pool::DEFAULT_VOLUME_WRITE_LIMIT,
        volume_metadata_limit: griffr_runtime::task_pool::DEFAULT_VOLUME_METADATA_LIMIT,
        volume_streaming_pressure_limit:
            griffr_runtime::task_pool::DEFAULT_VOLUME_STREAMING_PRESSURE_LIMIT,
        volume_streaming_mode: griffr_runtime::task_pool::DEFAULT_VOLUME_STREAMING_MODE,
        reuse_queue_limit: griffr_runtime::task_pool::DEFAULT_REUSE_QUEUE_LIMIT,
        output: OutputFormat::Text,
    }
}
mod archives;
mod live_api;
mod package_selection;

#[test]
fn global_options_apply_explicit_task_pool_volume_policy() {
    let mut opts = test_global_options();
    opts.volume_read_limit = 3;
    opts.volume_write_limit = 1;
    opts.volume_metadata_limit = 2;
    opts.volume_streaming_pressure_limit = 4;
    opts.volume_streaming_mode = griffr_runtime::task_pool::VolumeStreamingMode::Exclusive;
    opts.reuse_queue_limit = 24;

    let config = opts.task_pool_config();
    assert_eq!(
        config.default_volume_policy,
        griffr_runtime::task_pool::VolumeIoPolicy::new(
            3,
            1,
            2,
            4,
            griffr_runtime::task_pool::VolumeStreamingMode::Exclusive,
        )
    );
    assert_eq!(config.reuse_queue_limit, 24);
}
