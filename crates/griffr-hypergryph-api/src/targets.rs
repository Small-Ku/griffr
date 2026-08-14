use std::path::{Component, Path, PathBuf};

use griffr_core::{game_definition, BackendKind, ChannelId, ChannelPair, GameId, RegionId};

use crate::{Error, Result};

pub const HYPERGRYPH_GATEWAY: &str = "https://launcher.hypergryph.com";
pub const GRYPHLINE_GATEWAY: &str = "https://launcher.gryphline.com";
pub const HYPERGRYPH_LAUNCHER_APPCODE: &str = "abYeZZ16BPluCFyT";
pub const GRYPHLINE_LAUNCHER_APPCODE: &str = "TiaytKBUIEdoEwRT";
pub const EPIC_LAUNCHER_APPCODE: &str = "BBWoqCzuZ2bZ1Dro";

/// A full Hypergryph/Gryphline launcher API destination for one invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiTarget {
    pub gateway: String,
    pub game_appcode: String,
    pub launcher_appcode: String,
    pub channels: ChannelPair,
}

/// Resolved remote API target plus the local game layout advertised by Griffr's catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallTarget {
    pub api: ApiTarget,
    pub exe_name: PathBuf,
    pub data_root: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct ApiTargetOverrides {
    pub gateway: Option<String>,
    pub game_appcode: Option<String>,
    pub launcher_appcode: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct InstallTargetOverrides {
    pub api: ApiTargetOverrides,
    pub exe_name: Option<String>,
    pub data_root: Option<String>,
}

fn builtin_game_appcode(game: &GameId, region: RegionId) -> Option<&'static str> {
    match (game.as_str(), region) {
        ("arknights", RegionId::Cn) => Some("GzD1CpaWgmSq1wew"),
        ("endfield", RegionId::Cn) => Some("6LL0KJuqHBVz33WK"),
        ("endfield", RegionId::Sg) => Some("YDUTE5gscDZ229CW"),
        _ => None,
    }
}

pub fn game_by_appcode(appcode: &str) -> Option<GameId> {
    [
        (GameId::ARKNIGHTS, "GzD1CpaWgmSq1wew"),
        (GameId::ENDFIELD, "6LL0KJuqHBVz33WK"),
        (GameId::ENDFIELD, "YDUTE5gscDZ229CW"),
    ]
    .into_iter()
    .find_map(|(game, known)| (known == appcode).then_some(game))
}

fn gateway(region: RegionId) -> Result<&'static str> {
    match region {
        RegionId::Cn => Ok(HYPERGRYPH_GATEWAY),
        RegionId::Sg => Ok(GRYPHLINE_GATEWAY),
        RegionId::Kr | RegionId::En | RegionId::Jp => Err(Error::Message {
            context: "Target resolution error: ",
            detail: format!("{region} uses the YoStar launcher backend"),
        }),
    }
}

fn launcher_appcode(region: RegionId, sub_channel: &ChannelId) -> Result<&'static str> {
    match region {
        RegionId::Cn => Ok(HYPERGRYPH_LAUNCHER_APPCODE),
        RegionId::Sg if sub_channel == &ChannelId::EPIC => Ok(EPIC_LAUNCHER_APPCODE),
        RegionId::Sg => Ok(GRYPHLINE_LAUNCHER_APPCODE),
        RegionId::Kr | RegionId::En | RegionId::Jp => Err(Error::Message {
            context: "Target resolution error: ",
            detail: format!("{region} does not use a Hypergryph launcher appcode"),
        }),
    }
}

fn safe_relative_path(value: &str, field: &str) -> Result<PathBuf> {
    let path = Path::new(value);
    if value.trim().is_empty() {
        return Err(Error::Message {
            context: "Target resolution error: ",
            detail: format!("{field} cannot be empty"),
        });
    }
    if path.is_absolute()
        || !path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(Error::Message {
            context: "Target resolution error: ",
            detail: format!("{field} must be a safe relative path"),
        });
    }
    Ok(path.to_path_buf())
}

pub fn resolve_api_target(
    game: &GameId,
    region: RegionId,
    channels: &ChannelPair,
    overrides: &ApiTargetOverrides,
) -> Result<ApiTarget> {
    if region.backend() != BackendKind::Hypergryph {
        return Err(Error::Message {
            context: "Target resolution error: ",
            detail: format!(
                "YoStar {region} cannot be resolved as a Hypergryph/Gryphline API target"
            ),
        });
    }

    let game_appcode = overrides
        .game_appcode
        .clone()
        .or_else(|| builtin_game_appcode(game, region).map(str::to_owned))
        .ok_or_else(|| Error::Message {
            context: "Target resolution error: ",
            detail: format!(
                "No built-in {region} API target exists for {game}; pass --game-appcode to probe a custom target"
            ),
        })?;

    Ok(ApiTarget {
        gateway: overrides
            .gateway
            .clone()
            .unwrap_or(gateway(region)?.to_owned()),
        game_appcode,
        launcher_appcode: overrides
            .launcher_appcode
            .clone()
            .unwrap_or(launcher_appcode(region, channels.sub_channel())?.to_owned()),
        channels: channels.clone(),
    })
}

pub fn resolve_install_target(
    game: &GameId,
    region: RegionId,
    channels: &ChannelPair,
    overrides: &InstallTargetOverrides,
) -> Result<InstallTarget> {
    let api = resolve_api_target(game, region, channels, &overrides.api)?;
    let definition = game_definition(game);

    let exe_name = match overrides.exe_name.as_deref() {
        Some(value) => safe_relative_path(value, "exe_name")?,
        None => definition
            .map(|definition| PathBuf::from(definition.exe_name))
            .ok_or_else(|| Error::Message {
                context: "Target resolution error: ",
                detail: format!("Unknown game '{game}'; pass --exe for a custom target"),
            })?,
    };
    let data_root = match overrides.data_root.as_deref() {
        Some(value) => safe_relative_path(value, "data-root")?,
        None => definition
            .map(|definition| PathBuf::from(definition.data_root))
            .ok_or_else(|| Error::Message {
                context: "Target resolution error: ",
                detail: format!("Unknown game '{game}'; pass --data-root for a custom target"),
            })?,
    };

    Ok(InstallTarget {
        api,
        exe_name,
        data_root,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_backend_owned() {
        let channels = ChannelPair::parse(RegionId::Sg, None, Some("epic".into())).unwrap();
        let target = resolve_api_target(
            &GameId::ENDFIELD,
            RegionId::Sg,
            &channels,
            &Default::default(),
        )
        .unwrap();
        assert_eq!(target.game_appcode, "YDUTE5gscDZ229CW");
        assert_eq!(target.launcher_appcode, EPIC_LAUNCHER_APPCODE);
    }

    #[test]
    fn rejects_yostar_regions() {
        let channels = ChannelPair::new(ChannelId::CN_OFFICIAL, None);
        assert!(resolve_api_target(
            &GameId::ARKNIGHTS,
            RegionId::En,
            &channels,
            &Default::default(),
        )
        .is_err());
    }
}
