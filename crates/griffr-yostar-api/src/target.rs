use griffr_core::RegionId;

pub const YOSTAR_KR_GATEWAY: &str = "https://api-launcher-kr.yo-star.com";
pub const YOSTAR_EN_GATEWAY: &str = "https://api-launcher-en.yo-star.com";
pub const YOSTAR_JP_GATEWAY: &str = "https://api-launcher-jp.yo-star.com";
pub const YOSTAR_ARKNIGHTS_KR_TAG: &str = "Arknights_KR";
pub const YOSTAR_ARKNIGHTS_EN_TAG: &str = "Arknights_EN";
pub const YOSTAR_ARKNIGHTS_JP_TAG: &str = "Arknights_JP";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YostarArknightsTarget {
    pub gateway: &'static str,
    pub game_tag: &'static str,
}

pub const fn yostar_arknights_target(region: RegionId) -> Option<YostarArknightsTarget> {
    let (gateway, game_tag) = match region {
        RegionId::Kr => (YOSTAR_KR_GATEWAY, YOSTAR_ARKNIGHTS_KR_TAG),
        RegionId::En => (YOSTAR_EN_GATEWAY, YOSTAR_ARKNIGHTS_EN_TAG),
        RegionId::Jp => (YOSTAR_JP_GATEWAY, YOSTAR_ARKNIGHTS_JP_TAG),
        RegionId::Cn | RegionId::Sg => return None,
    };
    Some(YostarArknightsTarget { gateway, game_tag })
}

pub fn yostar_region_from_game_tag(game_tag: &str) -> Option<RegionId> {
    match game_tag {
        YOSTAR_ARKNIGHTS_KR_TAG => Some(RegionId::Kr),
        YOSTAR_ARKNIGHTS_EN_TAG => Some(RegionId::En),
        YOSTAR_ARKNIGHTS_JP_TAG => Some(RegionId::Jp),
        _ => None,
    }
}
