use super::*;
use griffr_common::api::client::ApiClient;
use griffr_common::api::types::{GetLatestGameResponse, PackFile, PackageInfo, PatchInfo};
use griffr_common::config::{ChannelPair, GameId};
use griffr_common::runtime::task_pool::{
    TaskPoolConfig, TaskPoolRunner, DEFAULT_PROGRESS_BUFFER_BYTES,
};
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
        skip_vfs: true,
        keep_pack_archives: false,
        extraction_progress_buffer_bytes: DEFAULT_PROGRESS_BUFFER_BYTES,
        download_progress_buffer_bytes: DEFAULT_PROGRESS_BUFFER_BYTES,
        output: OutputFormat::Text,
    }
}
mod archive_pipeline;
mod live_api;
mod package_selection;
