use std::path::PathBuf;

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::config::{ChannelPair, GameId};
use serde_json::json;

use super::local::{detect_local_install, LocalInstall};
use crate::{ui, GlobalOptions, OutputFormat};

fn local_rows(local: &LocalInstall) -> Vec<(String, String)> {
    vec![
        ("path".to_string(), local.install_path.display().to_string()),
        (
            "config_ini".to_string(),
            local.config_ini.path.display().to_string(),
        ),
        (
            "appcode".to_string(),
            local.config_ini.appcode().unwrap_or("").to_string(),
        ),
        (
            "region".to_string(),
            local.config_ini.region().unwrap_or("").to_string(),
        ),
        (
            "channel".to_string(),
            local.config_ini.channel().unwrap_or("").to_string(),
        ),
        (
            "sub_channel".to_string(),
            local.config_ini.sub_channel().unwrap_or("").to_string(),
        ),
        (
            "version".to_string(),
            local.config_ini.version().unwrap_or("").to_string(),
        ),
        (
            "entry".to_string(),
            local.config_ini.entry().unwrap_or("").to_string(),
        ),
        ("known_game".to_string(), format!("{:?}", local.game_id)),
        (
            "known_channel".to_string(),
            format!("{:?}", local.channel_id),
        ),
    ]
}

pub async fn show(
    path: Option<PathBuf>,
    game: Option<String>,
    channel: Option<String>,
    sub_channel: Option<String>,
    language: &str,
    opts: GlobalOptions,
) -> Result<()> {
    let api_client = ApiClient::new()?;

    let mut remote_target: Option<(GameId, ChannelPair)> = None;
    let mut local_install: Option<LocalInstall> = None;

    if let Some(path) = path {
        let local = detect_local_install(&path).await?;
        if let (Some(game_id), Some(channel_id)) = (local.game_id.clone(), local.channel_id.clone())
        {
            remote_target = Some((game_id, channel_id));
        }
        local_install = Some(local);
    } else if let (Some(game), Some(channel)) = (game, channel) {
        let channel_id = ChannelPair::parse(channel, sub_channel)?;
        remote_target = Some((game.parse::<GameId>()?, channel_id));
    } else {
        anyhow::bail!("info requires either --path or both --game and --channel");
    }

    let mut remote_json = None;
    let mut media_json = None;

    if let Some((game_id, channel_id)) = remote_target {
        let target =
            griffr_common::config::resolve_api_target(&game_id, &channel_id, &Default::default())?;

        let info = api_client
            .get_latest_game(&target, None)
            .await
            .with_context(|| {
                format!(
                    "Failed to fetch remote info for {:?} channel={} sub-channel={}",
                    game_id,
                    channel_id.channel(),
                    channel_id.sub_channel()
                )
            })?;

        remote_json = Some(json!({
            "game": game_id.to_string(),
            "channel": channel_id.channel().as_str(),
            "sub_channel": channel_id.sub_channel().as_str(),
            "version": info.version,
            "action": info.action,
            "request_version": info.request_version,
            "has_full_package": info.has_full_package(),
            "has_patch_package": info.has_patch_package(),
            "package": info.pkg.as_ref().map(|pkg| {
                json!({
                    "file_path": pkg.file_path,
                    "packs": pkg.packs.len(),
                    "game_files_md5": pkg.game_files_md5
                })
            })
        }));

        if opts.verbose {
            let media = api_client.get_media(&target, language).await?;
            media_json = Some(json!({
                "language": language,
                "banners": media.banners.as_ref().map(|v| v.banners.len()).unwrap_or(0),
                "announcement_tabs": media.announcements.as_ref().map(|v| v.tabs.len()).unwrap_or(0),
                "sidebar": media.sidebar.as_ref().map(|v| v.sidebars.len()).unwrap_or(0),
            }));
        }
    }

    if opts.output == OutputFormat::Json {
        let local_json = local_install.as_ref().map(|local| {
            json!({
                "path": local.install_path.display().to_string(),
                "config_ini": local.config_ini.path.display().to_string(),
                "appcode": local.config_ini.appcode(),
                "region": local.config_ini.region(),
                "channel": local.config_ini.channel(),
                "sub_channel": local.config_ini.sub_channel(),
                "version": local.config_ini.version(),
                "entry": local.config_ini.entry(),
                "known_game": local.game_id.as_ref().map(|g| g.to_string()),
                "known_channel": local.channel_id.as_ref().map(|s| s.channel().to_string()),
                "known_sub_channel": local.channel_id.as_ref().map(|s| s.sub_channel().to_string()),
            })
        });
        return ui::emit_json(&json!({
            "local": local_json,
            "remote": remote_json,
            "media": media_json,
        }));
    }

    if let Some(local) = local_install.as_ref() {
        ui::print_kv_section("Local Install", &local_rows(local));
    }

    if let Some(remote) = remote_json {
        let mut rows = vec![
            (
                "game".to_string(),
                remote
                    .get("game")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ),
            (
                "channel".to_string(),
                remote
                    .get("channel")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ),
            (
                "sub_channel".to_string(),
                remote
                    .get("sub_channel")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ),
            (
                "version".to_string(),
                remote
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ),
            (
                "action".to_string(),
                remote
                    .get("action")
                    .and_then(|v| v.as_i64())
                    .unwrap_or_default()
                    .to_string(),
            ),
            (
                "request_version".to_string(),
                remote
                    .get("request_version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ),
            (
                "has_full_package".to_string(),
                remote
                    .get("has_full_package")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                    .to_string(),
            ),
            (
                "has_patch_package".to_string(),
                remote
                    .get("has_patch_package")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                    .to_string(),
            ),
        ];

        if let Some(pkg) = remote.get("package") {
            rows.push((
                "pkg.file_path".to_string(),
                pkg.get("file_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ));
            rows.push((
                "pkg.packs".to_string(),
                pkg.get("packs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or_default()
                    .to_string(),
            ));
            rows.push((
                "pkg.game_files_md5".to_string(),
                pkg.get("game_files_md5")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ));
        }

        if local_install.is_some() {
            println!();
        }
        ui::print_kv_section("Remote State", &rows);
    }

    if let Some(media) = media_json {
        println!();
        let rows = vec![
            (
                "language".to_string(),
                media
                    .get("language")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ),
            (
                "banners".to_string(),
                media
                    .get("banners")
                    .and_then(|v| v.as_u64())
                    .unwrap_or_default()
                    .to_string(),
            ),
            (
                "announcement_tabs".to_string(),
                media
                    .get("announcement_tabs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or_default()
                    .to_string(),
            ),
            (
                "sidebar".to_string(),
                media
                    .get("sidebar")
                    .and_then(|v| v.as_u64())
                    .unwrap_or_default()
                    .to_string(),
            ),
        ];
        ui::print_kv_section("Remote Media", &rows);
    }

    Ok(())
}
