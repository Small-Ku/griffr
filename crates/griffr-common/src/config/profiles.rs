use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{ChannelId, ChannelPair, GameAppCode, GameId, LauncherAppCode, LauncherGateway};

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
        let game = game.as_str();
        let channel = channels.channel().as_str();
        let sub_channel = channels.sub_channel().as_str();
        let is_cn = matches!(channel, "1" | "2");

        let gateway = if is_cn {
            "https://launcher.hypergryph.com"
        } else {
            "https://launcher.gryphline.com"
        };
        let launcher_appcode = if is_cn {
            "abYeZZ16BPluCFyT"
        } else if sub_channel == "801" {
            "BBWoqCzuZ2bZ1Dro"
        } else {
            "TiaytKBUIEdoEwRT"
        };

        let (game_appcode, executable, streaming_assets_subdir) = match (game, is_cn) {
            ("arknights", _) => (
                "GzD1CpaWgmSq1wew",
                PathBuf::from("Arknights.exe"),
                PathBuf::from("Arknights_Data"),
            ),
            ("endfield", true) => (
                "6LL0KJuqHBVz33WK",
                PathBuf::from("Endfield.exe"),
                PathBuf::from("Endfield_Data"),
            ),
            ("endfield", false) => (
                "YDUTE5gscDZ229CW",
                PathBuf::from("Endfield.exe"),
                PathBuf::from("Endfield_Data"),
            ),
            _ => return None,
        };

        Some(InstallProfile {
            target: ApiTarget {
                game_appcode: GameAppCode::new(game_appcode),
                launcher_appcode: LauncherAppCode::new(launcher_appcode),
                gateway: LauncherGateway::new(gateway),
                channel: channels.channel().clone(),
                sub_channel: channels.sub_channel().clone(),
            },
            executable,
            streaming_assets_subdir,
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
