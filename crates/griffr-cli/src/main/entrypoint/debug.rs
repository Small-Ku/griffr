use super::parse_remote_args;
use crate::debug_cli::DebugCommands;
use crate::{commands, GlobalOptions};
use anyhow::Result;

pub(super) async fn dispatch_debug(command: DebugCommands, opts: GlobalOptions) -> Result<()> {
    match command {
        DebugCommands::DetectConfigIni { path } => commands::debug_detect(path, opts).await?,
        DebugCommands::DecryptConfigIni { path } => commands::debug_config_ini(path, opts).await?,
        DebugCommands::DecryptGameFiles { path } => commands::debug_game_files(path, opts).await?,
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
            let (game_id, region_id, channel_id) = parse_remote_args(remote)?;
            commands::debug_api_get_latest_game(
                game_id, region_id, channel_id, overrides, version, output, opts,
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
            let (game_id, region_id, channel_id) = parse_remote_args(remote)?;
            commands::debug_api_get_latest_resources(
                game_id,
                region_id,
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
            let (game_id, region_id, channel_id) = parse_remote_args(remote)?;
            commands::debug_fetch_game_files(
                game_id, region_id, channel_id, overrides, version, output, opts,
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
            let (game_id, region_id, channel_id) = parse_remote_args(remote)?;
            commands::debug_list_resource_files(
                game_id,
                region_id,
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
            let (game_id, region_id, channel_id) = parse_remote_args(remote)?;
            commands::debug_fetch_file(
                game_id, region_id, channel_id, overrides, version, file, output, opts,
            )
            .await?;
        }
        DebugCommands::GetRawMedia {
            remote,
            overrides,
            language,
            output,
        } => {
            let (game_id, region_id, channel_id) = parse_remote_args(remote)?;
            commands::debug_api_get_media(
                game_id, region_id, channel_id, overrides, language, output, opts,
            )
            .await?;
        }
        DebugCommands::GetMedia {
            remote,
            overrides,
            language,
            output,
        } => {
            let (game_id, region_id, channel_id) = parse_remote_args(remote)?;
            commands::debug_fetch_media(
                game_id, region_id, channel_id, overrides, language, output, opts,
            )
            .await?;
        }
    }
    Ok(())
}
