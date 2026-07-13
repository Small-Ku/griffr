use std::path::PathBuf;

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::config::{ChannelPair, GameId, RegionId};
use serde::Serialize;

use super::local::{detect_local_install, LocalInstall};
use crate::{ui, GlobalOptions, OutputFormat};

#[derive(Debug, Serialize)]
struct InfoReport {
    local: Option<LocalReport>,
    remote: Option<RemoteReport>,
    media: Option<MediaReport>,
}

#[derive(Debug, Serialize)]
struct LocalReport {
    path: String,
    config_ini: String,
    appcode: Option<String>,
    region: Option<String>,
    channel: Option<String>,
    sub_channel: Option<String>,
    version: Option<String>,
    entry: Option<String>,
    known_game: Option<String>,
    known_region: Option<String>,
    known_channel: Option<String>,
    known_sub_channel: Option<String>,
}

impl LocalReport {
    fn from_install(local: &LocalInstall) -> Self {
        Self {
            path: local.install_path.display().to_string(),
            config_ini: local.config_ini.path.display().to_string(),
            appcode: local.config_ini.appcode().map(str::to_owned),
            region: local.config_ini.region().map(str::to_owned),
            channel: local.config_ini.channel().map(str::to_owned),
            sub_channel: local.config_ini.sub_channel().map(str::to_owned),
            version: local.config_ini.version().map(str::to_owned),
            entry: local.config_ini.entry().map(str::to_owned),
            known_game: local.game_id.as_ref().map(ToString::to_string),
            known_region: local.region_id.map(|region| region.to_string()),
            known_channel: local
                .channel_id
                .as_ref()
                .map(|channels| channels.channel().to_string()),
            known_sub_channel: local
                .channel_id
                .as_ref()
                .map(|channels| channels.sub_channel().to_string()),
        }
    }

    fn rows(&self) -> Vec<(String, String)> {
        vec![
            row("path", &self.path),
            row("config_ini", &self.config_ini),
            optional_row("appcode", self.appcode.as_deref()),
            optional_row("region", self.region.as_deref()),
            optional_row("channel", self.channel.as_deref()),
            optional_row("sub_channel", self.sub_channel.as_deref()),
            optional_row("version", self.version.as_deref()),
            optional_row("entry", self.entry.as_deref()),
            optional_row("known_game", self.known_game.as_deref()),
            optional_row("known_region", self.known_region.as_deref()),
            optional_row("known_channel", self.known_channel.as_deref()),
            optional_row("known_sub_channel", self.known_sub_channel.as_deref()),
        ]
    }
}

#[derive(Debug, Serialize)]
struct RemoteReport {
    game: String,
    region: String,
    channel: String,
    sub_channel: String,
    version: String,
    action: i32,
    request_version: String,
    has_full_package: bool,
    has_patch_package: bool,
    package: Option<PackageReport>,
}

impl RemoteReport {
    fn rows(&self) -> Vec<(String, String)> {
        let mut rows = vec![
            row("game", &self.game),
            row("region", &self.region),
            row("channel", &self.channel),
            row("sub_channel", &self.sub_channel),
            row("version", &self.version),
            row("action", self.action),
            row("request_version", &self.request_version),
            row("has_full_package", self.has_full_package),
            row("has_patch_package", self.has_patch_package),
        ];

        if let Some(package) = &self.package {
            rows.extend([
                row("pkg.file_path", &package.file_path),
                row("pkg.packs", package.packs),
                optional_row("pkg.game_files_md5", package.game_files_md5.as_deref()),
            ]);
        }

        rows
    }
}

#[derive(Debug, Serialize)]
struct PackageReport {
    file_path: String,
    packs: usize,
    game_files_md5: Option<String>,
}

#[derive(Debug, Serialize)]
struct MediaReport {
    language: String,
    banners: usize,
    announcement_tabs: usize,
    sidebar: usize,
}

impl MediaReport {
    fn rows(&self) -> Vec<(String, String)> {
        vec![
            row("language", &self.language),
            row("banners", self.banners),
            row("announcement_tabs", self.announcement_tabs),
            row("sidebar", self.sidebar),
        ]
    }
}

fn row(key: &str, value: impl ToString) -> (String, String) {
    (key.to_owned(), value.to_string())
}

fn optional_row(key: &str, value: Option<&str>) -> (String, String) {
    row(key, value.unwrap_or_default())
}

pub async fn show(
    path: Option<PathBuf>,
    game: Option<String>,
    region: Option<String>,
    channel: Option<String>,
    sub_channel: Option<String>,
    language: &str,
    opts: GlobalOptions,
) -> Result<()> {
    let api_client = ApiClient::new()?;

    let mut remote_target: Option<(GameId, RegionId, ChannelPair)> = None;
    let local_install = if let Some(path) = path {
        let local = detect_local_install(&path).await?;
        if let (Some(game_id), Some(region_id), Some(channel_id)) = (
            local.game_id.clone(),
            local.region_id,
            local.channel_id.clone(),
        ) {
            remote_target = Some((game_id, region_id, channel_id));
        }
        Some(local)
    } else if let (Some(game), Some(region)) = (game, region) {
        let region = region.parse::<RegionId>()?;
        remote_target = Some((
            game.parse::<GameId>()?,
            region,
            ChannelPair::parse(region, channel, sub_channel)?,
        ));
        None
    } else {
        anyhow::bail!("info requires either --path or both --game and --region");
    };

    let local = local_install.as_ref().map(LocalReport::from_install);
    let mut remote = None;
    let mut media = None;

    if let Some((game_id, region_id, channel_id)) = remote_target {
        let target = griffr_common::config::resolve_api_target(
            &game_id,
            region_id,
            &channel_id,
            &Default::default(),
        )?;

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

        let has_full_package = info.has_full_package();
        let has_patch_package = info.has_patch_package();
        let package = info.pkg.as_ref().map(|package| PackageReport {
            file_path: package.file_path.clone(),
            packs: package.packs.len(),
            game_files_md5: package.game_files_md5.clone(),
        });
        remote = Some(RemoteReport {
            game: game_id.to_string(),
            region: region_id.to_string(),
            channel: channel_id.channel().to_string(),
            sub_channel: channel_id.sub_channel().to_string(),
            version: info.version,
            action: info.action,
            request_version: info.request_version,
            has_full_package,
            has_patch_package,
            package,
        });

        if opts.verbose {
            let response = api_client.get_media(&target, language).await?;
            media = Some(MediaReport {
                language: language.to_owned(),
                banners: response
                    .banners
                    .as_ref()
                    .map(|value| value.banners.len())
                    .unwrap_or_default(),
                announcement_tabs: response
                    .announcements
                    .as_ref()
                    .map(|value| value.tabs.len())
                    .unwrap_or_default(),
                sidebar: response
                    .sidebar
                    .as_ref()
                    .map(|value| value.sidebars.len())
                    .unwrap_or_default(),
            });
        }
    }

    let report = InfoReport {
        local,
        remote,
        media,
    };

    if opts.output == OutputFormat::Json {
        return ui::emit_json(&report);
    }

    if let Some(local) = &report.local {
        ui::print_kv_section("Local Install", &local.rows());
    }

    if let Some(remote) = &report.remote {
        if report.local.is_some() {
            println!();
        }
        ui::print_kv_section("Remote State", &remote.rows());
    }

    if let Some(media) = &report.media {
        println!();
        ui::print_kv_section("Remote Media", &media.rows());
    }

    Ok(())
}
