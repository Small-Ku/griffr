use std::path::PathBuf;

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::api::crypto;
use griffr_common::config::{ChannelPair, GameId, RegionId};
use griffr_common::runtime::{resource_manifest_url, ResourceManifestKind};
use serde_json::json;

use super::utils::emit_json;
use crate::GlobalOptions;

pub async fn fetch_game_files(
    game_id: GameId,
    region_id: RegionId,
    channel_id: ChannelPair,
    overrides: crate::ApiTargetOverrideArgs,
    version: Option<String>,
    output: Option<PathBuf>,
    _opts: GlobalOptions,
) -> Result<()> {
    let api_client = ApiClient::new()?;
    let target = griffr_common::config::resolve_api_target(
        &game_id,
        region_id,
        &channel_id,
        &overrides.into(),
    )?;
    let version_info = api_client
        .get_latest_game(&target, version.as_deref())
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
    region_id: RegionId,
    channel_id: ChannelPair,
    overrides: crate::ApiTargetOverrideArgs,
    version: Option<String>,
    file: String,
    output: PathBuf,
    _opts: GlobalOptions,
) -> Result<()> {
    let api_client = ApiClient::new()?;
    let target = griffr_common::config::resolve_api_target(
        &game_id,
        region_id,
        &channel_id,
        &overrides.into(),
    )?;
    let version_info = api_client
        .get_latest_game(&target, version.as_deref())
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
    region_id: RegionId,
    channel_id: ChannelPair,
    overrides: crate::ApiTargetOverrideArgs,
    version: Option<String>,
    output: Option<PathBuf>,
    _opts: GlobalOptions,
) -> Result<()> {
    let api_client = ApiClient::new()?;
    let target = griffr_common::config::resolve_api_target(
        &game_id,
        region_id,
        &channel_id,
        &overrides.into(),
    )?;
    let latest = api_client
        .get_latest_game(&target, version.as_deref())
        .await?;
    let payload = json!({
        "game": game_id.to_string(),
        "region": region_id.to_string(),
        "channel": channel_id.channel().to_string(),
        "sub_channel": channel_id.sub_channel().to_string(),
        "request_version": version,
        "response": {
            "action": latest.action,
            "request_version": latest.request_version,
            "version": latest.version,
            "state": latest.state,
            "launcher_action": latest.launcher_action,
            "pkg": latest.pkg,
            "patch": latest.patch,
            "pre_patch": latest.pre_patch,
            "has_update": latest.has_update(),
            "has_full_package": latest.has_full_package(),
            "has_patch_package": latest.has_patch_package(),
            "has_pre_patch_package": latest.has_pre_patch_package(),
            "rand_str": latest.rand_str(),
        }
    });
    emit_json(output, payload).await
}

pub async fn api_get_latest_resources(
    game_id: GameId,
    region_id: RegionId,
    channel_id: ChannelPair,
    overrides: crate::ApiTargetOverrideArgs,
    version: Option<String>,
    resource_version: Option<String>,
    rand_str: Option<String>,
    platform: String,
    output: Option<PathBuf>,
    _opts: GlobalOptions,
) -> Result<()> {
    let api_client = ApiClient::new()?;
    let target = griffr_common::config::resolve_api_target(
        &game_id,
        region_id,
        &channel_id,
        &overrides.into(),
    )?;
    let latest = api_client
        .get_latest_game(&target, version.as_deref())
        .await?;
    let effective_resource_version = resource_version.unwrap_or_else(|| latest.version.clone());
    let effective_rand_str = rand_str.unwrap_or_else(|| latest.rand_str());
    if effective_rand_str.is_empty() {
        anyhow::bail!(
            "rand_str is empty for game={} channel={} sub-channel={} version={}; pass --rand-str explicitly",
            game_id,
            channel_id.channel(),
            channel_id.sub_channel(),
            effective_resource_version
        );
    }

    let resources = api_client
        .get_latest_resources(
            &target,
            &effective_resource_version,
            &effective_rand_str,
            &platform,
        )
        .await?;

    let payload = json!({
        "game": game_id.to_string(),
        "region": region_id.to_string(),
        "channel": channel_id.channel().to_string(),
        "sub_channel": channel_id.sub_channel().to_string(),
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
    region_id: RegionId,
    channel_id: ChannelPair,
    overrides: crate::ApiTargetOverrideArgs,
    version: Option<String>,
    resource_version: Option<String>,
    rand_str: Option<String>,
    platform: String,
    output: Option<PathBuf>,
    _opts: GlobalOptions,
) -> Result<()> {
    let api_client = ApiClient::new()?;
    let target = griffr_common::config::resolve_api_target(
        &game_id,
        region_id,
        &channel_id,
        &overrides.into(),
    )?;
    let latest = api_client
        .get_latest_game(&target, version.as_deref())
        .await?;

    let effective_resource_version = resource_version.unwrap_or_else(|| latest.version.clone());
    let effective_rand_str = rand_str.unwrap_or_else(|| latest.rand_str());
    if effective_rand_str.is_empty() {
        anyhow::bail!(
            "rand_str is empty for game={} channel={} sub-channel={} version={}; pass --rand-str explicitly",
            game_id,
            channel_id.channel(),
            channel_id.sub_channel(),
            effective_resource_version
        );
    }

    let resources = api_client
        .get_latest_resources(
            &target,
            &effective_resource_version,
            &effective_rand_str,
            &platform,
        )
        .await?;

    let mut total_bytes: u64 = 0;
    let mut files = Vec::new();
    for resource in &resources.resources {
        let index_url =
            resource_manifest_url(&resource.path, ResourceManifestKind::Index, &resource.name);
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
        "region": region_id.to_string(),
        "channel": channel_id.channel().to_string(),
        "sub_channel": channel_id.sub_channel().to_string(),
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
