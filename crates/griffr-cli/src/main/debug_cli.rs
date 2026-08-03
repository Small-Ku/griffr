use clap::Subcommand;
use griffr_common::api::protocol::{DEFAULT_LANGUAGE, DEFAULT_PLATFORM};
use griffr_common::config::{GameId, RegionId};
use tracing::debug;

use crate::cli::{
    ApiTargetOverrideArgs, InstallTargetOverrideArgs, MutationArgs, OutputFormat, PathArg,
    RequiredGameRegionChannelArgs, SnapshotHashScope, VfsDiffAgainst,
};

#[derive(Subcommand)]
pub(crate) enum DebugCommands {
    /// Detect known game/region/channel/version from encrypted config.ini
    DetectConfigIni {
        #[arg(long)]
        path: std::path::PathBuf,
    },
    /// Print decrypted config.ini contents
    DecryptConfigIni {
        #[arg(long)]
        path: std::path::PathBuf,
    },
    /// Print decrypted local game_files contents
    DecryptGameFiles {
        #[arg(long)]
        path: std::path::PathBuf,
    },
    /// Decrypt local encrypted resource index/pref JSON files
    DecryptResIndex {
        /// Path to an encrypted .json file or a directory containing .json files
        #[arg(long)]
        path: std::path::PathBuf,

        /// Optional key override (defaults to built-in Endfield RES_INDEX_KEY)
        #[arg(long)]
        key: Option<String>,
    },
    /// Compare on-disk VFS files against local index/pref manifests
    VfsDiff {
        /// Path to a root containing `VFS/` and `index_*.json` / `pref_*.json` files
        #[arg(long)]
        path: std::path::PathBuf,

        /// Comparison mode: `persistent` uses pref-first policy; `streamingassets` uses index-full
        #[arg(long, value_enum, default_value_t = VfsDiffAgainst::Persistent)]
        against: VfsDiffAgainst,

        /// Optional key override (defaults to built-in Endfield RES_INDEX_KEY)
        #[arg(long)]
        key: Option<String>,

        /// Max entries printed for missing/extra lists
        #[arg(long, default_value_t = 20)]
        show_limit: usize,
    },
    /// Capture local resource state snapshot for Persistent and StreamingAssets
    SnapshotResourceState {
        /// Install root, Endfield_Data root, or direct path containing Persistent/StreamingAssets
        #[arg(long)]
        path: std::path::PathBuf,

        /// Optional output file path for snapshot JSON payload
        #[arg(long = "output-file", id = "snapshot_resource_state_output")]
        output: Option<std::path::PathBuf>,

        /// Hash check scope: none, persistent-only, or both persistent+streamingassets
        #[arg(long, value_enum, default_value_t = SnapshotHashScope::Persistent)]
        hash_check: SnapshotHashScope,
    },
    /// Compare two resource state snapshots and summarize differences
    DiffResourceSnapshots {
        /// Baseline snapshot file
        #[arg(long)]
        before: std::path::PathBuf,

        /// Newer snapshot file
        #[arg(long)]
        after: std::path::PathBuf,

        /// Max entries printed for changed lists
        #[arg(long, default_value_t = 20)]
        show_limit: usize,
    },
    /// Call get_latest_game and print raw response JSON
    GetRawLatestGame {
        #[command(flatten)]
        remote: RequiredGameRegionChannelArgs,

        #[command(flatten)]
        overrides: ApiTargetOverrideArgs,

        /// Version passed to get_latest_game (defaults to latest when omitted)
        #[arg(long)]
        version: Option<String>,

        /// Optional output file path for JSON payload
        #[arg(long = "output-file", id = "api_get_latest_game_output")]
        output: Option<std::path::PathBuf>,
    },
    /// Call get_latest_resources and print raw response JSON
    GetRawLatestResources {
        #[command(flatten)]
        remote: RequiredGameRegionChannelArgs,

        #[command(flatten)]
        overrides: ApiTargetOverrideArgs,

        /// Version passed to get_latest_game for version/rand resolution (defaults to latest when omitted)
        #[arg(long)]
        version: Option<String>,

        /// Full version used for get_latest_resources (defaults to resolved latest version)
        #[arg(long = "resource-version")]
        resource_version: Option<String>,

        /// rand_str for get_latest_resources (defaults to resolved latest rand_str)
        #[arg(long = "rand-str")]
        rand_str: Option<String>,

        /// Platform for get_latest_resources
        #[arg(long, default_value = DEFAULT_PLATFORM)]
        platform: String,

        #[arg(long = "output-file", id = "api_get_latest_resources_output")]
        output: Option<std::path::PathBuf>,
    },
    /// Fetch and print the remote game_files manifest
    ListGameFiles {
        #[command(flatten)]
        remote: RequiredGameRegionChannelArgs,

        #[command(flatten)]
        overrides: ApiTargetOverrideArgs,

        /// Version passed to get_latest_game for manifest resolution (defaults to latest when omitted)
        #[arg(long)]
        version: Option<String>,

        /// Optional output file path for newline-delimited JSON entries
        #[arg(long = "output-file", id = "api_get_game_files_output")]
        output: Option<std::path::PathBuf>,
    },
    /// List files from latest resource indexes (index_main/index_initial)
    ListResourceFiles {
        #[command(flatten)]
        remote: RequiredGameRegionChannelArgs,

        #[command(flatten)]
        overrides: ApiTargetOverrideArgs,

        /// Version passed to get_latest_game for version/rand resolution (defaults to latest when omitted)
        #[arg(long)]
        version: Option<String>,

        /// Full version used for get_latest_resources (defaults to resolved latest version)
        #[arg(long = "resource-version")]
        resource_version: Option<String>,

        /// rand_str for get_latest_resources (defaults to resolved latest rand_str)
        #[arg(long = "rand-str")]
        rand_str: Option<String>,

        /// Platform for get_latest_resources
        #[arg(long, default_value = DEFAULT_PLATFORM)]
        platform: String,

        #[arg(long = "output-file", id = "list_resource_files_output")]
        output: Option<std::path::PathBuf>,
    },
    /// Fetch one file referenced by the latest remote game_files manifest
    GetFile {
        #[command(flatten)]
        remote: RequiredGameRegionChannelArgs,

        #[command(flatten)]
        overrides: ApiTargetOverrideArgs,

        /// Version passed to get_latest_game for manifest resolution (defaults to latest when omitted)
        #[arg(long)]
        version: Option<String>,

        #[arg(long)]
        file: String,

        /// Output file path for the downloaded remote file
        #[arg(long = "output-file", id = "api_get_file_output")]
        output: std::path::PathBuf,
    },
    /// Fetch raw media/news payload as JSON
    GetRawMedia {
        #[command(flatten)]
        remote: RequiredGameRegionChannelArgs,

        #[command(flatten)]
        overrides: ApiTargetOverrideArgs,

        /// Launcher language
        #[arg(long, default_value = DEFAULT_LANGUAGE)]
        language: String,

        /// Optional output file path for JSON payload
        #[arg(long = "output-file", id = "api_get_media_output")]
        output: Option<std::path::PathBuf>,
    },
    /// Fetch normalized media/news payload as JSON
    GetMedia {
        #[command(flatten)]
        remote: RequiredGameRegionChannelArgs,

        #[command(flatten)]
        overrides: ApiTargetOverrideArgs,

        /// Launcher language
        #[arg(long, default_value = DEFAULT_LANGUAGE)]
        language: String,

        /// Optional output file path for JSON payload
        #[arg(long = "output-file", id = "fetch_media_output")]
        output: Option<std::path::PathBuf>,
    },
}

#[derive(Subcommand)]
pub(crate) enum StageCommands {
    /// Inspect whether a staged update payload is available
    #[command(alias = "check")]
    Inspect {
        #[command(flatten)]
        path: PathArg,
    },
    /// Download and verify staged predownload archives without applying them
    Fetch {
        #[command(flatten)]
        mutation: MutationArgs,

        #[command(flatten)]
        path: PathArg,

        /// Directory for downloaded staged archives
        #[arg(long = "stage-dir", alias = "output-dir")]
        stage_dir: Option<std::path::PathBuf>,
    },
    /// Legacy alias for `update --stage-dir ... --require-staged`
    #[command(hide = true)]
    Apply {
        #[command(flatten)]
        mutation: MutationArgs,

        #[command(flatten)]
        path: PathArg,

        #[command(flatten)]
        overrides: InstallTargetOverrideArgs,

        /// Directory containing staged archives
        #[arg(long = "stage-dir", alias = "output-dir")]
        stage_dir: Option<std::path::PathBuf>,

        /// Commit content but defer final verification
        #[arg(long = "defer-verification", alias = "skip-verify")]
        defer_verification: bool,

        /// Choose how launcher resource-index files are sourced
        #[arg(long, value_enum, conflicts_with = "skip_vfs")]
        resource_policy: Option<crate::ResourcePolicyArg>,

        /// Skip launcher resource-index sync. Package and game_files resource entries are still installed and verified.
        #[arg(long, conflicts_with = "resource_policy")]
        skip_vfs: bool,

        /// Keep archive files after successful extraction
        #[arg(long)]
        keep_pack_archives: bool,

        /// Put extraction staging and patch temporary files under this directory
        #[arg(long)]
        work_dir: Option<std::path::PathBuf>,

        /// Persist the patch-managed asset tree under this directory and link it into the install root
        #[arg(long)]
        external_asset_root: Option<std::path::PathBuf>,
    },
    /// Legacy alias for the top-level recover command
    #[command(hide = true)]
    Resume {
        #[command(flatten)]
        mutation: MutationArgs,

        #[command(flatten)]
        path: PathArg,
    },
}

#[derive(Subcommand)]
pub(crate) enum AccountCommands {
    /// Capture current local account state into a directory bundle
    Capture {
        #[command(flatten)]
        mutation: MutationArgs,

        /// Known game id
        game: GameId,

        /// Optional launcher region hint to narrow default sdk_data discovery roots
        #[arg(long)]
        region_hint: Option<RegionId>,

        /// Output bundle directory
        #[arg(long = "to")]
        bundle: std::path::PathBuf,

        /// Explicit sdk_data_* directory (defaults to latest under LocalLow)
        #[arg(long)]
        sdk_dir: Option<std::path::PathBuf>,

        /// Install root path for optional install-local mmkv capture
        #[arg(long)]
        install_path: Option<std::path::PathBuf>,

        /// Include optional install-local mmkv directory in the bundle
        #[arg(long, requires = "install_path")]
        include_install_mmkv: bool,

        /// Replace bundle destination if it already exists
        #[arg(long)]
        force: bool,
    },

    /// Activate account state from a directory bundle
    Activate {
        #[command(flatten)]
        mutation: MutationArgs,

        /// Known game id
        game: GameId,

        /// Optional launcher region hint to narrow default sdk_data discovery roots
        #[arg(long)]
        region_hint: Option<RegionId>,

        /// Input bundle directory
        #[arg(long = "from")]
        bundle: std::path::PathBuf,

        /// Explicit sdk_data_* target directory (defaults to latest under LocalLow)
        #[arg(long)]
        sdk_dir: Option<std::path::PathBuf>,

        /// Install root path for optional install-local mmkv restore
        #[arg(long)]
        install_path: Option<std::path::PathBuf>,

        /// Restore optional install-local mmkv directory from the bundle
        #[arg(long, requires = "install_path")]
        include_install_mmkv: bool,

        /// Replace target directories if they already exist
        #[arg(long)]
        force: bool,
    },
}

/// Runtime options shared across command implementations. User-facing I/O
/// tuning is configured through `GRIFFR_*` environment variables so normal
/// command help stays focused on operation semantics.
#[derive(Debug, Clone, Copy)]
pub struct GlobalOptions {
    pub dry_run: bool,
    pub verbose: bool,
    pub skip_verify: bool,
    pub force_full_package: bool,
    pub resource_policy: crate::ResourcePolicyArg,
    pub keep_pack_archives: bool,
    pub extraction_progress_buffer_bytes: usize,
    pub download_progress_buffer_bytes: usize,
    pub volume_read_limit: usize,
    pub volume_write_limit: usize,
    pub volume_metadata_limit: usize,
    pub volume_streaming_pressure_limit: usize,
    pub volume_streaming_mode: griffr_common::runtime::task_pool::VolumeStreamingMode,
    pub reuse_queue_limit: usize,
    pub output: OutputFormat,
}

impl GlobalOptions {
    pub fn from_environment(dry_run: bool, verbose: bool, output: OutputFormat) -> Self {
        use griffr_common::runtime::task_pool::{
            VolumeStreamingMode, DEFAULT_PROGRESS_BUFFER_BYTES, DEFAULT_REUSE_QUEUE_LIMIT,
            DEFAULT_VOLUME_METADATA_LIMIT, DEFAULT_VOLUME_READ_LIMIT,
            DEFAULT_VOLUME_STREAMING_MODE, DEFAULT_VOLUME_STREAMING_PRESSURE_LIMIT,
            DEFAULT_VOLUME_WRITE_LIMIT,
        };

        Self {
            dry_run,
            verbose,
            skip_verify: false,
            force_full_package: false,
            resource_policy: crate::ResourcePolicyArg::Auto,
            keep_pack_archives: false,
            extraction_progress_buffer_bytes: env_usize(
                "GRIFFR_EXTRACTION_PROGRESS_BUFFER_BYTES",
                DEFAULT_PROGRESS_BUFFER_BYTES,
            ),
            download_progress_buffer_bytes: env_usize(
                "GRIFFR_DOWNLOAD_PROGRESS_BUFFER_BYTES",
                DEFAULT_PROGRESS_BUFFER_BYTES,
            ),
            volume_read_limit: env_positive_usize(
                "GRIFFR_VOLUME_READ_LIMIT",
                DEFAULT_VOLUME_READ_LIMIT,
            ),
            volume_write_limit: env_positive_usize(
                "GRIFFR_VOLUME_WRITE_LIMIT",
                DEFAULT_VOLUME_WRITE_LIMIT,
            ),
            volume_metadata_limit: env_positive_usize(
                "GRIFFR_VOLUME_METADATA_LIMIT",
                DEFAULT_VOLUME_METADATA_LIMIT,
            ),
            volume_streaming_pressure_limit: env_positive_usize(
                "GRIFFR_VOLUME_STREAMING_PRESSURE_LIMIT",
                DEFAULT_VOLUME_STREAMING_PRESSURE_LIMIT,
            ),
            volume_streaming_mode: match std::env::var("GRIFFR_VOLUME_STREAMING_MODE") {
                Ok(value) if value.eq_ignore_ascii_case("exclusive") => {
                    VolumeStreamingMode::Exclusive
                }
                Ok(value) if value.eq_ignore_ascii_case("mixed") => VolumeStreamingMode::Mixed,
                Ok(value) => {
                    tracing::warn!(
                        "Ignoring invalid GRIFFR_VOLUME_STREAMING_MODE={value:?}; expected exclusive or mixed"
                    );
                    DEFAULT_VOLUME_STREAMING_MODE
                }
                Err(_) => DEFAULT_VOLUME_STREAMING_MODE,
            },
            reuse_queue_limit: env_positive_usize(
                "GRIFFR_REUSE_QUEUE_LIMIT",
                DEFAULT_REUSE_QUEUE_LIMIT,
            ),
            output,
        }
    }

    pub fn with_output(self, output: OutputFormat) -> Self {
        Self { output, ..self }
    }

    pub fn with_dry_run(self, dry_run: bool) -> Self {
        Self { dry_run, ..self }
    }

    pub fn task_pool_config(&self) -> griffr_common::runtime::task_pool::TaskPoolConfig {
        self.task_pool_config_for_batch(1)
    }

    pub fn task_pool_config_for_batch(
        &self,
        target_jobs: usize,
    ) -> griffr_common::runtime::task_pool::TaskPoolConfig {
        use griffr_common::runtime::task_pool::{TaskPoolConfig, VolumeIoPolicy};

        let target_jobs = target_jobs.max(1);
        let share = |value: usize| value.div_ceil(target_jobs).max(1);
        let mut config = TaskPoolConfig::with_progress_buffers(
            self.extraction_progress_buffer_bytes,
            self.download_progress_buffer_bytes,
        );
        config.dispatcher_threads = share(config.dispatcher_threads);
        config.network_slots = share(config.network_slots);
        config.cpu_slots = share(config.cpu_slots);
        config.blocking_slots = share(config.blocking_slots);
        config.blocking_pool_limit = share(config.blocking_pool_limit).max(2);
        config.extract_slots = share(config.extract_slots);
        config.extract_shards = share(config.extract_shards);
        config.default_volume_policy = VolumeIoPolicy::new(
            share(self.volume_read_limit),
            share(self.volume_write_limit),
            share(self.volume_metadata_limit),
            share(self.volume_streaming_pressure_limit),
            self.volume_streaming_mode,
        );
        config.reuse_queue_limit = share(self.reuse_queue_limit);
        config
    }

    /// Print a message if verbose mode is enabled.
    pub fn verbose(&self, msg: impl AsRef<str>) {
        if self.verbose {
            debug!("{}", msg.as_ref());
        }
    }

    pub fn dry_run(&self, msg: impl AsRef<str>) {
        if self.dry_run {
            crate::ui::print_info(format!("DRY RUN: {}", msg.as_ref()));
        }
    }

    pub fn is_dry_run(&self) -> bool {
        self.dry_run
    }
}

fn env_usize(name: &str, default: usize) -> usize {
    match std::env::var(name) {
        Ok(value) => match value.parse::<usize>() {
            Ok(value) => value,
            Err(_) => {
                tracing::warn!(
                    "Ignoring invalid {name}={value:?}; expected a non-negative integer"
                );
                default
            }
        },
        Err(_) => default,
    }
}

fn env_positive_usize(name: &str, default: usize) -> usize {
    let value = env_usize(name, default);
    if value == 0 {
        tracing::warn!("Ignoring {name}=0; expected an integer greater than zero");
        default
    } else {
        value
    }
}
