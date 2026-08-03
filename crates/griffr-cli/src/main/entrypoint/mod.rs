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
        resource_policy: ResourcePolicyArg::Auto,
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
            resource_policy,
            skip_vfs,
            keep_pack_archives,
        } => {
            let (game_id, region_id, channel_id) = parse_remote_args(remote)?;
            let PathArg { path } = path;
            let ReuseSourcesArg {
                reuse_from,
                force_copy,
            } = reuse;

            let resource_policy = ResourcePolicyArg::resolve(resource_policy, skip_vfs);
            let opts = GlobalOptions {
                resource_policy,
                keep_pack_archives,
                ..opts
            };

            opts.verbose(format!(
                "Install command: game={:?}, region={}, channel={:?}, path={:?}, reuse_from={:?}, force_copy={}, resource_policy={:?}, keep_pack_archives={}",
                game_id, region_id, channel_id, path, reuse_from, force_copy, resource_policy, keep_pack_archives
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
            paths,
            overrides,
            reuse,
            skip_verify,
            full_package,
            use_predownload,
            resource_policy,
            skip_vfs,
            keep_pack_archives,
            work_dir,
            external_asset_root,
        } => {
            let TargetPathsArg { paths } = paths;
            let ReuseSourcesArg {
                reuse_from,
                force_copy,
            } = reuse;
            let resource_policy = ResourcePolicyArg::resolve(resource_policy, skip_vfs);
            let opts = GlobalOptions {
                skip_verify,
                force_full_package: full_package,
                resource_policy,
                keep_pack_archives,
                ..opts
            };
            opts.verbose(format!(
                "Update paths: {:?}, reuse_from={:?}, force_copy={}, use_predownload={}",
                paths, reuse_from, force_copy, use_predownload
            ));
            commands::update(
                paths,
                overrides,
                reuse_from,
                force_copy,
                use_predownload,
                griffr_common::runtime::PatchApplyOptions {
                    work_dir,
                    external_asset_root,
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
                resource_policy,
                skip_vfs,
                keep_pack_archives,
                work_dir,
                external_asset_root,
            } => {
                let PathArg { path } = path;
                let resource_policy = ResourcePolicyArg::resolve(resource_policy, skip_vfs);
                let opts = GlobalOptions {
                    skip_verify,
                    resource_policy,
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
                        external_asset_root,
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

        Commands::Launch {
            path,
            force,
            wine,
            wine_prefix,
        } => {
            opts.verbose(format!(
                "Launch path: {:?}, force={}, wine={:?}, wine_prefix={:?}",
                path, force, wine, wine_prefix
            ));
            commands::launch(path, force, wine, wine_prefix, opts).await?;
        }

        Commands::Verify {
            paths,
            remote,
            overrides,
            repair,
            reuse,
            relink_reuse,
            scope,
            skip_vfs,
            skip_local_detect,
        } => {
            let TargetPathsArg { paths } = paths;
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
            let scope = if skip_vfs {
                Some(VerifyScopeArg::Core)
            } else {
                scope
            };
            opts.verbose(format!(
                "Verify paths: {:?}, game={:?}, region={:?}, channel={:?}, repair={}, reuse_from={:?}, force_copy={}, relink_reuse={}, scope={:?}, skip_local_detect={}",
                paths, game, region, channel, repair, reuse_from, force_copy, relink_reuse, scope, skip_local_detect
            ));
            commands::verify(
                paths,
                game,
                region,
                channel,
                overrides,
                skip_local_detect,
                repair,
                reuse_from,
                force_copy,
                relink_reuse,
                scope,
                opts,
            )
            .await?;
        }
        Commands::SetupPersistentResources {
            path,
            overrides,
            file_set,
            reuse_from,
            allow_download,
            prefer_reuse,
            prune,
        } => {
            let PathArg { path } = path;
            opts.verbose(format!(
                "Setup Persistent resources path={:?}, file_set={:?}, reuse_from={:?}, allow_download={}, prefer_reuse={}, prune={}",
                path, file_set, reuse_from, allow_download, prefer_reuse, prune
            ));
            commands::setup_persistent_resources(
                path,
                overrides,
                file_set,
                reuse_from,
                allow_download,
                prefer_reuse,
                prune,
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
