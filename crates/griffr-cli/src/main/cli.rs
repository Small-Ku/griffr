use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use griffr_common::api::protocol::DEFAULT_LANGUAGE;
use griffr_common::runtime::task_pool::DEFAULT_PROGRESS_BUFFER_BYTES;
use griffr_common::runtime::VfsBootstrapScope;

use crate::debug_cli::{AccountCommands, DebugCommands, PredownloadCommands};

/// Griffr - Hypergryph Game Launcher CLI
#[derive(Parser)]
#[command(name = "griffr")]
#[command(about = "A CLI launcher for Hypergryph games (Arknights / Endfield)")]
#[command(version)]
pub(crate) struct Cli {
    /// Perform a dry run without making changes
    #[arg(
        long,
        global = true,
        help = "Show what would be done without making changes"
    )]
    pub(crate) dry_run: bool,

    /// Enable verbose output
    #[arg(short, long, global = true, help = "Enable verbose logging")]
    pub(crate) verbose: bool,

    /// Output format for user-facing command results
    #[arg(
        long,
        global = true,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Choose text or JSON output for report-style commands"
    )]
    pub(crate) output: OutputFormat,

    /// Extraction progress buffer size in bytes (controls progress update granularity)
    #[arg(long, global = true, default_value_t = DEFAULT_PROGRESS_BUFFER_BYTES)]
    pub(crate) extraction_progress_buffer_bytes: usize,

    /// Download progress buffer size in bytes (controls progress update granularity)
    #[arg(long, global = true, default_value_t = DEFAULT_PROGRESS_BUFFER_BYTES)]
    pub(crate) download_progress_buffer_bytes: usize,

    #[command(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Args)]
pub(crate) struct PathArg {
    /// Install root or config.ini path
    #[arg(long)]
    pub(crate) path: std::path::PathBuf,
}

#[derive(Args)]
pub(crate) struct ReuseSourcesArg {
    /// Reuse matching files from other local install paths
    #[arg(long = "reuse-from")]
    pub(crate) reuse_from: Vec<std::path::PathBuf>,

    /// Allow copying reused files if hardlinks fail
    #[arg(long)]
    pub(crate) force_copy: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ApiTargetOverrideArgs {
    /// Override remote API gateway URL
    #[arg(long = "gateway")]
    pub gateway: Option<String>,

    /// Override game appcode
    #[arg(long = "game-appcode")]
    pub game_appcode: Option<String>,

    /// Override launcher appcode
    #[arg(long = "launcher-appcode")]
    pub launcher_appcode: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct InstallTargetOverrideArgs {
    #[command(flatten)]
    pub api: ApiTargetOverrideArgs,

    /// Override game executable filename (e.g. Arknights.exe)
    #[arg(long = "executable")]
    pub executable: Option<String>,

    /// Override game data-root directory name (e.g. Arknights_Data)
    #[arg(long = "data-root")]
    pub data_root: Option<String>,
}

impl From<ApiTargetOverrideArgs> for griffr_common::config::ApiTargetOverrides {
    fn from(args: ApiTargetOverrideArgs) -> Self {
        Self {
            gateway: args.gateway,
            game_appcode: args.game_appcode,
            launcher_appcode: args.launcher_appcode,
        }
    }
}

impl From<InstallTargetOverrideArgs> for griffr_common::config::InstallTargetOverrides {
    fn from(args: InstallTargetOverrideArgs) -> Self {
        Self {
            api: args.api.into(),
            executable: args.executable,
            data_root: args.data_root,
        }
    }
}

#[derive(Args, Debug, Clone)]
pub(crate) struct GameArg {
    /// Game ID (`arknights` or `endfield`)
    #[arg(long, requires = "region")]
    pub(crate) game: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct RegionArg {
    /// Launcher config/API region (`cn` or `sg`; aliases accepted)
    #[arg(long, requires = "game")]
    pub(crate) region: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct ChannelArg {
    /// API channel ID or alias (`official`, `bilibili`/`bili`); omitted means official
    #[arg(long, requires = "region")]
    pub(crate) channel: Option<String>,

    /// API sub-channel ID or alias (`official`, `bilibili`, `epic`, `google-play`); omitted copies channel
    #[arg(long = "sub-channel", aliases = ["subchannel", "sub_channel"], requires = "region")]
    pub(crate) sub_channel: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct GameRegionChannelArgs {
    #[command(flatten)]
    pub(crate) game: GameArg,

    #[command(flatten)]
    pub(crate) region: RegionArg,

    #[command(flatten)]
    pub(crate) channel: ChannelArg,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct RequiredGameRegionChannelArgs {
    /// Game ID (`arknights` or `endfield`)
    #[arg(long)]
    pub(crate) game: String,

    /// Launcher config/API region (`cn` or `sg`; aliases accepted)
    #[arg(long)]
    pub(crate) region: String,

    /// API channel ID or alias (`official`, `bilibili`/`bili`); omitted means official
    #[arg(long)]
    pub(crate) channel: Option<String>,

    /// API sub-channel ID or alias (`official`, `bilibili`, `epic`, `google-play`); omitted copies channel
    #[arg(long = "sub-channel", aliases = ["subchannel", "sub_channel"])]
    pub(crate) sub_channel: Option<String>,
}

impl RequiredGameRegionChannelArgs {
    pub(crate) fn into_parts(self) -> (String, String, Option<String>, Option<String>) {
        (self.game, self.region, self.channel, self.sub_channel)
    }
}

#[derive(Args)]
#[command(group(
    ArgGroup::new("target")
        .required(true)
        .args(["path", "game"])
))]
pub(crate) struct InfoSelectorArgs {
    /// Install root or config.ini path
    #[arg(long, conflicts_with_all = ["game", "region", "channel", "sub_channel"])]
    pub(crate) path: Option<std::path::PathBuf>,

    #[command(flatten)]
    pub(crate) remote: GameRegionChannelArgs,

    /// Launcher language
    #[arg(long, default_value = DEFAULT_LANGUAGE)]
    pub(crate) language: String,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    /// Download and install a game to an explicit path
    Install {
        #[command(flatten)]
        remote: RequiredGameRegionChannelArgs,

        #[command(flatten)]
        overrides: InstallTargetOverrideArgs,

        #[command(flatten)]
        path: PathArg,

        /// Re-run install into a non-empty path
        #[arg(long)]
        force: bool,

        #[command(flatten)]
        reuse: ReuseSourcesArg,

        /// Skip VFS resource download
        #[arg(long)]
        skip_vfs: bool,

        /// Keep downloaded package archives after successful extraction
        #[arg(long)]
        keep_pack_archives: bool,
    },

    /// Delete a local install path
    Uninstall {
        /// Install root
        #[arg(long)]
        path: std::path::PathBuf,

        /// Keep files on disk
        #[arg(long)]
        keep_files: bool,

        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },

    /// Update an existing install identified by its encrypted config.ini
    Update {
        #[command(flatten)]
        path: PathArg,

        #[command(flatten)]
        overrides: InstallTargetOverrideArgs,

        #[command(flatten)]
        reuse: ReuseSourcesArg,

        /// Skip post-update verification
        #[arg(long)]
        skip_verify: bool,

        /// Force full package instead of patch
        #[arg(long)]
        full_package: bool,

        /// Reuse staged predownload patch archives when they match the live update payload
        #[arg(long)]
        use_predownload: bool,

        /// Skip VFS resource download
        #[arg(long)]
        skip_vfs: bool,

        /// Keep downloaded package archives after successful extraction
        #[arg(long)]
        keep_pack_archives: bool,

        /// Put extraction staging and patch temporary files under this directory
        #[arg(long)]
        work_dir: Option<std::path::PathBuf>,

        /// Persist the VFS tree under this directory and link it into the install root
        #[arg(long)]
        external_vfs_root: Option<std::path::PathBuf>,
    },

    /// Predownload patch archive operations
    Predownload {
        #[command(subcommand)]
        command: PredownloadCommands,
    },

    /// Launch a local install path
    Launch {
        /// Install root or config.ini path
        #[arg(long)]
        path: std::path::PathBuf,

        /// Kill existing process if running
        #[arg(short, long)]
        force: bool,
    },

    /// Verify a local install against the latest game_files manifest
    Verify {
        #[command(flatten)]
        path: PathArg,

        #[command(flatten)]
        remote: GameRegionChannelArgs,

        #[command(flatten)]
        overrides: InstallTargetOverrideArgs,

        /// Repair corrupt or missing files and resync launcher metadata
        #[arg(short, long)]
        repair: bool,

        #[command(flatten)]
        reuse: ReuseSourcesArg,

        /// Prefer relinking from reuse sources even when local files already verify
        #[arg(long, requires = "repair", requires = "reuse_from")]
        relink_reuse: bool,

        /// Skip VFS resource sync during repair
        #[arg(long)]
        skip_vfs: bool,

        /// Do not read game/region/channel from local install metadata; requires --game and --region
        #[arg(long, requires = "game", requires = "region")]
        skip_local_detect: bool,
    },
    /// Bootstrap Persistent VFS state from StreamingAssets with launcher-parity scopes
    Bootstrap {
        #[command(flatten)]
        path: PathArg,

        #[command(flatten)]
        overrides: InstallTargetOverrideArgs,

        /// Bootstrap scope for ensuring Persistent files
        #[arg(long, default_value_t = VfsBootstrapScope::Initial)]
        scope: VfsBootstrapScope,

        #[command(flatten)]
        reuse: ReuseSourcesArg,

        /// Allow downloading missing files from CDN when not found in source roots
        #[arg(long)]
        allow_download: bool,

        /// Prefer relinking from source roots even when target files already verify
        #[arg(long)]
        relink_reuse: bool,

        /// Keep files outside the selected bootstrap scope (do not prune Persistent/VFS extras)
        #[arg(long)]
        no_prune: bool,
    },

    /// Print local metadata from config.ini and optionally the matching remote state
    Info {
        #[command(flatten)]
        selector: InfoSelectorArgs,
    },

    /// Fetch launcher news/media for a known game/region/channel
    News {
        #[command(flatten)]
        remote: RequiredGameRegionChannelArgs,

        #[command(flatten)]
        overrides: ApiTargetOverrideArgs,

        /// Launcher language
        #[arg(long, default_value = DEFAULT_LANGUAGE)]
        language: String,
    },

    /// Developer-only helpers for raw launcher artifacts
    Debug {
        #[command(subcommand)]
        command: DebugCommands,
    },

    /// Account session snapshot operations (explicit paths, no central registry)
    Account {
        #[command(subcommand)]
        command: AccountCommands,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum, Default, PartialEq, Eq)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum VfsDiffAgainst {
    Persistent,
    Streamingassets,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum SnapshotHashScope {
    None,
    Persistent,
    All,
}
