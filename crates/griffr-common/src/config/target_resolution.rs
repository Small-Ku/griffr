use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{
    ApiTarget, ChannelPair, GameAppCode, GameId, InstallProfile, KnownTargets, LauncherAppCode,
    LauncherGateway,
};
use crate::error::{Error, Result};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TargetOverride {
    pub gateway: Option<String>,
    pub game_appcode: Option<String>,
    pub launcher_appcode: Option<String>,
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
    channel: &ChannelPair,
    overrides: &TargetOverride,
) -> Result<InstallProfile> {
    let mut profile = KnownTargets::resolve(game, channel).ok_or_else(|| {
        Error::Config(format!(
            "Unknown game profile '{game}'; channel values are passed through to the server"
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
    channel: &ChannelPair,
    overrides: &TargetOverride,
) -> Result<ApiTarget> {
    Ok(resolve_install_profile(game, channel, overrides)?.target)
}
