use super::{ChannelId, GameId, RegionId};

/// Static facts that are intrinsic to a supported game.
#[derive(Debug, Clone)]
pub struct GameDefinition {
    pub id: GameId,
    pub executable: &'static str,
    pub data_root: &'static str,
    pub local_low_dir: &'static str,
    pub cn_appcode: &'static str,
    pub sg_appcode: Option<&'static str>,
}

impl GameDefinition {
    pub fn game_id(&self) -> GameId {
        self.id.clone()
    }

    pub fn appcode(&self, region: RegionId) -> Option<&'static str> {
        match region {
            RegionId::Cn => Some(self.cn_appcode),
            RegionId::Sg => self.sg_appcode,
        }
    }
}

pub const HYPERGRYPH_GATEWAY: &str = "https://launcher.hypergryph.com";
pub const GRYPHLINE_GATEWAY: &str = "https://launcher.gryphline.com";
pub const HYPERGRYPH_LAUNCHER_APPCODE: &str = "abYeZZ16BPluCFyT";
pub const GRYPHLINE_LAUNCHER_APPCODE: &str = "TiaytKBUIEdoEwRT";
pub const EPIC_LAUNCHER_APPCODE: &str = "BBWoqCzuZ2bZ1Dro";
pub const HYPERGRYPH_LOCAL_LOW_VENDOR: &str = "Hypergryph";
pub const GRYPHLINE_LOCAL_LOW_VENDOR: &str = "Gryphline";

pub static GAME_DEFINITIONS: &[GameDefinition] = &[
    GameDefinition {
        id: GameId::ARKNIGHTS,
        executable: "Arknights.exe",
        data_root: "Arknights_Data",
        local_low_dir: "Arknights",
        cn_appcode: "GzD1CpaWgmSq1wew",
        // The launcher API does not provide an official SG PC target.
        sg_appcode: None,
    },
    GameDefinition {
        id: GameId::ENDFIELD,
        executable: "Endfield.exe",
        data_root: "Endfield_Data",
        local_low_dir: "Endfield",
        cn_appcode: "6LL0KJuqHBVz33WK",
        sg_appcode: Some("YDUTE5gscDZ229CW"),
    },
];

pub fn game_definition(game: &GameId) -> Option<&'static GameDefinition> {
    GAME_DEFINITIONS.iter().find(|entry| &entry.id == game)
}

pub fn game_by_appcode(appcode: &str) -> Option<GameId> {
    GAME_DEFINITIONS
        .iter()
        .find(|entry| {
            entry.cn_appcode == appcode || entry.sg_appcode.is_some_and(|value| value == appcode)
        })
        .map(GameDefinition::game_id)
}

pub fn game_by_executable(executable: &str) -> Option<GameId> {
    GAME_DEFINITIONS
        .iter()
        .find(|entry| entry.executable.eq_ignore_ascii_case(executable))
        .map(GameDefinition::game_id)
}

pub const fn gateway(region: RegionId) -> &'static str {
    match region {
        RegionId::Cn => HYPERGRYPH_GATEWAY,
        RegionId::Sg => GRYPHLINE_GATEWAY,
    }
}

pub fn launcher_appcode(region: RegionId, sub_channel: &ChannelId) -> &'static str {
    match region {
        RegionId::Cn => HYPERGRYPH_LAUNCHER_APPCODE,
        RegionId::Sg if sub_channel == &ChannelId::EPIC => EPIC_LAUNCHER_APPCODE,
        RegionId::Sg => GRYPHLINE_LAUNCHER_APPCODE,
    }
}

pub const fn local_low_vendor(region: RegionId) -> &'static str {
    match region {
        RegionId::Cn => HYPERGRYPH_LOCAL_LOW_VENDOR,
        RegionId::Sg => GRYPHLINE_LOCAL_LOW_VENDOR,
    }
}
