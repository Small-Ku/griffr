use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{
    deployment_region, game_catalog_entry, gateway, launcher_appcode, ChannelId, ChannelPair,
    GameAppCode, GameId, LauncherAppCode, LauncherGateway,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiTarget {
    pub game_appcode: GameAppCode,
    pub launcher_appcode: LauncherAppCode,
    pub gateway: LauncherGateway,
    pub channel: ChannelId,
    pub sub_channel: ChannelId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallProfile {
    pub target: ApiTarget,
    pub executable: PathBuf,
    pub streaming_assets_subdir: PathBuf,
}

pub struct KnownTargets;

impl KnownTargets {
    pub fn resolve(game: &GameId, channels: &ChannelPair) -> Option<InstallProfile> {
        let game = game_catalog_entry(game)?;
        let region = deployment_region(channels.channel());

        Some(InstallProfile {
            target: ApiTarget {
                game_appcode: GameAppCode::new(game.appcode(region)),
                launcher_appcode: LauncherAppCode::new(launcher_appcode(
                    region,
                    channels.sub_channel(),
                )),
                gateway: LauncherGateway::new(gateway(region)),
                channel: channels.channel().clone(),
                sub_channel: channels.sub_channel().clone(),
            },
            executable: PathBuf::from(game.executable),
            streaming_assets_subdir: PathBuf::from(game.data_root),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_passes_both_channel_ids_through_unchanged() {
        let channels = ChannelPair::parse("123", Some("456")).unwrap();
        let profile = KnownTargets::resolve(&GameId::ENDFIELD, &channels).unwrap();
        assert_eq!(profile.target.channel.as_str(), "123");
        assert_eq!(profile.target.sub_channel.as_str(), "456");
    }
}
