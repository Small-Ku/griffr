use crate::cli::*;
use crate::debug_cli::*;
use crate::{commands, GlobalOptions};
use anyhow::Result;
use clap::Parser;
use griffr_common::config::{ChannelId, ChannelPair, GameId};
use tracing::debug;

fn parse_remote_args(remote: RequiredGameChannelArgs) -> Result<(GameId, ChannelPair)> {
    let (game, channel, sub_channel) = remote.into_parts();
    Ok((game.parse::<GameId>()?, ChannelPair::parse(channel, sub_channel)?))
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
            let (game_id, channel_id) = parse_remote_args(remote)?;
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
                "Install command: game={:?}, channel={:?}, path={:?}, reuse_from={:?}, force_copy={}, skip_vfs={}, keep_pack_archives={}",
                game_id, channel_id, path, reuse_from, force_copy, skip_vfs, keep_pack_archives
            ));
            commands::install(
                game_id, channel_id, overrides, path, force, reuse_from, force_copy, opts,
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
                commands::predownload_apply(path, overrides, output_dir, opts).await?;
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
            let GameChannelArgs {
                game: GameArg { game },
                channel: ChannelArg { channel, sub_channel },
            } = remote;
            let game = game.map(|value| value.parse::<GameId>()).transpose()?;
            let channel = channel
                .map(|value| ChannelPair::parse(value, sub_channel))
                .transpose()?;
            opts.verbose(format!(
                "Verify path: {:?}, game={:?}, channel={:?}, repair={}, reuse_from={:?}, force_copy={}, relink_reuse={}, skip_vfs={}, skip_local_detect={}",
                path, game, channel, repair, reuse_from, force_copy, relink_reuse, skip_vfs, skip_local_detect
            ));
            commands::verify(
                path,
                game,
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
        Commands::Bootstrap {
            path,
            overrides,
            scope,
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
                "Bootstrap path={:?}, scope={:?}, reuse_from={:?}, force_copy={}, allow_download={}, relink_reuse={}, no_prune={}",
                path, scope, reuse_from, force_copy, allow_download, relink_reuse, no_prune
            ));
            commands::bootstrap(
                path,
                overrides,
                scope,
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
                selector.game_channel.game.game,
                selector.game_channel.channel.channel,
                selector.game_channel.channel.sub_channel,
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
            let (game_id, channel_id) = parse_remote_args(remote)?;
            opts.verbose(format!("News: {:?} {:?}", game_id, channel_id));
            commands::news_show(game_id, channel_id, overrides, &language, opts).await?;
        }

        Commands::Debug { command } => match command {
            DebugCommands::DetectConfigIni { path } => commands::debug_detect(path, opts).await?,
            DebugCommands::DecryptConfigIni { path } => {
                commands::debug_config_ini(path, opts).await?
            }
            DebugCommands::DecryptGameFiles { path } => {
                commands::debug_game_files(path, opts).await?
            }
            DebugCommands::DecryptResIndex { path, key } => {
                commands::debug_res_index(path, key, opts).await?
            }
            DebugCommands::VfsDiff {
                path,
                against,
                key,
                show_limit,
            } => commands::debug_vfs_diff(path, against, key, show_limit, opts).await?,
            DebugCommands::SnapshotResourceState {
                path,
                output,
                hash_check,
            } => commands::debug_snapshot_resource_state(path, output, hash_check, opts).await?,
            DebugCommands::DiffResourceSnapshots {
                before,
                after,
                show_limit,
            } => commands::debug_diff_resource_snapshots(before, after, show_limit, opts).await?,
            DebugCommands::GetRawLatestGame {
                remote,
                overrides,
                version,
                output,
            } => {
                let (game_id, channel_id) = parse_remote_args(remote)?;
                commands::debug_api_get_latest_game(
                    game_id, channel_id, overrides, version, output, opts,
                )
                .await?;
            }
            DebugCommands::GetRawLatestResources {
                remote,
                overrides,
                version,
                resource_version,
                rand_str,
                platform,
                output,
            } => {
                let (game_id, channel_id) = parse_remote_args(remote)?;
                commands::debug_api_get_latest_resources(
                    game_id,
                    channel_id,
                    overrides,
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
                overrides,
                version,
                output,
            } => {
                let (game_id, channel_id) = parse_remote_args(remote)?;
                commands::debug_fetch_game_files(
                    game_id, channel_id, overrides, version, output, opts,
                )
                .await?;
            }
            DebugCommands::ListResourceFiles {
                remote,
                overrides,
                version,
                resource_version,
                rand_str,
                platform,
                output,
            } => {
                let (game_id, channel_id) = parse_remote_args(remote)?;
                commands::debug_list_resource_files(
                    game_id,
                    channel_id,
                    overrides,
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
                overrides,
                version,
                file,
                output,
            } => {
                let (game_id, channel_id) = parse_remote_args(remote)?;
                commands::debug_fetch_file(
                    game_id, channel_id, overrides, version, file, output, opts,
                )
                .await?;
            }
            DebugCommands::GetRawMedia {
                remote,
                overrides,
                language,
                output,
            } => {
                let (game_id, channel_id) = parse_remote_args(remote)?;
                commands::debug_api_get_media(
                    game_id, channel_id, overrides, language, output, opts,
                )
                .await?;
            }
            DebugCommands::GetMedia {
                remote,
                overrides,
                language,
                output,
            } => {
                let (game_id, channel_id) = parse_remote_args(remote)?;
                commands::debug_fetch_media(game_id, channel_id, overrides, language, output, opts)
                    .await?;
            }
        },
        Commands::Account { command } => match command {
            AccountCommands::Capture {
                game,
                channel_hint,
                bundle,
                sdk_dir,
                install_path,
                include_install_mmkv,
                force,
            } => {
                let game_id = game.parse::<GameId>()?;
                let channel_hint = channel_hint.map(|s| s.parse::<ChannelId>()).transpose()?;
                commands::account_capture(
                    game_id,
                    channel_hint,
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
                channel_hint,
                bundle,
                sdk_dir,
                install_path,
                include_install_mmkv,
                force,
            } => {
                let game_id = game.parse::<GameId>()?;
                let channel_hint = channel_hint.map(|s| s.parse::<ChannelId>()).transpose()?;
                commands::account_activate(
                    game_id,
                    channel_hint,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_args_normalize_channel_fields_independently() {
        let remote = RequiredGameChannelArgs {
            game: "endfield".to_string(),
            channel: "hypergryph".to_string(),
            sub_channel: Some("google_play".to_string()),
        };

        let (game, channel) = parse_remote_args(remote).unwrap();
        assert_eq!(game, GameId::ENDFIELD);
        assert_eq!(channel.channel().as_str(), "1");
        assert_eq!(channel.sub_channel().as_str(), "802");
    }

    #[test]
    fn remote_args_default_sub_channel_to_channel() {
        let remote = RequiredGameChannelArgs {
            game: "endfield".to_string(),
            channel: "epic_store".to_string(),
            sub_channel: None,
        };

        let (_, channel) = parse_remote_args(remote).unwrap();
        assert_eq!(channel.channel().as_str(), "801");
        assert_eq!(channel.sub_channel().as_str(), "801");
    }
}
