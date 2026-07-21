use std::path::{Component, Path, PathBuf};

use super::{game_definition, gateway, launcher_appcode, ChannelPair, GameId, RegionId};
use crate::error::{Error, Result};

/// A full launcher API destination for one invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiTarget {
    pub gateway: String,
    pub game_appcode: String,
    pub launcher_appcode: String,
    pub channels: ChannelPair,
}

/// A resolved API target plus local installation layout.
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

fn safe_relative_path(value: &str, field: &str) -> Result<PathBuf> {
    let path = Path::new(value);
    if value.trim().is_empty() {
        return Err(Error::Message {
            context: "Configuration error: ",
            detail: format!("{field} cannot be empty"),
        });
    }
    if path.is_absolute() {
        return Err(Error::Message {
            context: "Configuration error: ",
            detail: format!("{field} must be relative"),
        });
    }
    if !path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(Error::Message {
            context: "Configuration error: ",
            detail: format!("{field} must not contain '.', '..', root, or platform prefixes"),
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
    let game_appcode = overrides
        .game_appcode
        .clone()
        .or_else(|| {
            game_definition(game)
                .and_then(|definition| definition.appcode(region))
                .map(str::to_owned)
        })
        .ok_or_else(|| {
            Error::Message { context: "Configuration error: ", detail: format!(
                "No built-in {region} API target exists for {game}; pass --game-appcode to probe a custom target"
            ) }
        })?;

    Ok(ApiTarget {
        gateway: overrides
            .gateway
            .clone()
            .unwrap_or_else(|| gateway(region).to_owned()),
        game_appcode,
        launcher_appcode: overrides
            .launcher_appcode
            .clone()
            .unwrap_or_else(|| launcher_appcode(region, channels.sub_channel()).to_owned()),
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
        Some(value) => safe_relative_path(value, "exe_name").map_err(|e| Error::Message {
            context: "Configuration error: ",
            detail: format!("Invalid exe_name override: {e}"),
        })?,
        None => definition
            .map(|definition| PathBuf::from(definition.exe_name))
            .ok_or_else(|| Error::Message {
                context: "Configuration error: ",
                detail: format!(
                    "Unknown game definition '{game}'; pass --exe for a custom install target"
                ),
            })?,
    };

    let data_root = match overrides.data_root.as_deref() {
        Some(value) => safe_relative_path(value, "data-root").map_err(|e| Error::Message {
            context: "Configuration error: ",
            detail: format!("Invalid data-root override: {e}"),
        })?,
        None => definition
            .map(|definition| PathBuf::from(definition.data_root))
            .ok_or_else(|| Error::Message {
                context: "Configuration error: ",
                detail: format!(
                    "Unknown game definition '{game}'; pass --data-root for a custom install target"
                ),
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
    use crate::config::{EPIC_LAUNCHER_APPCODE, GRYPHLINE_LAUNCHER_APPCODE};

    #[test]
    fn api_target_uses_explicit_region_and_preserves_channel_pair() {
        let channels = ChannelPair::from_api("123", Some("456")).unwrap();
        let target = resolve_api_target(
            &GameId::ENDFIELD,
            RegionId::Sg,
            &channels,
            &ApiTargetOverrides::default(),
        )
        .unwrap();

        assert_eq!(target.gateway, "https://launcher.gryphline.com");
        assert_eq!(target.channels, channels);
    }

    #[test]
    fn sg_store_aliases_select_native_launcher_appcodes() {
        let google_play =
            ChannelPair::parse(RegionId::Sg, None, Some("google-play".to_string())).unwrap();
        let google_play_target = resolve_api_target(
            &GameId::ENDFIELD,
            RegionId::Sg,
            &google_play,
            &ApiTargetOverrides::default(),
        )
        .unwrap();
        assert_eq!(
            google_play_target.launcher_appcode,
            GRYPHLINE_LAUNCHER_APPCODE
        );

        let epic = ChannelPair::parse(RegionId::Sg, None, Some("epic".to_string())).unwrap();
        let epic_target = resolve_api_target(
            &GameId::ENDFIELD,
            RegionId::Sg,
            &epic,
            &ApiTargetOverrides::default(),
        )
        .unwrap();
        assert_eq!(epic_target.launcher_appcode, EPIC_LAUNCHER_APPCODE);
    }

    #[test]
    fn missing_builtin_region_appcode_is_a_resolution_error_not_a_parse_error() {
        let channels = ChannelPair::parse(RegionId::Sg, None, None).unwrap();
        let result = resolve_api_target(
            &GameId::ARKNIGHTS,
            RegionId::Sg,
            &channels,
            &ApiTargetOverrides::default(),
        );

        assert!(result.is_err());
    }

    #[test]
    fn explicit_appcode_can_probe_target_without_builtin_region_definition() {
        let channels = ChannelPair::parse(RegionId::Sg, None, None).unwrap();
        let target = resolve_api_target(
            &GameId::ARKNIGHTS,
            RegionId::Sg,
            &channels,
            &ApiTargetOverrides {
                game_appcode: Some("custom-arknights-sg-appcode".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(target.game_appcode, "custom-arknights-sg-appcode");
        assert_eq!(target.channels.channel().as_str(), "6");
        assert_eq!(target.channels.sub_channel().as_str(), "6");
    }

    #[test]
    fn custom_install_target_only_needs_explicit_local_layout() {
        let game = GameId::new("custom-game");
        let channels = ChannelPair::parse(RegionId::Cn, None, None).unwrap();
        let target = resolve_install_target(
            &game,
            RegionId::Cn,
            &channels,
            &InstallTargetOverrides {
                api: ApiTargetOverrides {
                    game_appcode: Some("custom-appcode".to_string()),
                    ..Default::default()
                },
                exe_name: Some("Custom.exe".to_string()),
                data_root: Some("Custom_Data".to_string()),
            },
        )
        .unwrap();

        assert_eq!(target.api.game_appcode, "custom-appcode");
        assert_eq!(target.exe_name, PathBuf::from("Custom.exe"));
        assert_eq!(target.data_root, PathBuf::from("Custom_Data"));
    }
}
