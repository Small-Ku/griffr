use std::path::{Path, PathBuf};

use base64::{engine::general_purpose::STANDARD, Engine};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};

use crate::api::yostar::{
    YostarGameConfig, YostarManifest, YostarManifestEntry, YOSTAR_ARKNIGHTS_TAG,
};
use crate::error::{Error, Result};
use crate::runtime::task_pool::fs_ops::{
    path_safety::parse_safe_relative_path, write_atomic_bytes,
};
use crate::runtime::ContentHash;

pub const YOSTAR_LAUNCHER_CONFIG_NAME: &str = "game-launcher-config.json";
pub const YOSTAR_MANIFEST_NAME: &str = "manifest.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct YostarLauncherConfig {
    pub tag: String,
    pub name: String,
    #[serde(default)]
    pub params: Vec<String>,
    pub version: String,
    #[serde(rename = "gameUninstallScript", default)]
    pub game_uninstall_script: String,
    pub vc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct YostarLocalManifest {
    pub name: String,
    pub version: String,
    pub basis: String,
    pub vc: String,
    #[serde(default)]
    pub files: Vec<YostarLocalManifestEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct YostarLocalManifestEntry {
    pub path: String,
    #[serde(deserialize_with = "deserialize_u64")]
    pub size: u64,
    pub hash: String,
    pub vc: String,
}

#[derive(Debug, Clone)]
pub struct YostarLocalMetadata {
    pub config_path: PathBuf,
    pub manifest_path: PathBuf,
    pub config: YostarLauncherConfig,
    pub manifest: YostarLocalManifest,
}

impl YostarLocalMetadata {
    pub fn version(&self) -> &str {
        &self.config.version
    }

    pub fn entry(&self) -> &str {
        &self.config.name
    }

    pub fn basis(&self) -> &str {
        &self.manifest.basis
    }
}

fn vc(values: &[&str]) -> String {
    STANDARD.encode(Md5::digest(values.join(";").as_bytes()))
}

fn config_vc(config: &YostarLauncherConfig) -> String {
    // JavaScript Array#toString is comma-joined for the launcher-observed
    // primitive string parameter array.
    let params = config.params.join(",");
    vc(&[
        &config.tag,
        &config.name,
        &params,
        &config.version,
        &config.game_uninstall_script,
    ])
}

fn manifest_vc(manifest: &YostarLocalManifest) -> String {
    vc(&[&manifest.name, &manifest.version, &manifest.basis])
}

fn file_vc(entry: &YostarLocalManifestEntry) -> String {
    let size = entry.size.to_string();
    vc(&[&entry.path, &size, &entry.hash])
}

fn invalid(detail: impl Into<String>) -> Error {
    Error::Message {
        context: "YoStar metadata error: ",
        detail: detail.into(),
    }
}

pub fn validate_remote_yostar_manifest(manifest: &YostarManifest) -> Result<()> {
    let mut paths = std::collections::BTreeSet::new();
    for entry in &manifest.files {
        parse_safe_relative_path("YoStar manifest path", &entry.path)?;
        ContentHash::crc64_xz_decimal(&entry.hash)?;
        let normalized = crate::runtime::normalize_logical_path(&entry.path);
        if !paths.insert(normalized) {
            return Err(invalid(format!(
                "manifest contains duplicate/colliding path {:?}",
                entry.path
            )));
        }
    }
    Ok(())
}

pub async fn read_yostar_metadata(install_path: &Path) -> Result<YostarLocalMetadata> {
    let config_path = install_path.join(YOSTAR_LAUNCHER_CONFIG_NAME);
    let manifest_path = install_path.join(YOSTAR_MANIFEST_NAME);
    let (config_bytes, manifest_bytes) = futures_util::try_join!(
        async {
            compio::fs::read(&config_path)
                .await
                .map_err(|source| Error::IoAt {
                    action: "read YoStar launcher config",
                    path: config_path.clone(),
                    source,
                })
        },
        async {
            compio::fs::read(&manifest_path)
                .await
                .map_err(|source| Error::IoAt {
                    action: "read YoStar game manifest",
                    path: manifest_path.clone(),
                    source,
                })
        }
    )?;

    let config: YostarLauncherConfig = serde_json::from_slice(&config_bytes).map_err(|error| {
        invalid(format!(
            "failed to parse {}: {error}",
            config_path.display()
        ))
    })?;
    let manifest: YostarLocalManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|error| {
            invalid(format!(
                "failed to parse {}: {error}",
                manifest_path.display()
            ))
        })?;

    if config.tag != YOSTAR_ARKNIGHTS_TAG {
        return Err(invalid(format!(
            "unsupported YoStar game tag {:?} in {}",
            config.tag,
            config_path.display()
        )));
    }
    if config.version != manifest.version {
        return Err(invalid(format!(
            "launcher config version {} does not match manifest version {}",
            config.version, manifest.version
        )));
    }
    if config.vc != config_vc(&config) {
        return Err(invalid(format!(
            "{} has an invalid vc",
            config_path.display()
        )));
    }
    if manifest.vc != manifest_vc(&manifest) {
        return Err(invalid(format!(
            "{} has an invalid vc",
            manifest_path.display()
        )));
    }

    let mut paths = std::collections::BTreeSet::new();
    for entry in &manifest.files {
        parse_safe_relative_path("YoStar local manifest path", &entry.path)?;
        ContentHash::crc64_xz_decimal(&entry.hash)?;
        if entry.vc != file_vc(entry) {
            return Err(invalid(format!(
                "manifest entry {:?} has an invalid vc",
                entry.path
            )));
        }
        let normalized = crate::runtime::normalize_logical_path(&entry.path);
        if !paths.insert(normalized) {
            return Err(invalid(format!(
                "manifest contains duplicate/colliding path {:?}",
                entry.path
            )));
        }
    }

    Ok(YostarLocalMetadata {
        config_path,
        manifest_path,
        config,
        manifest,
    })
}

pub fn build_yostar_metadata(
    game_config: &YostarGameConfig,
    manifest: &YostarManifest,
) -> Result<(YostarLauncherConfig, YostarLocalManifest)> {
    validate_remote_yostar_manifest(manifest)?;

    let mut config = YostarLauncherConfig {
        tag: YOSTAR_ARKNIGHTS_TAG.to_string(),
        name: game_config.game_start_exe_name.clone(),
        params: game_config.game_start_params.clone(),
        version: game_config.game_latest_version.clone(),
        game_uninstall_script: game_config.game_uninstall_script.clone(),
        vc: String::new(),
    };
    config.vc = config_vc(&config);

    let mut files = Vec::with_capacity(manifest.files.len());
    for remote in &manifest.files {
        let mut entry = YostarLocalManifestEntry {
            path: remote.path.clone(),
            size: remote.size,
            hash: remote.hash.clone(),
            vc: String::new(),
        };
        entry.vc = file_vc(&entry);
        files.push(entry);
    }
    let mut local_manifest = YostarLocalManifest {
        name: YOSTAR_ARKNIGHTS_TAG.to_string(),
        version: game_config.game_latest_version.clone(),
        basis: game_config.game_latest_file_path.clone(),
        vc: String::new(),
        files,
    };
    local_manifest.vc = manifest_vc(&local_manifest);
    Ok((config, local_manifest))
}

pub fn write_yostar_metadata(
    install_path: &Path,
    game_config: &YostarGameConfig,
    manifest: &YostarManifest,
) -> Result<()> {
    let (config, local_manifest) = build_yostar_metadata(game_config, manifest)?;
    let config_bytes = serde_json::to_vec(&config)
        .map_err(|error| invalid(format!("failed to serialize launcher config: {error}")))?;
    let manifest_bytes = serde_json::to_vec(&local_manifest)
        .map_err(|error| invalid(format!("failed to serialize game manifest: {error}")))?;

    // manifest.json is the canonical content receipt. Commit it first and the
    // launcher config/version last, mirroring config.ini-last semantics on the
    // Hypergryph backend.
    write_atomic_bytes(&install_path.join(YOSTAR_MANIFEST_NAME), &manifest_bytes)?;
    write_atomic_bytes(
        &install_path.join(YOSTAR_LAUNCHER_CONFIG_NAME),
        &config_bytes,
    )?;
    Ok(())
}

impl From<&YostarManifestEntry> for YostarLocalManifestEntry {
    fn from(entry: &YostarManifestEntry) -> Self {
        let mut local = Self {
            path: entry.path.clone(),
            size: entry.size,
            hash: entry.hash.clone(),
            vc: String::new(),
        };
        local.vc = file_vc(&local);
        local
    }
}

fn deserialize_u64<'de, D>(deserializer: D) -> std::result::Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Number {
        Integer(u64),
        Text(String),
    }
    match Number::deserialize(deserializer)? {
        Number::Integer(value) => Ok(value),
        Number::Text(value) => value.parse().map_err(serde::de::Error::custom),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> YostarGameConfig {
        YostarGameConfig {
            game_latest_version: "2.7.1".to_string(),
            game_lowest_version: "2.7.0".to_string(),
            game_latest_file_path: "basis-271".to_string(),
            game_start_exe_name: "Arknights.exe".to_string(),
            game_start_params: vec!["-foo".to_string(), "bar".to_string()],
            game_uninstall_script: "uninstall.bat".to_string(),
            decompression_size: Some(123),
        }
    }

    fn manifest() -> YostarManifest {
        YostarManifest {
            source: "files".to_string(),
            files: vec![YostarManifestEntry {
                path: "Arknights_Data/a.bin".to_string(),
                size: 9,
                hash: "11051210869376104954".to_string(),
            }],
        }
    }

    #[test]
    fn metadata_roundtrips_with_observed_vc_scheme() {
        let (config, manifest) = build_yostar_metadata(&config(), &manifest()).unwrap();
        assert_eq!(config.vc, config_vc(&config));
        assert_eq!(manifest.vc, manifest_vc(&manifest));
        assert_eq!(manifest.files[0].vc, file_vc(&manifest.files[0]));
    }

    #[test]
    fn remote_manifest_rejects_unsafe_or_colliding_paths() {
        let mut bad = manifest();
        bad.files[0].path = "../escape.bin".to_string();
        assert!(validate_remote_yostar_manifest(&bad).is_err());

        let mut duplicate = manifest();
        duplicate.files.push(YostarManifestEntry {
            path: "arknights_data/A.bin".to_string(),
            size: 9,
            hash: "11051210869376104954".to_string(),
        });
        assert!(validate_remote_yostar_manifest(&duplicate).is_err());
    }

    #[compio::test]
    async fn written_metadata_is_detectably_valid() {
        let temp = tempfile::tempdir().unwrap();
        write_yostar_metadata(temp.path(), &config(), &manifest()).unwrap();
        let metadata = read_yostar_metadata(temp.path()).await.unwrap();
        assert_eq!(metadata.version(), "2.7.1");
        assert_eq!(metadata.basis(), "basis-271");
        assert_eq!(metadata.entry(), "Arknights.exe");
    }
}
