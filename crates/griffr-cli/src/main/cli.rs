use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use griffr_common::api::protocol::DEFAULT_LANGUAGE;
use griffr_common::runtime::PersistentVfsFileSet;

use crate::debug_cli::{AccountCommands, DebugCommands, StageCommands};

/// Griffr - Hypergryph Game Launcher CLI
#[derive(Parser)]
#[command(name = "griffr")]
#[command(about = "A CLI launcher for Hypergryph/Gryphline/YoStar games (Arknights / Endfield)")]
#[command(version)]
pub(crate) struct Cli {
    /// Enable diagnostic logging on stderr
    #[arg(
        short,
        long,
        global = true,
        conflicts_with = "quiet",
        help = "Enable diagnostic logging"
    )]
    pub(crate) verbose: bool,

    /// Suppress progress and nonessential status output
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub(crate) quiet: bool,

    #[command(subcommand)]
    pub(crate) command: Box<Commands>,
}

#[derive(Args)]
pub(crate) struct PathArg {
    /// Install root or native launcher metadata path
    #[arg(long)]
    pub(crate) path: std::path::PathBuf,
}

#[derive(Args)]
pub(crate) struct TargetPathsArg {
    /// Install root or native launcher metadata path; repeat --path to process a batch
    #[arg(long = "path", required = true)]
    pub(crate) paths: Vec<std::path::PathBuf>,
}

#[derive(Args, Debug, Clone, Copy, Default)]
pub(crate) struct MutationArgs {
    /// Show planned changes without modifying files or processes
    #[arg(long)]
    pub(crate) dry_run: bool,
}

#[derive(Args, Debug, Clone, Copy)]
pub(crate) struct BatchArgs {
    /// Maximum target operations run concurrently; targets sharing storage are serialized
    #[arg(long, default_value_t = 1)]
    pub(crate) jobs: usize,

    /// Stop after the first failed target; requires --jobs 1
    #[arg(long, conflicts_with = "keep_going")]
    pub(crate) fail_fast: bool,

    /// Continue after target failures (the default)
    #[arg(long, conflicts_with = "fail_fast")]
    pub(crate) keep_going: bool,
}

impl Default for BatchArgs {
    fn default() -> Self {
        Self {
            jobs: 1,
            fail_fast: false,
            keep_going: false,
        }
    }
}

impl BatchArgs {
    pub(crate) const fn continue_after_failure(self) -> bool {
        self.keep_going || !self.fail_fast
    }
}

#[derive(Args, Debug, Clone, Copy)]
pub(crate) struct OutputArgs {
    /// Output format for the final report
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub(crate) output: OutputFormat,
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

    /// Override game program file name (e.g. Arknights.exe)
    #[arg(long = "exe")]
    pub exe_name: Option<String>,

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
            exe_name: args.exe_name,
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
    /// Launcher/API region (`cn`, `sg`, or YoStar `en`; aliases accepted)
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

    /// Launcher/API region (`cn`, `sg`, or YoStar `en`; aliases accepted)
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

#[derive(Args, Debug)]
#[command(group(
    ArgGroup::new("target")
        .required(true)
        .args(["path", "game"])
))]
pub(crate) struct InfoSelectorArgs {
    /// Install root or native launcher metadata path
    #[arg(long, conflicts_with_all = ["game", "region", "channel", "sub_channel"])]
    pub(crate) path: Option<std::path::PathBuf>,

    #[command(flatten)]
    pub(crate) remote: GameRegionChannelArgs,

    /// Fetch the matching remote release when --path is used
    #[arg(long = "remote", conflicts_with = "local_only")]
    pub(crate) remote_state: bool,

    /// Never contact the remote API; valid only with --path
    #[arg(long, requires = "path", conflicts_with = "remote_state")]
    pub(crate) local_only: bool,

    /// Include a remote media summary; implies remote lookup
    #[arg(long, conflicts_with = "local_only")]
    pub(crate) include_media: bool,

    /// Launcher language used for media lookup
    #[arg(long, default_value = DEFAULT_LANGUAGE)]
    pub(crate) language: String,

    #[command(flatten)]
    pub(crate) report: OutputArgs,
}

#[derive(Args)]
pub(crate) struct PersistentResourceArgs {
    #[command(flatten)]
    pub(crate) mutation: MutationArgs,

    #[command(flatten)]
    pub(crate) path: PathArg,

    #[command(flatten)]
    pub(crate) overrides: InstallTargetOverrideArgs,

    /// File set to write in Persistent
    #[arg(long, default_value_t = PersistentVfsFileSet::Base)]
    pub(crate) file_set: PersistentVfsFileSet,

    /// Reuse matching files from other local install paths; Persistent copies are never hardlinked
    #[arg(long = "reuse-from")]
    pub(crate) reuse_from: Vec<std::path::PathBuf>,

    /// Allow downloading missing files from CDN when not found in source roots
    #[arg(long)]
    pub(crate) allow_download: bool,

    /// Prefer copying from reuse sources even when target files already verify
    #[arg(long)]
    pub(crate) prefer_reuse: bool,

    /// Remove previously Griffr-managed Persistent files no longer in the selected file set
    #[arg(long)]
    pub(crate) prune: bool,
}

#[derive(Subcommand)]
pub(crate) enum ResourceCommands {
    /// Synchronize the selected Persistent resource working set
    Sync {
        #[command(flatten)]
        args: PersistentResourceArgs,
    },
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    /// Download and install a game to an explicit path
    Install {
        #[command(flatten)]
        mutation: MutationArgs,

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

        /// Choose how launcher resource-index files are sourced
        #[arg(
            long = "resources",
            alias = "resource-policy",
            value_enum,
            conflicts_with = "skip_vfs"
        )]
        resource_policy: Option<ResourcePolicyArg>,

        /// Skip launcher resource-index sync. Package and game_files resource entries are still installed and verified.
        #[arg(long, hide = true, conflicts_with = "resource_policy")]
        skip_vfs: bool,

        /// Keep downloaded package archives after successful extraction
        #[arg(long)]
        keep_pack_archives: bool,
    },

    /// Delete a local install path
    Uninstall {
        #[command(flatten)]
        mutation: MutationArgs,

        /// Install root
        #[arg(long)]
        path: std::path::PathBuf,

        /// Remove Griffr private state but keep game and external asset files
        #[arg(long, alias = "keep-files")]
        detach: bool,

        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },

    /// Update one or more existing installs identified by native launcher metadata
    Update {
        #[command(flatten)]
        mutation: MutationArgs,

        #[command(flatten)]
        batch: BatchArgs,

        #[command(flatten)]
        paths: TargetPathsArg,

        #[command(flatten)]
        overrides: InstallTargetOverrideArgs,

        #[command(flatten)]
        reuse: ReuseSourcesArg,

        /// Commit downloaded content but keep the install blocked until a later verify --repair
        #[arg(long = "defer-verification", alias = "skip-verify")]
        defer_verification: bool,

        /// Force full package instead of patch
        #[arg(long)]
        full_package: bool,

        /// Prefer archives from this staging directory
        #[arg(long = "stage-dir")]
        stage_dir: Option<std::path::PathBuf>,

        /// Fail instead of downloading when staged archives are absent or mismatched
        #[arg(long, requires = "stage_dir")]
        require_staged: bool,

        /// Deprecated alias that uses Griffr's default stage directory
        #[arg(long, hide = true)]
        use_predownload: bool,

        /// Choose how launcher resource-index files are sourced
        #[arg(
            long = "resources",
            alias = "resource-policy",
            value_enum,
            conflicts_with = "skip_vfs"
        )]
        resource_policy: Option<ResourcePolicyArg>,

        /// Skip launcher resource-index sync. Package and game_files resource entries are still installed and verified.
        #[arg(long, hide = true, conflicts_with = "resource_policy")]
        skip_vfs: bool,

        /// Keep downloaded package archives after successful extraction
        #[arg(long)]
        keep_pack_archives: bool,

        /// Put extraction staging and patch temporary files under this directory
        #[arg(long)]
        work_dir: Option<std::path::PathBuf>,

        /// Persist the patch-managed asset tree under this directory and link it into the install root
        #[arg(long)]
        external_asset_root: Option<std::path::PathBuf>,
    },

    /// Inspect and fetch staged update archives
    #[command(alias = "predownload")]
    Stage {
        #[command(subcommand)]
        command: StageCommands,
    },

    /// Resume a persisted patch transaction
    Recover {
        #[command(flatten)]
        mutation: MutationArgs,

        #[command(flatten)]
        path: PathArg,
    },

    /// Launch a local install path
    Launch {
        #[command(flatten)]
        mutation: MutationArgs,

        /// Install root or native launcher metadata path
        #[arg(long)]
        path: std::path::PathBuf,

        /// Stop the existing process if it is running
        #[arg(short, long)]
        force: bool,

        /// Wine-compatible runner used on non-Windows hosts
        #[arg(long, value_name = "PROGRAM")]
        wine: Option<std::path::PathBuf>,

        /// Wine prefix used on non-Windows hosts
        #[arg(long, value_name = "PATH")]
        wine_prefix: Option<std::path::PathBuf>,
    },

    /// Verify one or more local installs against their native launcher manifests
    Verify {
        #[command(flatten)]
        mutation: MutationArgs,

        #[command(flatten)]
        batch: BatchArgs,

        #[command(flatten)]
        paths: TargetPathsArg,

        #[command(flatten)]
        remote: GameRegionChannelArgs,

        #[command(flatten)]
        overrides: InstallTargetOverrideArgs,

        /// Repair corrupt or missing files and resync launcher metadata
        #[arg(short, long)]
        repair: bool,

        #[command(flatten)]
        reuse: ReuseSourcesArg,

        /// Prefer relinking from explicit sources or same-game batch peers
        #[arg(long, requires = "repair")]
        relink_reuse: bool,

        /// Select integrity scope. Without this option, unfinished changes reuse their saved resource policy; other verifies use all files.
        #[arg(long, value_enum, conflicts_with = "skip_vfs")]
        scope: Option<VerifyScopeArg>,

        /// Deprecated alias for --scope core
        #[arg(long, hide = true, conflicts_with = "scope")]
        skip_vfs: bool,

        /// Do not read game/region/channel from local install metadata; requires --game and --region
        #[arg(long, requires = "game", requires = "region")]
        skip_local_detect: bool,

        #[command(flatten)]
        report: OutputArgs,
    },
    /// Manage launcher resource working sets
    Resources {
        #[command(subcommand)]
        command: ResourceCommands,
    },

    /// Legacy alias for `resources sync`
    #[command(hide = true)]
    SetupPersistentResources {
        #[command(flatten)]
        args: PersistentResourceArgs,
    },

    /// Print native local launcher metadata and optionally the matching remote state
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

        /// Include announcement IDs and URLs in text output
        #[arg(long)]
        include_links: bool,

        #[command(flatten)]
        report: OutputArgs,
    },

    /// Developer-only helpers for raw launcher artifacts
    Debug {
        #[command(subcommand)]
        command: DebugCommands,
    },

    /// Account session snapshot calls (explicit paths, no central registry)
    Account {
        #[command(subcommand)]
        command: AccountCommands,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum ResourcePolicyArg {
    /// Use launcher resource indexes when the endpoint supports them.
    Auto,
    /// Do not query resource indexes; packages and game_files remain authoritative.
    PackageOnly,
}

impl ResourcePolicyArg {
    pub const fn resolve(value: Option<Self>, skip_vfs: bool) -> Self {
        if skip_vfs {
            Self::PackageOnly
        } else {
            match value {
                Some(value) => value,
                None => Self::Auto,
            }
        }
    }

    pub const fn uses_resource_index(self) -> bool {
        matches!(self, Self::Auto)
    }
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum VerifyScopeArg {
    /// Verify core game files and launcher resource-index files.
    All,
    /// Verify core game files only; do not query or hash launcher resource-index paths.
    Core,
    /// Verify launcher resource-index paths only.
    Resources,
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
