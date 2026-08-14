use super::{GameId, RegionId};

/// Static facts intrinsic to a supported game and independent of a remote launcher backend.
#[derive(Debug, Clone)]
pub struct GameDefinition {
    pub id: GameId,
    pub exe_name: &'static str,
    pub data_root: &'static str,
    pub local_low_dir: &'static str,
}

impl GameDefinition {
    pub fn game_id(&self) -> GameId {
        self.id.clone()
    }
}

pub const HYPERGRYPH_LOCAL_LOW_VENDOR: &str = "Hypergryph";
pub const GRYPHLINE_LOCAL_LOW_VENDOR: &str = "Gryphline";
pub const YOSTAR_LOCAL_LOW_VENDOR: &str = "Yostar";

pub static GAME_DEFINITIONS: &[GameDefinition] = &[
    GameDefinition {
        id: GameId::ARKNIGHTS,
        exe_name: "Arknights.exe",
        data_root: "Arknights_Data",
        local_low_dir: "Arknights",
    },
    GameDefinition {
        id: GameId::ENDFIELD,
        exe_name: "Endfield.exe",
        data_root: "Endfield_Data",
        local_low_dir: "Endfield",
    },
];

pub fn game_definition(game: &GameId) -> Option<&'static GameDefinition> {
    GAME_DEFINITIONS.iter().find(|entry| &entry.id == game)
}

pub fn game_by_exe_name(name: &str) -> Option<GameId> {
    GAME_DEFINITIONS
        .iter()
        .find(|entry| entry.exe_name.eq_ignore_ascii_case(name))
        .map(GameDefinition::game_id)
}

pub const fn local_low_vendor(region: RegionId) -> &'static str {
    match region {
        RegionId::Cn => HYPERGRYPH_LOCAL_LOW_VENDOR,
        RegionId::Sg => GRYPHLINE_LOCAL_LOW_VENDOR,
        RegionId::Kr | RegionId::En | RegionId::Jp => YOSTAR_LOCAL_LOW_VENDOR,
    }
}
