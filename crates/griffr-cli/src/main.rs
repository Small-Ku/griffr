use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use tracing::{debug, info};

use griffr_common::config::{GameId, ServerId};

mod commands;
mod progress;

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

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Download and install a game to an explicit path
    Install {
        /// Known game id
        game: String,

        /// Known server id
        #[arg(long)]
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
}

#[derive(Args)]
struct QueryTarget {
    /// Install root or config.ini path
    #[arg(long)]
    path: Option<std::path::PathBuf>,

    /// Known game id for remote lookup when no local path is provided
    #[arg(long)]
    game: Option<String>,

    /// Known server id for remote lookup when no local path is provided
    #[arg(long)]
    server: Option<String>,

    /// Launcher language
    #[arg(long, default_value = "zh-cn")]
    language: String,
}

#[derive(Args)]
struct RemoteTarget {
    #[arg(long)]
    game: String,

    #[arg(long)]
    server: String,
}

#[derive(Subcommand)]
enum DebugCommands {
    /// Detect known game/server/version from encrypted config.ini
    Detect {
        #[arg(long)]
        path: std::path::PathBuf,
    },
    /// Print decrypted config.ini contents
    ConfigIni {
        #[arg(long)]
        path: std::path::PathBuf,
    },
    /// Print decrypted local game_files contents
    GameFiles {
        #[arg(long)]
        path: std::path::PathBuf,
    },
    /// Fetch and print the remote game_files manifest
    FetchGameFiles {
        #[command(flatten)]
        remote: RemoteTarget,

        #[arg(long)]
        version: Option<String>,

        #[arg(long)]
        output: Option<std::path::PathBuf>,
    },
    /// Fetch one file referenced by the latest remote game_files manifest
    FetchFile {
        #[command(flatten)]
        remote: RemoteTarget,

        #[arg(long)]
        version: Option<String>,

        #[arg(long)]
        file: String,

        #[arg(long)]
        output: std::path::PathBuf,
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
            info!(dry_run = true, "{}", msg.as_ref());
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
        "info,griffr=debug,griffr_common=debug"
    } else {
        "info"
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
        } => {
            opts.verbose(format!(
                "Verify path: {:?}, repair={}, reuse_from={:?}, force_copy={}",
                path, repair, reuse_from, force_copy
            ));
            commands::verify(path, repair, reuse_from, force_copy, opts).await?;
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
            DebugCommands::Detect { path } => commands::debug_detect(path, opts).await?,
            DebugCommands::ConfigIni { path } => commands::debug_config_ini(path, opts).await?,
            DebugCommands::GameFiles { path } => commands::debug_game_files(path, opts).await?,
            DebugCommands::FetchGameFiles {
                remote,
                version,
                output,
            } => {
                let game_id = remote.game.parse::<GameId>()?;
                let server_id = remote.server.parse::<ServerId>()?;
                commands::debug_fetch_game_files(game_id, server_id, version, output, opts).await?;
            }
            DebugCommands::FetchFile {
                remote,
                version,
                file,
                output,
            } => {
                let game_id = remote.game.parse::<GameId>()?;
                let server_id = remote.server.parse::<ServerId>()?;
                commands::debug_fetch_file(game_id, server_id, version, file, output, opts).await?;
            }
        },
    }

    Ok(())
}
