use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};
use tracing::debug;

use griffr_common::config::{GameId, ServerId};

mod commands;
mod progress;
mod ui;

/// Griffr - Hypergryph Game Launcher CLI
#[derive(Parser)]
#[command(name = "griffr")]
#[command(about = "A CLI launcher for Hypergryph games (Arknights / Endfield)")]
#[command(version)]
struct Cli {
    /// Perform a dry run without making changes
    #[arg(
        long,
        global = true,
        help = "Show what would be done without making changes"
    )]
    dry_run: bool,

    /// Enable verbose output
    #[arg(short, long, global = true, help = "Enable verbose logging")]
    verbose: bool,

    /// Output format for user-facing command results
    #[arg(
        long,
        global = true,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Choose text or JSON output for report-style commands"
    )]
    output: OutputFormat,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Download and install a game to an explicit path
    Install {
        /// Known game id
        #[arg(value_parser = ["arknights", "endfield"])]
        game: String,

        /// Known server id
        #[arg(
            long,
            value_parser = ["cn_official", "cn_bilibili", "global_official", "global_epic"]
        )]
        server: String,

        /// Install root
        #[arg(long)]
        path: std::path::PathBuf,

        /// Re-run install into a non-empty path
        #[arg(long)]
        force: bool,

        /// Reuse matching files from other local install paths
        #[arg(long = "reuse-from")]
        reuse_from: Vec<std::path::PathBuf>,

        /// Allow copying reused files if hardlinks fail
        #[arg(long)]
        force_copy: bool,

        /// Skip VFS resource download
        #[arg(long)]
        skip_vfs: bool,
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
        /// Install root or config.ini path
        #[arg(long)]
        path: std::path::PathBuf,

        /// Reuse matching files from another local install path
        #[arg(long = "reuse-from")]
        reuse_from: Vec<std::path::PathBuf>,

        /// Allow copying reused files if hardlinks fail
        #[arg(long)]
        force_copy: bool,

        /// Skip post-update verification
        #[arg(long)]
        skip_verify: bool,

        /// Force full package instead of patch
        #[arg(long)]
        full_package: bool,

        /// Skip VFS resource download
        #[arg(long)]
        skip_vfs: bool,
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
        /// Install root or config.ini path
        #[arg(long)]
        path: std::path::PathBuf,

        /// Repair corrupt or missing files and resync launcher metadata
        #[arg(short, long)]
        repair: bool,

        /// Reuse matching files from other local install paths during repair
        #[arg(long = "reuse-from")]
        reuse_from: Vec<std::path::PathBuf>,

        /// Allow copying reused files if hardlinks fail
        #[arg(long)]
        force_copy: bool,

        /// Prefer relinking from reuse sources even when local files already verify
        #[arg(long)]
        relink_reuse: bool,

        /// Skip VFS resource sync during repair
        #[arg(long)]
        skip_vfs: bool,
    },

    /// Print local metadata from config.ini and optionally the matching remote state
    Info {
        #[command(flatten)]
        target: QueryTarget,
    },

    /// Fetch launcher news/media for a known game/server
    News {
        #[command(flatten)]
        remote: RemoteTarget,

        /// Launcher language
        #[arg(long, default_value = "zh-cn")]
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

#[derive(Args)]
struct QueryTarget {
    /// Install root or config.ini path
    #[arg(long)]
    path: Option<std::path::PathBuf>,

    /// Known game id for remote lookup when no local path is provided
    #[arg(long, value_parser = ["arknights", "endfield"])]
    game: Option<String>,

    /// Known server id for remote lookup when no local path is provided
    #[arg(
        long,
        value_parser = ["cn_official", "cn_bilibili", "global_official", "global_epic"]
    )]
    server: Option<String>,

    /// Launcher language
    #[arg(long, default_value = "zh-cn")]
    language: String,
}

#[derive(Args)]
struct RemoteTarget {
    #[arg(long, value_parser = ["arknights", "endfield"])]
    game: String,

    #[arg(
        long,
        value_parser = ["cn_official", "cn_bilibili", "global_official", "global_epic"]
    )]
    server: String,
}

#[derive(Debug, Clone, Copy, ValueEnum, Default, PartialEq, Eq)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Subcommand)]
enum DebugCommands {
    /// Detect known game/server/version from encrypted config.ini
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
    /// Call get_latest_game and print raw response JSON
    GetRawLatestGame {
        #[command(flatten)]
        remote: RemoteTarget,

        #[arg(long)]
        version: Option<String>,

        #[arg(long, id = "api_get_latest_game_output")]
        output: Option<std::path::PathBuf>,
    },
    /// Call get_latest_resources and print raw response JSON
    GetRawLatestResources {
        #[command(flatten)]
        remote: RemoteTarget,

        /// Version passed to get_latest_game for version/rand resolution
        #[arg(long)]
        version: Option<String>,

        /// Full version used for get_latest_resources (defaults to resolved latest version)
        #[arg(long = "resource-version")]
        resource_version: Option<String>,

        /// rand_str for get_latest_resources (defaults to resolved latest rand_str)
        #[arg(long = "rand-str")]
        rand_str: Option<String>,

        /// Platform for get_latest_resources
        #[arg(long, default_value = "Windows")]
        platform: String,

        #[arg(long = "output-file", id = "api_get_latest_resources_output")]
        output: Option<std::path::PathBuf>,
    },
    /// Fetch and print the remote game_files manifest
    ListGameFiles {
        #[command(flatten)]
        remote: RemoteTarget,

        #[arg(long)]
        version: Option<String>,

        #[arg(long, id = "api_get_game_files_output")]
        output: Option<std::path::PathBuf>,
    },
    /// List files from latest resource indexes (index_main/index_initial)
    ListResourceFiles {
        #[command(flatten)]
        remote: RemoteTarget,

        /// Version passed to get_latest_game for version/rand resolution
        #[arg(long)]
        version: Option<String>,

        /// Full version used for get_latest_resources (defaults to resolved latest version)
        #[arg(long = "resource-version")]
        resource_version: Option<String>,

        /// rand_str for get_latest_resources (defaults to resolved latest rand_str)
        #[arg(long = "rand-str")]
        rand_str: Option<String>,

        /// Platform for get_latest_resources
        #[arg(long, default_value = "Windows")]
        platform: String,

        #[arg(long = "output-file", id = "list_resource_files_output")]
        output: Option<std::path::PathBuf>,
    },
    /// Fetch one file referenced by the latest remote game_files manifest
    GetFile {
        #[command(flatten)]
        remote: RemoteTarget,

        #[arg(long)]
        version: Option<String>,

        #[arg(long)]
        file: String,

        #[arg(long, id = "api_get_file_output")]
        output: std::path::PathBuf,
    },
    /// Fetch raw media/news payload as JSON
    GetRawMedia {
        #[command(flatten)]
        remote: RemoteTarget,

        /// Launcher language
        #[arg(long, default_value = "zh-cn")]
        language: String,

        /// Optional output file path for JSON payload
        #[arg(long = "output-file", id = "api_get_media_output")]
        output: Option<std::path::PathBuf>,
    },
    /// Fetch normalized media/news payload as JSON
    GetMedia {
        #[command(flatten)]
        remote: RemoteTarget,

        /// Launcher language
        #[arg(long, default_value = "zh-cn")]
        language: String,

        /// Optional output file path for JSON payload
        #[arg(long = "output-file", id = "fetch_media_output")]
        output: Option<std::path::PathBuf>,
    },
}

#[derive(Subcommand)]
enum AccountCommands {
    /// Capture current local account state into a directory bundle
    Capture {
        /// Known game id
        #[arg(value_parser = ["arknights", "endfield"])]
        game: String,

        /// Optional server hint to narrow default sdk_data discovery roots
        #[arg(
            long,
            value_parser = ["cn_official", "cn_bilibili", "global_official", "global_epic"]
        )]
        server_hint: Option<String>,

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
        #[arg(long)]
        include_install_mmkv: bool,

        /// Replace bundle destination if it already exists
        #[arg(long)]
        force: bool,
    },

    /// Activate account state from a directory bundle
    Activate {
        /// Known game id
        #[arg(value_parser = ["arknights", "endfield"])]
        game: String,

        /// Optional server hint to narrow default sdk_data discovery roots
        #[arg(
            long,
            value_parser = ["cn_official", "cn_bilibili", "global_official", "global_epic"]
        )]
        server_hint: Option<String>,

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
        #[arg(long)]
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

#[compio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let default_level = if cli.verbose {
        "warn,griffr=debug,griffr_common=debug"
    } else {
        "warn"
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_level));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .init();

    let opts = GlobalOptions {
        dry_run: cli.dry_run,
        verbose: cli.verbose,
        skip_verify: false,
        force_full_package: false,
        skip_vfs: false,
        output: cli.output,
    };

    if opts.verbose {
        debug!("Griffr CLI started");
        debug!("Dry run: {}", opts.dry_run);
        debug!("Verbose: {}", opts.verbose);
    }

    match cli.command {
        Commands::Install {
            game,
            server,
            path,
            force,
            reuse_from,
            force_copy,
            skip_vfs,
        } => {
            let game_id = game.parse::<GameId>()?;
            let server_id = server.parse::<ServerId>()?;

            let opts = GlobalOptions { skip_vfs, ..opts };

            opts.verbose(format!(
                "Install command: game={:?}, server={:?}, path={:?}, reuse_from={:?}, force_copy={}, skip_vfs={}",
                game_id, server_id, path, reuse_from, force_copy, skip_vfs
            ));
            commands::install(
                game_id, server_id, path, force, reuse_from, force_copy, opts,
            )
            .await?;
        }

        Commands::Uninstall {
            path,
            keep_files,
            yes,
        } => {
            opts.verbose(format!(
                "Uninstall command: path={:?}, keep_files={}, yes={}",
                path, keep_files, yes
            ));
            commands::uninstall(path, keep_files, yes, opts).await?;
        }

        Commands::Update {
            path,
            reuse_from,
            force_copy,
            skip_verify,
            full_package,
            skip_vfs,
        } => {
            let opts = GlobalOptions {
                skip_verify,
                force_full_package: full_package,
                skip_vfs,
                ..opts
            };
            opts.verbose(format!(
                "Update path: {:?}, reuse_from={:?}, force_copy={}",
                path, reuse_from, force_copy
            ));
            commands::update(path, reuse_from, force_copy, opts).await?;
        }

        Commands::Launch { path, force } => {
            opts.verbose(format!("Launch path: {:?}, force={}", path, force));
            commands::launch(path, force, opts).await?;
        }

        Commands::Verify {
            path,
            repair,
            reuse_from,
            force_copy,
            relink_reuse,
            skip_vfs,
        } => {
            opts.verbose(format!(
                "Verify path: {:?}, repair={}, reuse_from={:?}, force_copy={}, relink_reuse={}, skip_vfs={}",
                path, repair, reuse_from, force_copy, relink_reuse, skip_vfs
            ));
            commands::verify(
                path,
                repair,
                reuse_from,
                force_copy,
                relink_reuse,
                skip_vfs,
                opts,
            )
            .await?;
        }

        Commands::Info { target } => {
            opts.verbose("Info query");
            commands::info_show(
                target.path,
                target.game,
                target.server,
                &target.language,
                opts,
            )
            .await?;
        }

        Commands::News { remote, language } => {
            let game_id = remote.game.parse::<GameId>()?;
            let server_id = remote.server.parse::<ServerId>()?;
            opts.verbose(format!("News: {:?} {:?}", game_id, server_id));
            commands::news_show(game_id, server_id, &language, opts).await?;
        }

        Commands::Debug { command } => match command {
            DebugCommands::DetectConfigIni { path } => commands::debug_detect(path, opts).await?,
            DebugCommands::DecryptConfigIni { path } => {
                commands::debug_config_ini(path, opts).await?
            }
            DebugCommands::DecryptGameFiles { path } => commands::debug_game_files(path, opts).await?,
            DebugCommands::GetRawLatestGame {
                remote,
                version,
                output,
            } => {
                let game_id = remote.game.parse::<GameId>()?;
                let server_id = remote.server.parse::<ServerId>()?;
                commands::debug_api_get_latest_game(game_id, server_id, version, output, opts)
                    .await?;
            }
            DebugCommands::GetRawLatestResources {
                remote,
                version,
                resource_version,
                rand_str,
                platform,
                output,
            } => {
                let game_id = remote.game.parse::<GameId>()?;
                let server_id = remote.server.parse::<ServerId>()?;
                commands::debug_api_get_latest_resources(
                    game_id,
                    server_id,
                    version,
                    resource_version,
                    rand_str,
                    platform,
                    output,
                    opts,
                )
                .await?;
            }
            DebugCommands::ListGameFiles {
                remote,
                version,
                output,
            } => {
                let game_id = remote.game.parse::<GameId>()?;
                let server_id = remote.server.parse::<ServerId>()?;
                commands::debug_fetch_game_files(game_id, server_id, version, output, opts).await?;
            }
            DebugCommands::ListResourceFiles {
                remote,
                version,
                resource_version,
                rand_str,
                platform,
                output,
            } => {
                let game_id = remote.game.parse::<GameId>()?;
                let server_id = remote.server.parse::<ServerId>()?;
                commands::debug_list_resource_files(
                    game_id,
                    server_id,
                    version,
                    resource_version,
                    rand_str,
                    platform,
                    output,
                    opts,
                )
                .await?;
            }
            DebugCommands::GetFile {
                remote,
                version,
                file,
                output,
            } => {
                let game_id = remote.game.parse::<GameId>()?;
                let server_id = remote.server.parse::<ServerId>()?;
                commands::debug_fetch_file(game_id, server_id, version, file, output, opts).await?;
            }
            DebugCommands::GetRawMedia {
                remote,
                language,
                output,
            } => {
                let game_id = remote.game.parse::<GameId>()?;
                let server_id = remote.server.parse::<ServerId>()?;
                commands::debug_api_get_media(game_id, server_id, language, output, opts).await?;
            }
            DebugCommands::GetMedia {
                remote,
                language,
                output,
            } => {
                let game_id = remote.game.parse::<GameId>()?;
                let server_id = remote.server.parse::<ServerId>()?;
                commands::debug_fetch_media(game_id, server_id, language, output, opts).await?;
            }
        },
        Commands::Account { command } => match command {
            AccountCommands::Capture {
                game,
                server_hint,
                bundle,
                sdk_dir,
                install_path,
                include_install_mmkv,
                force,
            } => {
                let game_id = game.parse::<GameId>()?;
                let server_hint = server_hint.map(|s| s.parse::<ServerId>()).transpose()?;
                commands::account_capture(
                    game_id,
                    server_hint,
                    bundle,
                    sdk_dir,
                    install_path,
                    include_install_mmkv,
                    force,
                    opts,
                )
                .await?;
            }
            AccountCommands::Activate {
                game,
                server_hint,
                bundle,
                sdk_dir,
                install_path,
                include_install_mmkv,
                force,
            } => {
                let game_id = game.parse::<GameId>()?;
                let server_hint = server_hint.map(|s| s.parse::<ServerId>()).transpose()?;
                commands::account_activate(
                    game_id,
                    server_hint,
                    bundle,
                    sdk_dir,
                    install_path,
                    include_install_mmkv,
                    force,
                    opts,
                )
                .await?;
            }
        },
    }

    Ok(())
}
