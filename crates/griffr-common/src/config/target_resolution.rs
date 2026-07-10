use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{
    ApiTarget, ChannelCode, ChannelId, GameAppCode, GameId, InstallProfile, KnownTargets,
    LauncherAppCode, LauncherGateway, SubChannelCode,
};
use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChannelSettings {
    pub installed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_path: Option<PathBuf>,
    pub version: Option<String>,
    pub last_update: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TargetOverride {
    pub gateway: Option<String>,
    pub game_appcode: Option<String>,
    pub launcher_appcode: Option<String>,
    pub channel_code: Option<String>,
    pub sub_channel: Option<String>,
    pub executable: Option<String>,
    pub streaming_assets_subdir: Option<String>,
}

fn safe_relative_path(value: &str, field: &str) -> Result<PathBuf> {
    let path = Path::new(value);
    if value.trim().is_empty() {
        return Err(Error::Config(format!("{field} cannot be empty")));
    }
    if path.is_absolute() {
        return Err(Error::Config(format!("{field} must be relative")));
    }
    if !path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(Error::Config(format!(
            "{field} must not contain '.', '..', root, or platform prefixes"
        )));
    }
    Ok(path.to_path_buf())
}

pub fn resolve_install_profile(
    game: &GameId,
    channel: &ChannelId,
    overrides: &TargetOverride,
) -> Result<InstallProfile> {
    let mut profile = KnownTargets::resolve(game, channel).ok_or_else(|| {
        Error::Config(format!(
            "Unknown target '{game}/{channel}'; custom targets are not supported without a complete built-in profile"
        ))
    })?;

    if let Some(value) = &overrides.gateway {
        profile.target.gateway = LauncherGateway::new(value.clone());
    }
    if let Some(value) = &overrides.game_appcode {
        profile.target.game_appcode = GameAppCode::new(value.clone());
    }
    if let Some(value) = &overrides.launcher_appcode {
        profile.target.launcher_appcode = LauncherAppCode::new(value.clone());
    }
    if let Some(value) = &overrides.channel_code {
        profile.target.channel_code = ChannelCode::new(value.clone());
    }
    if let Some(value) = &overrides.sub_channel {
        profile.target.sub_channel = SubChannelCode::new(value.clone());
    }
    if let Some(value) = &overrides.executable {
        profile.executable = safe_relative_path(value, "executable")
            .map_err(|e| Error::Config(format!("Invalid executable override: {e}")))?;
    }
    if let Some(value) = &overrides.streaming_assets_subdir {
        profile.streaming_assets_subdir = safe_relative_path(value, "streaming-assets-subdir")
            .map_err(|e| Error::Config(format!("Invalid streaming assets override: {e}")))?;
    }

    Ok(profile)
}

pub fn resolve_api_target(
    game: &GameId,
    channel: &ChannelId,
    overrides: &TargetOverride,
) -> Result<ApiTarget> {
    Ok(resolve_install_profile(game, channel, overrides)?.target)
}
