use std::path::PathBuf;

use anyhow::{Context, Result};
use griffr_hypergryph_api::client::ApiClient;

use serde::Serialize;

use crate::target::{parse_remote_target, RemoteTarget};
use crate::{ui, GlobalOptions, OutputFormat};
use griffr_runtime::{detect_local_install, LocalInstall};

#[derive(Debug, Serialize)]
struct InfoReport {
    local: Option<LocalReport>,
    remote: Option<RemoteReport>,
    media: Option<MediaReport>,
    remote_error: Option<String>,
}

#[derive(Debug, Serialize)]
struct LocalReport {
    path: String,
    backend: String,
    metadata: String,
    appcode: Option<String>,
    region: Option<String>,
    channel: Option<String>,
    sub_channel: Option<String>,
    version: Option<String>,
    entry: Option<String>,
    basis: Option<String>,
    known_game: Option<String>,
    known_region: Option<String>,
    known_channel: Option<String>,
    known_sub_channel: Option<String>,
}

impl LocalReport {
    fn from_install(local: &LocalInstall) -> Self {
        let (backend, metadata, appcode, region, channel, sub_channel, basis) =
            if let Some(config) = local.hypergryph_config() {
                (
                    "hypergryph",
                    config.path.display().to_string(),
                    config.appcode().map(str::to_owned),
                    config.region().map(str::to_owned),
                    config.channel().map(str::to_owned),
                    config.sub_channel().map(str::to_owned),
                    None,
                )
            } else {
                let yostar = local
                    .yostar_metadata()
                    .expect("local metadata backend is known");
                (
                    "yostar",
                    yostar.config_path.display().to_string(),
                    None,
                    Some(yostar.region().to_string()),
                    None,
                    None,
                    Some(yostar.basis().to_string()),
                )
            };
        Self {
            path: local.install_path.display().to_string(),
            backend: backend.to_string(),
            metadata,
            appcode,
            region,
            channel,
            sub_channel,
            version: local.version().map(str::to_owned),
            entry: local.entry().map(str::to_owned),
            basis,
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
            row("backend", &self.backend),
            row("metadata", &self.metadata),
            optional_row("appcode", self.appcode.as_deref()),
            optional_row("region", self.region.as_deref()),
            optional_row("channel", self.channel.as_deref()),
            optional_row("sub_channel", self.sub_channel.as_deref()),
            optional_row("version", self.version.as_deref()),
            optional_row("entry", self.entry.as_deref()),
            optional_row("basis", self.basis.as_deref()),
            optional_row("known_game", self.known_game.as_deref()),
            optional_row("known_region", self.known_region.as_deref()),
            optional_row("known_channel", self.known_channel.as_deref()),
            optional_row("known_sub_channel", self.known_sub_channel.as_deref()),
        ]
    }
}

#[derive(Debug, Serialize)]
struct RemoteReport {
    backend: String,
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
    minimum_version: Option<String>,
    basis: Option<String>,
    files: Option<usize>,
}

impl RemoteReport {
    fn rows(&self) -> Vec<(String, String)> {
        let mut rows = vec![
            row("backend", &self.backend),
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

        if let Some(value) = self.minimum_version.as_deref() {
            rows.push(row("minimum_version", value));
        }
        if let Some(value) = self.basis.as_deref() {
            rows.push(row("basis", value));
        }
        if let Some(value) = self.files {
            rows.push(row("files", value));
        }

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
    remote_state: bool,
    local_only: bool,
    include_media: bool,
    language: &str,
    opts: GlobalOptions,
) -> Result<()> {
    let mut remote_target: Option<RemoteTarget> = None;
    let requested_by_path = path.is_some();
    let local_install = if let Some(path) = path {
        let local = detect_local_install(&path).await?;
        if let (Some(game), Some(region)) = (local.game_id.clone(), local.region_id) {
            remote_target = Some(RemoteTarget::from_detected(
                game,
                region,
                local.channel_id.clone(),
            )?);
        }
        Some(local)
    } else if let (Some(game), Some(region)) = (game, region) {
        remote_target = Some(parse_remote_target(game, region, channel, sub_channel)?);
        None
    } else {
        anyhow::bail!("info requires either --path or both --game and --region");
    };

    let local = local_install.as_ref().map(LocalReport::from_install);
    let should_fetch_remote = !local_only && (!requested_by_path || remote_state || include_media);
    let mut remote = None;
    let mut media = None;
    let mut remote_error = None;

    if should_fetch_remote {
        let remote_target = remote_target.context(
            "Could not determine game/region for remote lookup; provide explicit remote arguments",
        )?;
        match remote_target {
            RemoteTarget::Yostar {
                game: game_id,
                region: region_id,
            } => {
                if include_media {
                    anyhow::bail!("YoStar media/news API support has not been observed and is not exposed by Griffr");
                }
                let client = griffr_yostar_api::YostarApiClient::arknights(region_id)?;
                match client.latest_release().await {
                    Ok(release) => {
                        remote = Some(RemoteReport {
                            backend: "yostar".to_string(),
                            game: game_id.to_string(),
                            region: region_id.to_string(),
                            channel: String::new(),
                            sub_channel: String::new(),
                            version: release.config.game_latest_version.clone(),
                            action: 0,
                            request_version: String::new(),
                            has_full_package: false,
                            has_patch_package: false,
                            package: None,
                            minimum_version: Some(release.config.game_lowest_version),
                            basis: Some(release.config.game_latest_file_path),
                            files: Some(release.manifest.files.len()),
                        });
                    }
                    Err(error) if requested_by_path => {
                        let message = format!(
                        "Failed to fetch matching YoStar remote state; local information is still available: {error}"
                    );
                        ui::print_warning(&message);
                        remote_error = Some(message);
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            RemoteTarget::Hypergryph {
                game: game_id,
                region: region_id,
                channels: channel_id,
            } => {
                let target = griffr_hypergryph_api::resolve_api_target(
                    &game_id,
                    region_id,
                    &channel_id,
                    &Default::default(),
                )?;
                let api_client = ApiClient::new()?;

                match api_client.get_latest_game(&target, None).await {
                    Ok(info) => {
                        let has_full_package = info.has_full_package();
                        let has_patch_package = info.has_patch_package();
                        let package = info.pkg.as_ref().map(|package| PackageReport {
                            file_path: package.file_path.clone(),
                            packs: package.packs.len(),
                            game_files_md5: package.game_files_md5.clone(),
                        });
                        remote = Some(RemoteReport {
                            backend: "hypergryph".to_string(),
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
                            minimum_version: None,
                            basis: None,
                            files: None,
                        });

                        if include_media {
                            let response = api_client
                                .get_media(&target, language)
                                .await
                                .context("Failed to fetch requested remote media summary")?;
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
                    Err(error) if requested_by_path => {
                        let message = format!(
                        "Failed to fetch matching remote state; local information is still available: {error}"
                    );
                        ui::print_warning(&message);
                        remote_error = Some(message);
                    }
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!(
                                "Failed to fetch remote info for {:?} channel={} sub-channel={}",
                                game_id,
                                channel_id.channel(),
                                channel_id.sub_channel()
                            )
                        })
                    }
                }
            }
        }
    }

    let report = InfoReport {
        local,
        remote,
        media,
        remote_error,
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
