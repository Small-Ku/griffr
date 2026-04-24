use std::path::PathBuf;

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::api::crypto;
use griffr_common::config::{GameId, ServerId};
use serde_json::{json, Value};

use super::local::{decrypt_config_ini, detect_local_install, resolve_named_path};
use crate::GlobalOptions;

async fn emit_json(output: Option<PathBuf>, payload: Value) -> Result<()> {
    let body = serde_json::to_vec_pretty(&payload)?;
    if let Some(output) = output {
        let write_result = compio::fs::write(&output, body).await;
        write_result
            .0
            .with_context(|| format!("Failed to write {}", output.display()))?;
        println!("output={}", output.display());
    } else {
        println!("{}", String::from_utf8(body).context("JSON is not UTF-8")?);
    }
    Ok(())
}

pub async fn detect(path: PathBuf, _opts: GlobalOptions) -> Result<()> {
    let local = detect_local_install(&path).await?;
    println!("install_path={}", local.install_path.display());
    println!("config_ini={}", local.config_ini.path.display());
    println!("known_game={:?}", local.game_id);
    println!("known_server={:?}", local.server_id);
    for (key, value) in &local.config_ini.fields {
        println!("{}={}", key, value);
    }
    Ok(())
}

pub async fn config_ini(path: PathBuf, _opts: GlobalOptions) -> Result<()> {
    let ini = decrypt_config_ini(&path).await?;
    print!("{}", ini.raw);
    Ok(())
}

pub async fn game_files(path: PathBuf, _opts: GlobalOptions) -> Result<()> {
    let game_files_path = resolve_named_path(&path, "game_files");
    let encrypted = compio::fs::read(&game_files_path)
        .await
        .with_context(|| format!("Failed to read {}", game_files_path.display()))?;
    let decrypted = crypto::decrypt_game_files(&encrypted)
        .with_context(|| format!("Failed to decrypt {}", game_files_path.display()))?;
    print!("{}", decrypted);
    Ok(())
}

pub async fn fetch_game_files(
    game_id: GameId,
    server_id: ServerId,
    version: Option<String>,
    output: Option<PathBuf>,
    _opts: GlobalOptions,
) -> Result<()> {
    let api_client = ApiClient::new()?;
    let version_info = api_client
        .get_latest_game(game_id, server_id, version.as_deref())
        .await?;
    let pkg = version_info
        .pkg
        .as_ref()
        .context("No package info available")?;
    let entries = api_client
        .fetch_game_files(&pkg.file_path, pkg.game_files_md5.as_deref())
        .await?;

    if let Some(output) = output {
        let mut body = String::new();
        for entry in entries {
            body.push_str(&serde_json::to_string(&entry)?);
            body.push('\n');
        }
        let write_result = compio::fs::write(&output, body.into_bytes()).await;
        write_result
            .0
            .with_context(|| format!("Failed to write {}", output.display()))?;
    } else {
        for entry in entries {
            println!("{}", serde_json::to_string(&entry)?);
        }
    }

    Ok(())
}

pub async fn fetch_file(
    game_id: GameId,
    server_id: ServerId,
    version: Option<String>,
    file: String,
    output: PathBuf,
    _opts: GlobalOptions,
) -> Result<()> {
    let api_client = ApiClient::new()?;
    let version_info = api_client
        .get_latest_game(game_id, server_id, version.as_deref())
        .await?;
    let pkg = version_info
        .pkg
        .as_ref()
        .context("No package info available")?;
    let entries = api_client
        .fetch_game_files(&pkg.file_path, pkg.game_files_md5.as_deref())
        .await?;
    let entry = entries
        .into_iter()
        .find(|entry| entry.path == file)
        .context("Requested file not found in remote game_files manifest")?;

    let base_url = pkg.file_path.trim_end_matches('/');
    let url = format!("{}/{}", base_url, entry.path);
    api_client
        .download_file_with_verify(&url, &output, &entry.md5)
        .await
        .with_context(|| format!("Failed to download {} to {}", entry.path, output.display()))?;

    println!("downloaded={} output={}", entry.path, output.display());
    Ok(())
}

pub async fn api_get_latest_game(
    game_id: GameId,
    server_id: ServerId,
    version: Option<String>,
    output: Option<PathBuf>,
    _opts: GlobalOptions,
) -> Result<()> {
    let api_client = ApiClient::new()?;
    let latest = api_client
        .get_latest_game(game_id, server_id, version.as_deref())
        .await?;
    let payload = json!({
        "game": game_id.to_string(),
        "server": server_id.to_string(),
        "request_version": version,
        "response": {
            "action": latest.action,
            "request_version": latest.request_version,
            "version": latest.version,
            "state": latest.state,
            "launcher_action": latest.launcher_action,
            "pkg": latest.pkg,
            "patch": latest.patch,
            "has_update": latest.has_update(),
            "has_full_package": latest.has_full_package(),
            "has_patch_package": latest.has_patch_package(),
            "rand_str": latest.rand_str(),
        }
    });
    emit_json(output, payload).await
}

pub async fn api_get_latest_resources(
    game_id: GameId,
    server_id: ServerId,
    version: Option<String>,
    resource_version: Option<String>,
    rand_str: Option<String>,
    platform: String,
    output: Option<PathBuf>,
    _opts: GlobalOptions,
) -> Result<()> {
    let api_client = ApiClient::new()?;
    let latest = api_client
        .get_latest_game(game_id, server_id, version.as_deref())
        .await?;
    let effective_resource_version = resource_version.unwrap_or_else(|| latest.version.clone());
    let effective_rand_str = rand_str.unwrap_or_else(|| latest.rand_str());
    if effective_rand_str.is_empty() {
        anyhow::bail!(
            "rand_str is empty for game={} server={} version={}; pass --rand-str explicitly",
            game_id,
            server_id,
            effective_resource_version
        );
    }

    let resources = api_client
        .get_latest_resources(
            game_id,
            server_id,
            &effective_resource_version,
            &effective_rand_str,
            &platform,
        )
        .await?;

    let payload = json!({
        "game": game_id.to_string(),
        "server": server_id.to_string(),
        "latest_game": {
            "requested_version": version,
            "resolved_version": latest.version,
            "resolved_rand_str": latest.rand_str(),
        },
        "request": {
            "resource_version": effective_resource_version,
            "rand_str": effective_rand_str,
            "platform": platform,
        },
        "response": {
            "res_version": resources.res_version,
            "patch_index_path": resources.patch_index_path,
            "domain": resources.domain,
            "configs": resources.configs,
            "resources": resources.resources,
        }
    });
    emit_json(output, payload).await
}

pub async fn list_resource_files(
    game_id: GameId,
    server_id: ServerId,
    version: Option<String>,
    resource_version: Option<String>,
    rand_str: Option<String>,
    platform: String,
    output: Option<PathBuf>,
    _opts: GlobalOptions,
) -> Result<()> {
    let api_client = ApiClient::new()?;
    let latest = api_client
        .get_latest_game(game_id, server_id, version.as_deref())
        .await?;

    let effective_resource_version = resource_version.unwrap_or_else(|| latest.version.clone());
    let effective_rand_str = rand_str.unwrap_or_else(|| latest.rand_str());
    if effective_rand_str.is_empty() {
        anyhow::bail!(
            "rand_str is empty for game={} server={} version={}; pass --rand-str explicitly",
            game_id,
            server_id,
            effective_resource_version
        );
    }

    let resources = api_client
        .get_latest_resources(
            game_id,
            server_id,
            &effective_resource_version,
            &effective_rand_str,
            &platform,
        )
        .await?;

    let mut total_bytes: u64 = 0;
    let mut files = Vec::new();
    for resource in &resources.resources {
        let index_url = format!("{}/index_{}.json", resource.path, resource.name);
        let index = api_client
            .fetch_res_index(&index_url, crypto::RES_INDEX_KEY)
            .await
            .with_context(|| {
                format!(
                    "Failed to fetch/decrypt resource index for {} ({})",
                    resource.name, index_url
                )
            })?;
        for file in index.files {
            let checksum = file.md5.clone().or(file.hash.clone()).unwrap_or_default();
            total_bytes = total_bytes.saturating_add(file.size);
            files.push(json!({
                "resource_name": resource.name,
                "resource_version": resource.version,
                "resource_path": resource.path,
                "index": file.index,
                "path": file.name,
                "size": file.size,
                "md5": file.md5,
                "hash": file.hash,
                "checksum": checksum,
                "type": file.r#type,
                "manifest": file.manifest
            }));
        }
    }

    let payload = json!({
        "game": game_id.to_string(),
        "server": server_id.to_string(),
        "request": {
            "requested_version": version,
            "resource_version": effective_resource_version,
            "rand_str": effective_rand_str,
            "platform": platform
        },
        "response": {
            "res_version": resources.res_version,
            "domain": resources.domain,
            "resource_groups": resources.resources,
            "total_files": files.len(),
            "total_bytes": total_bytes,
            "files": files
        }
    });
    emit_json(output, payload).await
}

fn media_to_json(
    game_id: GameId,
    server_id: ServerId,
    language: &str,
    media: &griffr_common::api::client::MediaResponse,
) -> Value {
    json!({
        "game": game_id.to_string(),
        "server": server_id.to_string(),
        "language": language,
        "banners": media.banners.as_ref().map(|b| {
            json!({
                "data_version": b.data_version,
                "items": b.banners.iter().map(|item| {
                    json!({
                        "id": item.id,
                        "url": item.url,
                        "md5": item.md5,
                        "jump_url": item.jump_url,
                        "need_token": item.need_token,
                    })
                }).collect::<Vec<_>>()
            })
        }),
        "announcements": media.announcements.as_ref().map(|a| {
            json!({
                "data_version": a.data_version,
                "tabs": a.tabs.iter().map(|tab| {
                    json!({
                        "tab_name": tab.tab_name,
                        "announcements": tab.announcements.iter().map(|item| {
                            json!({
                                "id": item.id,
                                "content": item.content,
                                "jump_url": item.jump_url,
                                "start_ts": item.start_ts,
                                "need_token": item.need_token,
                            })
                        }).collect::<Vec<_>>()
                    })
                }).collect::<Vec<_>>()
            })
        }),
        "background": media.background.as_ref().map(|bg| {
            json!({
                "data_version": bg.data_version,
                "main_bg_image": {
                    "url": bg.main_bg_image.url,
                    "md5": bg.main_bg_image.md5,
                    "video_url": bg.main_bg_image.video_url,
                }
            })
        }),
        "sidebar": media.sidebar.as_ref().map(|s| {
            json!({
                "data_version": s.data_version,
                "items": s.sidebars.iter().map(|item| {
                    json!({
                        "display_type": item.display_type,
                        "media": item.media,
                        "pic": item.pic.as_ref().map(|pic| json!({
                            "url": pic.url,
                            "md5": pic.md5,
                            "description": pic.description,
                        })),
                        "jump_url": item.jump_url,
                        "sidebar_labels": item.sidebar_labels.iter().map(|label| {
                            json!({
                                "content": label.content,
                                "jump_url": label.jump_url,
                            })
                        }).collect::<Vec<_>>(),
                        "grid_info": item.grid_info,
                        "need_token": item.need_token,
                    })
                }).collect::<Vec<_>>()
            })
        }),
    })
}

pub async fn api_get_media(
    game_id: GameId,
    server_id: ServerId,
    language: String,
    output: Option<PathBuf>,
    _opts: GlobalOptions,
) -> Result<()> {
    let api_client = ApiClient::new()?;
    let payload = api_client
        .get_media_raw(game_id, server_id, &language)
        .await?;
    emit_json(output, payload).await
}

pub async fn fetch_media(
    game_id: GameId,
    server_id: ServerId,
    language: String,
    output: Option<PathBuf>,
    _opts: GlobalOptions,
) -> Result<()> {
    let api_client = ApiClient::new()?;
    let media = api_client.get_media(game_id, server_id, &language).await?;
    let payload = media_to_json(game_id, server_id, &language, &media);
    emit_json(output, payload).await
}
