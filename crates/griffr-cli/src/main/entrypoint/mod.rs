use crate::cli::*;
use crate::debug_cli::*;
use crate::target::RemoteTarget;
use crate::{commands, GlobalOptions};
use anyhow::Result;
use clap::Parser;
use griffr_core::{ChannelPair, GameId, RegionId};
use tracing::debug;

mod account;
mod debug;

#[cfg(test)]
mod tests;

fn parse_remote_args(remote: RequiredGameRegionChannelArgs) -> Result<RemoteTarget> {
    let (game, region, channel, sub_channel) = remote.into_parts();
    RemoteTarget::parse(game, region, channel, sub_channel)
}

fn parse_hypergryph_remote_args(
    remote: RequiredGameRegionChannelArgs,
) -> Result<(GameId, RegionId, ChannelPair)> {
    match parse_remote_args(remote)? {
        RemoteTarget::Hypergryph {
            game,
            region,
            channels,
        } => Ok((game, region, channels)),
        RemoteTarget::Yostar { region, .. } => anyhow::bail!(
            "This command uses the Hypergryph/Gryphline API and is unavailable for YoStar {region}"
        ),
    }
}

async fn dispatch_resource_sync(args: PersistentResourceArgs, opts: GlobalOptions) -> Result<()> {
    let PersistentResourceArgs {
        mutation,
        path: PathArg { path },
        overrides,
        file_set,
        reuse_from,
        allow_download,
        prefer_reuse,
        prune,
    } = args;
    let opts = opts.with_dry_run(mutation.dry_run);
    opts.verbose(format!(
        "Resource sync path={:?}, file_set={:?}, reuse_from={:?}, allow_download={}, prefer_reuse={}, prune={}",
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
    .await
}

pub(crate) async fn run() -> Result<()> {
    let cli = std::thread::Builder::new()
        .name("cli-parse".to_string())
        .stack_size(8 * 1024 * 1024)
        .spawn(Cli::parse)
        .map_err(|e| anyhow::anyhow!("failed to spawn cli parser thread: {e}"))?
        .join()
        .map_err(|_| anyhow::anyhow!("cli parser thread panicked"))?;

    crate::ui::set_quiet(cli.quiet);
    let default_level = if cli.verbose {
        "warn,griffr=debug,griffr_core=debug,griffr_hypergryph_api=debug,griffr_yostar_api=debug,griffr_runtime=debug"
    } else if cli.quiet {
        "error"
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

    let opts = GlobalOptions::from_environment(false, cli.verbose, OutputFormat::Text);

    if opts.verbose {
        debug!("Griffr CLI started");
        debug!("Verbose: {}", opts.verbose);
    }

    match *cli.command {
        Commands::Install {
            mutation,
            remote,
            overrides,
            path,
            force,
            reuse,
            resource_policy,
            skip_vfs,
            keep_pack_archives,
        } => {
            let opts = opts.with_dry_run(mutation.dry_run);
            let target = parse_remote_args(remote)?;
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
                "Install command: target={:?}, path={:?}, reuse_from={:?}, force_copy={}, resource_policy={:?}, keep_pack_archives={}",
                target, path, reuse_from, force_copy, resource_policy, keep_pack_archives
            ));
            commands::install(target, overrides, path, force, reuse_from, force_copy, opts).await?;
        }

        Commands::Uninstall {
            mutation,
            path,
            detach,
            yes,
        } => {
            let opts = opts.with_dry_run(mutation.dry_run);
            opts.verbose(format!(
                "Uninstall command: path={:?}, detach={}, yes={}",
                path, detach, yes
            ));
            commands::uninstall(path, detach, yes, opts).await?;
        }

        Commands::Update {
            mutation,
            batch,
            paths,
            overrides,
            reuse,
            defer_verification,
            full_package,
            stage_dir,
            require_staged,
            use_predownload,
            resource_policy,
            skip_vfs,
            keep_pack_archives,
            work_dir,
            external_asset_root,
        } => {
            let opts = opts.with_dry_run(mutation.dry_run);
            let TargetPathsArg { paths } = paths;
            let ReuseSourcesArg {
                reuse_from,
                force_copy,
            } = reuse;
            let resource_policy = ResourcePolicyArg::resolve(resource_policy, skip_vfs);
            let opts = GlobalOptions {
                skip_verify: defer_verification,
                force_full_package: full_package,
                resource_policy,
                keep_pack_archives,
                ..opts
            };
            let use_default_stage = use_predownload && stage_dir.is_none();
            opts.verbose(format!(
                "Update paths: {:?}, reuse_from={:?}, force_copy={}, stage_dir={:?}, require_staged={}, use_default_stage={}",
                paths, reuse_from, force_copy, stage_dir, require_staged, use_default_stage
            ));
            commands::update(
                paths,
                overrides,
                reuse_from,
                force_copy,
                stage_dir,
                require_staged,
                use_default_stage,
                batch,
                griffr_runtime::PatchApplyOptions {
                    work_dir,
                    external_asset_root,
                },
                opts,
            )
            .await?;
        }

        Commands::Stage { command } => match command {
            StageCommands::Inspect { path, overrides } => {
                let PathArg { path } = path;
                opts.verbose(format!("Predownload check path: {:?}", path));
                commands::predownload_check(path, overrides, opts).await?;
            }
            StageCommands::Fetch {
                mutation,
                path,
                overrides,
                stage_dir,
            } => {
                let opts = opts.with_dry_run(mutation.dry_run);
                let PathArg { path } = path;
                opts.verbose(format!(
                    "Stage fetch path: {:?}, stage_dir={:?}",
                    path, stage_dir
                ));
                commands::predownload_fetch(path, overrides, stage_dir, opts).await?;
            }
            StageCommands::Apply {
                mutation,
                path,
                overrides,
                stage_dir,
                defer_verification,
                resource_policy,
                skip_vfs,
                keep_pack_archives,
                work_dir,
                external_asset_root,
            } => {
                let opts = opts.with_dry_run(mutation.dry_run);
                let PathArg { path } = path;
                let resource_policy = ResourcePolicyArg::resolve(resource_policy, skip_vfs);
                let opts = GlobalOptions {
                    skip_verify: defer_verification,
                    resource_policy,
                    keep_pack_archives,
                    ..opts
                };
                opts.verbose(format!(
                    "Legacy stage apply path: {:?}, stage_dir={:?}",
                    path, stage_dir
                ));
                commands::predownload_apply(
                    path,
                    overrides,
                    stage_dir,
                    griffr_runtime::PatchApplyOptions {
                        work_dir,
                        external_asset_root,
                    },
                    opts,
                )
                .await?;
            }
            StageCommands::Resume { mutation, path } => {
                let opts = opts.with_dry_run(mutation.dry_run);
                let PathArg { path } = path;
                opts.verbose(format!("Predownload resume path: {:?}", path));
                commands::predownload_resume(path, opts).await?;
            }
        },

        Commands::Recover { mutation, path } => {
            let opts = opts.with_dry_run(mutation.dry_run);
            let PathArg { path } = path;
            opts.verbose(format!("Recover path: {:?}", path));
            commands::predownload_resume(path, opts).await?;
        }

        Commands::Launch {
            mutation,
            path,
            force,
            wine,
            wine_prefix,
        } => {
            let opts = opts.with_dry_run(mutation.dry_run);
            opts.verbose(format!(
                "Launch path: {:?}, force={}, wine={:?}, wine_prefix={:?}",
                path, force, wine, wine_prefix
            ));
            commands::launch(path, force, wine, wine_prefix, opts).await?;
        }

        Commands::Verify {
            mutation,
            batch,
            paths,
            remote,
            overrides,
            repair,
            reuse,
            relink_reuse,
            scope,
            skip_vfs,
            skip_local_detect,
            report,
        } => {
            let opts = opts
                .with_dry_run(mutation.dry_run)
                .with_output(report.output);
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
                batch,
                opts,
            )
            .await?;
        }
        Commands::Resources { command } => match command {
            ResourceCommands::Sync { args } => {
                dispatch_resource_sync(args, opts).await?;
            }
        },
        Commands::SetupPersistentResources { args } => {
            dispatch_resource_sync(args, opts).await?;
        }

        Commands::Info { selector } => {
            let opts = opts.with_output(selector.report.output);
            opts.verbose("Info query");
            commands::info_show(
                selector.path,
                selector.remote.game.game,
                selector.remote.region.region,
                selector.remote.channel.channel,
                selector.remote.channel.sub_channel,
                selector.remote_state,
                selector.local_only,
                selector.include_media,
                &selector.language,
                opts,
            )
            .await?;
        }

        Commands::News {
            remote,
            overrides,
            language,
            include_links,
            report,
        } => {
            let opts = opts.with_output(report.output);
            let (game_id, region_id, channel_id) = parse_hypergryph_remote_args(remote)?;
            opts.verbose(format!("News: {:?} {:?}", game_id, channel_id));
            commands::news_show(
                game_id,
                region_id,
                channel_id,
                overrides,
                &language,
                include_links,
                opts,
            )
            .await?;
        }

        Commands::Debug { command } => debug::dispatch_debug(command, opts).await?,
        Commands::Account { command } => account::dispatch_account(command, opts).await?,
    }

    Ok(())
}
