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
pub struct InstallProfileOverrideArgs {
    #[command(flatten)]
    pub api: ApiTargetOverrideArgs,

    /// Override game executable filename (e.g. Arknights.exe)
    #[arg(long = "executable")]
    pub executable: Option<String>,

    /// Override game data-root directory name (e.g. Arknights_Data)
    #[arg(long = "data-root")]
    pub data_root: Option<String>,
}

impl From<ApiTargetOverrideArgs> for griffr_common::config::TargetOverride {
    fn from(args: ApiTargetOverrideArgs) -> Self {
        Self {
            gateway: args.gateway,
            game_appcode: args.game_appcode,
            launcher_appcode: args.launcher_appcode,
            ..Default::default()
        }
    }
}

impl From<InstallProfileOverrideArgs> for griffr_common::config::TargetOverride {
    fn from(args: InstallProfileOverrideArgs) -> Self {
        Self {
            gateway: args.api.gateway,
            game_appcode: args.api.game_appcode,
            launcher_appcode: args.api.launcher_appcode,
            executable: args.executable,
            data_root: args.data_root,
        }
    }
}

#[derive(Args, Debug, Clone)]
pub(crate) struct GameArg {
    /// Known game id or custom game
    #[arg(long, requires = "channel")]
    pub(crate) game: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct ChannelArg {
    /// API channel ID or friendly alias
    #[arg(long, requires = "game")]
    pub(crate) channel: Option<String>,

    /// API sub-channel ID or friendly alias; defaults to --channel
    #[arg(long = "sub-channel", alias = "subchannel", requires = "channel")]
    pub(crate) sub_channel: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct GameChannelArgs {
    #[command(flatten)]
    pub(crate) game: GameArg,

    #[command(flatten)]
    pub(crate) channel: ChannelArg,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct RequiredGameChannelArgs {
    /// Known game id or custom game
    #[arg(long)]
    pub(crate) game: String,

    /// API channel ID or friendly alias
    #[arg(long)]
    pub(crate) channel: String,

    /// API sub-channel ID or friendly alias; defaults to --channel
    #[arg(long = "sub-channel", alias = "subchannel")]
    pub(crate) sub_channel: Option<String>,
}

impl RequiredGameChannelArgs {
    pub(crate) fn into_parts(self) -> (String, String, Option<String>) {
        (self.game, self.channel, self.sub_channel)
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
    #[arg(long, conflicts_with_all = ["game", "channel"])]
    pub(crate) path: Option<std::path::PathBuf>,

    #[command(flatten)]
    pub(crate) game_channel: GameChannelArgs,

    /// Launcher language
    #[arg(long, default_value = DEFAULT_LANGUAGE)]
    pub(crate) language: String,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    /// Download and install a game to an explicit path
    Install {
        #[command(flatten)]
        remote: RequiredGameChannelArgs,

        #[command(flatten)]
        overrides: InstallProfileOverrideArgs,

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
        overrides: InstallProfileOverrideArgs,

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
        remote: GameChannelArgs,

        #[command(flatten)]
        overrides: InstallProfileOverrideArgs,

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

        /// Do not read game/channel from local install metadata; requires --game and --channel
        #[arg(long, requires = "game", requires = "channel")]
        skip_local_detect: bool,
    },
    /// Bootstrap Persistent VFS state from StreamingAssets with launcher-parity scopes
    Bootstrap {
        #[command(flatten)]
        path: PathArg,

        #[command(flatten)]
        overrides: InstallProfileOverrideArgs,

        /// Bootstrap scope for Persistent materialization
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

    /// Fetch launcher news/media for a known game/channel
    News {
        #[command(flatten)]
        remote: RequiredGameChannelArgs,

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
