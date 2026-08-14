use std::path::PathBuf;

use anyhow::{Context, Result};
use griffr_core::RegionId;
use griffr_yostar_api::yostar_arknights_target;
use griffr_yostar_api::{YostarApiClient, YostarManifest, YOSTAR_LAUNCHER_VERSION};
use serde_json::json;

use super::vfs_support::emit_json;

fn client(region: RegionId, gateway: Option<&str>) -> Result<YostarApiClient> {
    YostarApiClient::arknights_with_gateway(region, gateway).map_err(Into::into)
}

fn target_json(region: RegionId, gateway: Option<&str>) -> Result<serde_json::Value> {
    let target = yostar_arknights_target(region)
        .with_context(|| format!("region {region} is not a YoStar Arknights deployment"))?;
    Ok(json!({
        "backend": "yostar",
        "game": "arknights",
        "region": region.to_string(),
        "gateway": gateway.unwrap_or(target.gateway),
        "game_tag": target.game_tag,
        "launcher_version": YOSTAR_LAUNCHER_VERSION,
    }))
}

async fn resolve_manifest(
    client: &YostarApiClient,
    version: Option<String>,
    basis: Option<String>,
) -> Result<(String, String, YostarManifest)> {
    match (version, basis) {
        (None, None) => {
            let config = client.game_config().await?;
            let version = config.game_latest_version;
            let basis = config.game_latest_file_path;
            let manifest = client.manifest_for(&version, &basis).await?;
            Ok((version, basis, manifest))
        }
        (Some(version), Some(basis)) => {
            let manifest = client.manifest_for(&version, &basis).await?;
            Ok((version, basis, manifest))
        }
        _ => anyhow::bail!(
            "YoStar manifest identity is (version, basis); pass both --version and --basis, or omit both for latest"
        ),
    }
}

pub async fn config(
    region: RegionId,
    gateway: Option<String>,
    output: Option<PathBuf>,
) -> Result<()> {
    let client = client(region, gateway.as_deref())?;
    let response = client.game_config().await?;
    let mut payload = target_json(region, gateway.as_deref())?;
    payload["response"] = serde_json::to_value(response)?;
    emit_json(output, payload).await
}

pub async fn cdn(region: RegionId, gateway: Option<String>, output: Option<PathBuf>) -> Result<()> {
    let client = client(region, gateway.as_deref())?;
    let response = client.cdn_config().await?;
    let mut payload = target_json(region, gateway.as_deref())?;
    payload["response"] = serde_json::to_value(response)?;
    emit_json(output, payload).await
}

pub async fn manifest(
    region: RegionId,
    gateway: Option<String>,
    version: Option<String>,
    basis: Option<String>,
    output: Option<PathBuf>,
) -> Result<()> {
    let client = client(region, gateway.as_deref())?;
    let (version, basis, manifest) = resolve_manifest(&client, version, basis).await?;
    griffr_runtime::validate_remote_yostar_manifest(&manifest)?;
    let mut payload = target_json(region, gateway.as_deref())?;
    payload["request"] = json!({"version": version, "basis": basis});
    payload["response"] = serde_json::to_value(manifest)?;
    emit_json(output, payload).await
}

pub async fn file_url(
    region: RegionId,
    gateway: Option<String>,
    version: Option<String>,
    basis: Option<String>,
    file: String,
    output: Option<PathBuf>,
) -> Result<()> {
    let client = client(region, gateway.as_deref())?;
    let (manifest_result, cdn_result) = futures_util::join!(
        resolve_manifest(&client, version, basis),
        client.cdn_config()
    );
    let (version, basis, manifest) = manifest_result?;
    let cdn = cdn_result?;
    griffr_runtime::validate_remote_yostar_manifest(&manifest)?;
    let entry = manifest
        .files
        .iter()
        .find(|entry| {
            griffr_runtime::normalize_logical_path(&entry.path)
                == griffr_runtime::normalize_logical_path(&file)
        })
        .with_context(|| format!("file {file:?} is not present in the selected YoStar manifest"))?;

    let mut urls = vec![manifest.file_url(&cdn.primary_cdn, entry)];
    if !cdn.back_up_cdn.trim().is_empty() && cdn.back_up_cdn != cdn.primary_cdn {
        urls.push(manifest.file_url(&cdn.back_up_cdn, entry));
    }

    let mut payload = target_json(region, gateway.as_deref())?;
    payload["request"] = json!({"version": version, "basis": basis, "file": file});
    payload["response"] = json!({
        "path": entry.path,
        "size": entry.size,
        "crc64_xz": entry.hash,
        "urls": urls,
    });
    emit_json(output, payload).await
}
