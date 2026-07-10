use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{
    ChannelCode, ChannelId, GameAppCode, GameId, LauncherAppCode, LauncherGateway, SubChannelCode,
};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiTarget {
    pub game_appcode: GameAppCode,
    pub launcher_appcode: LauncherAppCode,
    pub gateway: LauncherGateway,
    pub channel_code: ChannelCode,
    pub sub_channel: SubChannelCode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallProfile {
    pub target: ApiTarget,
    pub executable: PathBuf,
    pub streaming_assets_subdir: PathBuf,
}

pub struct KnownTargets;

impl KnownTargets {
    pub fn resolve(game: &GameId, channel: &ChannelId) -> Option<InstallProfile> {
        let game_str = game.as_str();
        let channel_str = channel.as_str();

        let (gateway, launcher_appcode, channel_code, sub_channel) = match channel_str {
            "cn_official" => (
                "https://launcher.hypergryph.com",
                "abYeZZ16BPluCFyT",
                "1",
                "1",
            ),
            "cn_bilibili" => (
                "https://launcher.hypergryph.com",
                "abYeZZ16BPluCFyT",
                "2",
                "2",
            ),
            "global_official" => (
                "https://launcher.gryphline.com",
                "TiaytKBUIEdoEwRT",
                "6",
                "6",
            ),
            "global_epic" => (
                "https://launcher.gryphline.com",
                "BBWoqCzuZ2bZ1Dro",
                "6",
                "801",
            ),
            "global_googleplay" => (
                "https://launcher.gryphline.com",
                "TiaytKBUIEdoEwRT",
                "6",
                "802",
            ),
            _ => return None,
        };

        let (executable, streaming_assets_subdir) = match game_str {
            "arknights" => (
                PathBuf::from("Arknights.exe"),
                PathBuf::from("Arknights_Data"),
            ),
            "endfield" => (
                PathBuf::from("Endfield.exe"),
                PathBuf::from("Endfield_Data"),
            ),
            _ => return None,
        };

        let game_appcode = match (game_str, channel_str.starts_with("global_")) {
            ("arknights", _) => "GzD1CpaWgmSq1wew",
            ("endfield", false) => "6LL0KJuqHBVz33WK",
            ("endfield", true) => "YDUTE5gscDZ229CW",
            _ => return None,
        };

        Some(InstallProfile {
            target: ApiTarget {
                game_appcode: GameAppCode::new(game_appcode),
                launcher_appcode: LauncherAppCode::new(launcher_appcode),
                gateway: LauncherGateway::new(gateway),
                channel_code: ChannelCode::new(channel_code),
                sub_channel: SubChannelCode::new(sub_channel),
            },
            executable,
            streaming_assets_subdir,
        })
    }

    pub fn find_by_appcode_and_channel(
        appcode: &str,
        channel_code: &str,
        sub_channel: &str,
    ) -> Option<(GameId, ChannelId)> {
        for (game, channels) in &[
            ("arknights", &["cn_official", "cn_bilibili"][..]),
            (
                "endfield",
                &[
                    "cn_official",
                    "cn_bilibili",
                    "global_official",
                    "global_epic",
                    "global_googleplay",
                ][..],
            ),
        ] {
            for channel_alias in *channels {
                let g = GameId::new(*game);
                let s = ChannelId::new(*channel_alias);
                if let Some(profile) = Self::resolve(&g, &s) {
                    if profile.target.game_appcode.0 == appcode
                        && profile.target.channel_code.0 == channel_code
                        && profile.target.sub_channel.0 == sub_channel
                    {
                        return Some((g, s));
                    }
                }
            }
        }
        None
    }
}
