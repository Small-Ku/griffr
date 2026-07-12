use clap::Subcommand;
use griffr_common::api::protocol::{DEFAULT_LANGUAGE, DEFAULT_PLATFORM};
use griffr_common::config::{ChannelId, GameId};
use tracing::debug;

use crate::cli::{
    ApiTargetOverrideArgs, InstallProfileOverrideArgs, OutputFormat, PathArg,
    RequiredGameChannelArgs, SnapshotHashScope, VfsDiffAgainst,
};

#[derive(Subcommand)]
pub(crate) enum DebugCommands {
    /// Detect known game/channel/version from encrypted config.ini
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
        remote: RequiredGameChannelArgs,

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
        remote: RequiredGameChannelArgs,

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
        remote: RequiredGameChannelArgs,

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
        remote: RequiredGameChannelArgs,

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
        remote: RequiredGameChannelArgs,

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
        remote: RequiredGameChannelArgs,

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
        remote: RequiredGameChannelArgs,

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
pub(crate) enum PredownloadCommands {
    /// Check whether a predownload payload is available
    Check {
        #[command(flatten)]
        path: PathArg,
    },
    /// Download and verify staged predownload archives without applying them
    Fetch {
        #[command(flatten)]
        path: PathArg,

        /// Override the staging directory for downloaded predownload archives
        #[arg(long)]
        output_dir: Option<std::path::PathBuf>,
    },
    /// Apply the live release update using staged predownload archives when possible
    Apply {
        #[command(flatten)]
        path: PathArg,

        #[command(flatten)]
        overrides: InstallProfileOverrideArgs,

        /// Override the staging directory used for staged predownload archives
        #[arg(long)]
        output_dir: Option<std::path::PathBuf>,

        /// Skip post-update verification
        #[arg(long)]
        skip_verify: bool,

        /// Skip VFS resource download
        #[arg(long)]
        skip_vfs: bool,

        /// Keep archive files after successful extraction
        #[arg(long)]
        keep_pack_archives: bool,
    },
    /// Resume a previously extracted local patch state from `patch.json` + `vfs_files`
    Resume {
        #[command(flatten)]
        path: PathArg,
    },
}

#[derive(Subcommand)]
pub(crate) enum AccountCommands {
    /// Capture current local account state into a directory bundle
    Capture {
        /// Known game id
        game: GameId,

        /// Optional channel hint to narrow default sdk_data discovery roots
        #[arg(long)]
        channel_hint: Option<ChannelId>,

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
        /// Known game id
        game: GameId,

        /// Optional channel hint to narrow default sdk_data discovery roots
        #[arg(long)]
        channel_hint: Option<ChannelId>,

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

/// Global options shared across all commands
#[derive(Debug, Clone, Copy)]
pub struct GlobalOptions {
    pub dry_run: bool,
    pub verbose: bool,
    pub skip_verify: bool,
    pub force_full_package: bool,
    pub skip_vfs: bool,
    pub keep_pack_archives: bool,
    pub extraction_progress_buffer_bytes: usize,
    pub download_progress_buffer_bytes: usize,
    pub output: OutputFormat,
}

impl GlobalOptions {
    /// Print a message if verbose mode is enabled
    pub fn verbose(&self, msg: impl AsRef<str>) {
        if self.verbose {
            debug!("{}", msg.as_ref());
        }
    }

    /// Print a dry run message
    pub fn dry_run(&self, msg: impl AsRef<str>) {
        if self.dry_run {
            crate::ui::print_info(format!("DRY RUN: {}", msg.as_ref()));
        }
    }

    /// Check if we should skip actual execution
    pub fn is_dry_run(&self) -> bool {
        self.dry_run
    }
}
