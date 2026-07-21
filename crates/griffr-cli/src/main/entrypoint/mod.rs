use crate::cli::*;
use crate::debug_cli::*;
use crate::{commands, GlobalOptions};
use anyhow::Result;
use clap::Parser;
use griffr_common::config::{ChannelPair, GameId, RegionId};
use tracing::debug;

mod account;
mod debug;

#[cfg(test)]
mod tests;

fn parse_remote_args(
    remote: RequiredGameRegionChannelArgs,
) -> Result<(GameId, RegionId, ChannelPair)> {
    let (game, region, channel, sub_channel) = remote.into_parts();
    let region = region.parse::<RegionId>()?;
    Ok((
        game.parse::<GameId>()?,
        region,
        ChannelPair::parse(region, channel, sub_channel)?,
    ))
}

pub(crate) async fn run() -> Result<()> {
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
        keep_pack_archives: false,
        extraction_progress_buffer_bytes: cli.extraction_progress_buffer_bytes,
        download_progress_buffer_bytes: cli.download_progress_buffer_bytes,
        volume_read_limit: cli.volume_read_limit,
        volume_write_limit: cli.volume_write_limit,
        volume_metadata_limit: cli.volume_metadata_limit,
        volume_streaming_pressure_limit: cli.volume_streaming_pressure_limit,
        volume_streaming_mode: cli.volume_streaming_mode.into(),
        reuse_queue_limit: cli.reuse_queue_limit,
        output: cli.output,
    };

    if opts.verbose {
        debug!("Griffr CLI started");
        debug!("Dry run: {}", opts.dry_run);
        debug!("Verbose: {}", opts.verbose);
    }

    match cli.command {
        Commands::Install {
            remote,
            overrides,
            path,
            force,
            reuse,
            skip_vfs,
            keep_pack_archives,
        } => {
            let (game_id, region_id, channel_id) = parse_remote_args(remote)?;
            let PathArg { path } = path;
            let ReuseSourcesArg {
                reuse_from,
                force_copy,
            } = reuse;

            let opts = GlobalOptions {
                skip_vfs,
                keep_pack_archives,
                ..opts
            };

            opts.verbose(format!(
                "Install command: game={:?}, region={}, channel={:?}, path={:?}, reuse_from={:?}, force_copy={}, skip_vfs={}, keep_pack_archives={}",
                game_id, region_id, channel_id, path, reuse_from, force_copy, skip_vfs, keep_pack_archives
            ));
            commands::install(
                game_id, region_id, channel_id, overrides, path, force, reuse_from, force_copy,
                opts,
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
            overrides,
            reuse,
            skip_verify,
            full_package,
            use_predownload,
            skip_vfs,
            keep_pack_archives,
            work_dir,
            external_vfs_root,
        } => {
            let PathArg { path } = path;
            let ReuseSourcesArg {
                reuse_from,
                force_copy,
            } = reuse;
            let opts = GlobalOptions {
                skip_verify,
                force_full_package: full_package,
                skip_vfs,
                keep_pack_archives,
                ..opts
            };
            opts.verbose(format!(
                "Update path: {:?}, reuse_from={:?}, force_copy={}, use_predownload={}",
                path, reuse_from, force_copy, use_predownload
            ));
            commands::update(
                path,
                overrides,
                reuse_from,
                force_copy,
                use_predownload,
                griffr_common::runtime::PatchApplyOptions {
                    work_dir,
                    external_vfs_root,
                },
                opts,
            )
            .await?;
        }

        Commands::Predownload { command } => match command {
            PredownloadCommands::Check { path } => {
                let PathArg { path } = path;
                opts.verbose(format!("Predownload check path: {:?}", path));
                commands::predownload_check(path, opts).await?;
            }
            PredownloadCommands::Fetch { path, output_dir } => {
                let PathArg { path } = path;
                opts.verbose(format!(
                    "Predownload fetch path: {:?}, output_dir={:?}",
                    path, output_dir
                ));
                commands::predownload_fetch(path, output_dir, opts).await?;
            }
            PredownloadCommands::Apply {
                path,
                overrides,
                output_dir,
                skip_verify,
                skip_vfs,
                keep_pack_archives,
                work_dir,
                external_vfs_root,
            } => {
                let PathArg { path } = path;
                let opts = GlobalOptions {
                    skip_verify,
                    skip_vfs,
                    keep_pack_archives,
                    ..opts
                };
                opts.verbose(format!(
                    "Predownload apply path: {:?}, output_dir={:?}",
                    path, output_dir
                ));
                commands::predownload_apply(
                    path,
                    overrides,
                    output_dir,
                    griffr_common::runtime::PatchApplyOptions {
                        work_dir,
                        external_vfs_root,
                    },
                    opts,
                )
                .await?;
            }
            PredownloadCommands::Resume { path } => {
                let PathArg { path } = path;
                opts.verbose(format!("Predownload resume path: {:?}", path));
                commands::predownload_resume(path, opts).await?;
            }
        },

        Commands::Launch { path, force } => {
            opts.verbose(format!("Launch path: {:?}, force={}", path, force));
            commands::launch(path, force, opts).await?;
        }

        Commands::Verify {
            path,
            remote,
            overrides,
            repair,
            reuse,
            relink_reuse,
            skip_vfs,
            skip_local_detect,
        } => {
            let PathArg { path } = path;
            let ReuseSourcesArg {
                reuse_from,
                force_copy,
            } = reuse;
            let GameRegionChannelArgs {
                game: GameArg { game },
                region: RegionArg { region },
                channel:
                    ChannelArg {
                        channel,
                        sub_channel,
                    },
            } = remote;
            let game = game.map(|value| value.parse::<GameId>()).transpose()?;
            let region = region.map(|value| value.parse::<RegionId>()).transpose()?;
            let channel = region
                .map(|region| ChannelPair::parse(region, channel, sub_channel))
                .transpose()?;
            opts.verbose(format!(
                "Verify path: {:?}, game={:?}, region={:?}, channel={:?}, repair={}, reuse_from={:?}, force_copy={}, relink_reuse={}, skip_vfs={}, skip_local_detect={}",
                path, game, region, channel, repair, reuse_from, force_copy, relink_reuse, skip_vfs, skip_local_detect
            ));
            commands::verify(
                path,
                game,
                region,
                channel,
                overrides,
                skip_local_detect,
                repair,
                reuse_from,
                force_copy,
                relink_reuse,
                skip_vfs,
                opts,
            )
            .await?;
        }
        Commands::SetupVfs {
            path,
            overrides,
            file_set,
            reuse,
            allow_download,
            relink_reuse,
            no_prune,
        } => {
            let PathArg { path } = path;
            let ReuseSourcesArg {
                reuse_from,
                force_copy,
            } = reuse;
            opts.verbose(format!(
                "Setup VFS path={:?}, file_set={:?}, reuse_from={:?}, force_copy={}, allow_download={}, relink_reuse={}, no_prune={}",
                path, file_set, reuse_from, force_copy, allow_download, relink_reuse, no_prune
            ));
            commands::setup_vfs(
                path,
                overrides,
                file_set,
                reuse_from,
                force_copy,
                allow_download,
                relink_reuse,
                !no_prune,
                opts,
            )
            .await?;
        }

        Commands::Info { selector } => {
            opts.verbose("Info query");
            commands::info_show(
                selector.path,
                selector.remote.game.game,
                selector.remote.region.region,
                selector.remote.channel.channel,
                selector.remote.channel.sub_channel,
                &selector.language,
                opts,
            )
            .await?;
        }

        Commands::News {
            remote,
            overrides,
            language,
        } => {
            let (game_id, region_id, channel_id) = parse_remote_args(remote)?;
            opts.verbose(format!("News: {:?} {:?}", game_id, channel_id));
            commands::news_show(game_id, region_id, channel_id, overrides, &language, opts).await?;
        }

        Commands::Debug { command } => debug::dispatch_debug(command, opts).await?,
        Commands::Account { command } => account::dispatch_account(command, opts).await?,
    }

    Ok(())
}
