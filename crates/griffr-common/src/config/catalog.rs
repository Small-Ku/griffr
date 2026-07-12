use super::{ChannelId, GameId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentRegion {
    Cn,
    Global,
}

#[derive(Debug, Clone)]
pub struct GameCatalogEntry {
    pub id: GameId,
    pub executable: &'static str,
    pub data_root: &'static str,
    pub local_low_dir: &'static str,
    pub cn_appcode: &'static str,
    pub global_appcode: &'static str,
}

impl GameCatalogEntry {
    pub fn game_id(&self) -> GameId {
        self.id.clone()
    }

    pub fn appcode(&self, region: DeploymentRegion) -> &'static str {
        match region {
            DeploymentRegion::Cn => self.cn_appcode,
            DeploymentRegion::Global => self.global_appcode,
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

pub static GAME_CATALOG: &[GameCatalogEntry] = &[
    GameCatalogEntry {
        id: GameId::ARKNIGHTS,
        executable: "Arknights.exe",
        data_root: "Arknights_Data",
        local_low_dir: "Arknights",
        cn_appcode: "GzD1CpaWgmSq1wew",
        global_appcode: "GzD1CpaWgmSq1wew",
    },
    GameCatalogEntry {
        id: GameId::ENDFIELD,
        executable: "Endfield.exe",
        data_root: "Endfield_Data",
        local_low_dir: "Endfield",
        cn_appcode: "6LL0KJuqHBVz33WK",
        global_appcode: "YDUTE5gscDZ229CW",
    },
];

pub fn game_catalog_entry(game: &GameId) -> Option<&'static GameCatalogEntry> {
    GAME_CATALOG.iter().find(|entry| &entry.id == game)
}

pub fn game_by_appcode(appcode: &str) -> Option<GameId> {
    GAME_CATALOG
        .iter()
        .find(|entry| entry.cn_appcode == appcode || entry.global_appcode == appcode)
        .map(GameCatalogEntry::game_id)
}

pub fn game_by_executable(executable: &str) -> Option<GameId> {
    GAME_CATALOG
        .iter()
        .find(|entry| entry.executable.eq_ignore_ascii_case(executable))
        .map(GameCatalogEntry::game_id)
}

pub fn deployment_region(channel: &ChannelId) -> DeploymentRegion {
    if channel == &ChannelId::HYPERGRYPH || channel == &ChannelId::BILIBILI {
        DeploymentRegion::Cn
    } else {
        DeploymentRegion::Global
    }
}

pub fn gateway(region: DeploymentRegion) -> &'static str {
    match region {
        DeploymentRegion::Cn => HYPERGRYPH_GATEWAY,
        DeploymentRegion::Global => GRYPHLINE_GATEWAY,
    }
}

pub fn launcher_appcode(region: DeploymentRegion, sub_channel: &ChannelId) -> &'static str {
    match region {
        DeploymentRegion::Cn => HYPERGRYPH_LAUNCHER_APPCODE,
        DeploymentRegion::Global if sub_channel == &ChannelId::EPIC_STORE => EPIC_LAUNCHER_APPCODE,
        DeploymentRegion::Global => GRYPHLINE_LAUNCHER_APPCODE,
    }
}

pub fn local_low_vendor(channel: &ChannelId) -> Option<&'static str> {
    if channel == &ChannelId::HYPERGRYPH || channel == &ChannelId::BILIBILI {
        Some(HYPERGRYPH_LOCAL_LOW_VENDOR)
    } else if channel == &ChannelId::GRYPHLINE
        || channel == &ChannelId::EPIC_STORE
        || channel == &ChannelId::GOOGLE_PLAY
    {
        Some(GRYPHLINE_LOCAL_LOW_VENDOR)
    } else {
        None
    }
}
