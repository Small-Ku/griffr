use std::path::PathBuf;

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::config::{GameId, ServerId};

use super::local::detect_local_install;
use crate::GlobalOptions;

pub async fn show(
    path: Option<PathBuf>,
    game: Option<String>,
    server: Option<String>,
    language: &str,
    opts: GlobalOptions,
) -> Result<()> {
    let api_client = ApiClient::new()?;

    let mut remote_target: Option<(GameId, ServerId)> = None;

    if let Some(path) = path {
        let local = detect_local_install(&path).await?;
        println!("path={}", local.install_path.display());
        println!("config_ini={}", local.config_ini.path.display());
        println!("appcode={}", local.config_ini.appcode().unwrap_or(""));
        println!("region={}", local.config_ini.region().unwrap_or(""));
        println!("channel={}", local.config_ini.channel().unwrap_or(""));
        println!(
            "sub_channel={}",
            local.config_ini.sub_channel().unwrap_or("")
        );
        println!("version={}", local.config_ini.version().unwrap_or(""));
        println!("entry={}", local.config_ini.entry().unwrap_or(""));
        println!("known_game={:?}", local.game_id);
        println!("known_server={:?}", local.server_id);

        if let (Some(game_id), Some(server_id)) = (local.game_id, local.server_id) {
            remote_target = Some((game_id, server_id));
        }
    } else if let (Some(game), Some(server)) = (game, server) {
        remote_target = Some((game.parse::<GameId>()?, server.parse::<ServerId>()?));
    } else {
        anyhow::bail!("info requires either --path or both --game and --server");
    }

    if let Some((game_id, server_id)) = remote_target {
        let info = api_client
            .get_latest_game(game_id, server_id, None)
            .await
            .with_context(|| {
                format!(
                    "Failed to fetch remote info for {:?} {}",
                    game_id, server_id
                )
            })?;
        println!("remote.game={:?}", game_id);
        println!("remote.server={}", server_id);
        println!("remote.version={}", info.version);
        println!("remote.action={}", info.action);
        println!("remote.request_version={}", info.request_version);
        println!("remote.has_full_package={}", info.has_full_package());
        println!("remote.has_patch_package={}", info.has_patch_package());
        if let Some(pkg) = info.pkg {
            println!("remote.pkg.file_path={}", pkg.file_path);
            println!("remote.pkg.packs={}", pkg.packs.len());
            println!(
                "remote.pkg.game_files_md5={}",
                pkg.game_files_md5.unwrap_or_default()
            );
        }

        if opts.verbose {
            let media = api_client.get_media(game_id, server_id, language).await?;
            println!("remote.media.language={}", language);
            println!(
                "remote.media.banners={}",
                media.banners.map(|v| v.banners.len()).unwrap_or(0)
            );
            println!(
                "remote.media.announcement_tabs={}",
                media.announcements.map(|v| v.tabs.len()).unwrap_or(0)
            );
            println!(
                "remote.media.sidebar={}",
                media.sidebar.map(|v| v.sidebars.len()).unwrap_or(0)
            );
        }
    }

    Ok(())
}
