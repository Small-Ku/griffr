use crate::config::ApiTarget;

use super::{BatchRequest, CommonRequest, GetLatestGameRequest, ProxyRequest};

pub const BATCH_PROXY_PATH: &str = "/api/proxy/batch_proxy";
pub const WEB_BATCH_PROXY_PATH: &str = "/api/proxy/web/batch_proxy";
pub const LATEST_RESOURCES_PATH: &str = "/api/game/get_latest_resources";

pub const DEFAULT_BATCH_SEQUENCE: &str = "1";
pub const DEFAULT_PLATFORM: &str = "Windows";
pub const LAUNCHER_SOURCE: &str = "launcher";
pub const DEFAULT_LANGUAGE: &str = "zh-cn";

pub const USER_AGENT_HEADER: &str = "User-Agent";
pub const CONTENT_TYPE_HEADER: &str = "Content-Type";
pub const JSON_CONTENT_TYPE: &str = "application/json";
pub const RANGE_HEADER: &str = "Range";

pub const MIN_USER_AGENT: &str = "Mozilla/5.0";
pub const OFFICIAL_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) QtWebEngine/5.15.8 Chrome/92.0.4515.159 PC/WIN/HGSDK HGWebPC/1.30.1 Safari/537.36";

pub fn byte_range_from(offset: u64) -> String {
    format!("bytes={offset}-")
}

pub fn endpoint_url(gateway: &str, path: &str) -> String {
    format!("{}{}", gateway.trim_end_matches('/'), path)
}

pub fn batch_proxy_url(gateway: &str) -> String {
    endpoint_url(gateway, BATCH_PROXY_PATH)
}

pub fn web_batch_proxy_url(gateway: &str) -> String {
    endpoint_url(gateway, WEB_BATCH_PROXY_PATH)
}

pub fn latest_resources_url(gateway: &str) -> String {
    endpoint_url(gateway, LATEST_RESOURCES_PATH)
}

pub fn latest_game_batch(target: &ApiTarget, current_version: Option<&str>) -> BatchRequest {
    BatchRequest::new(vec![ProxyRequest::GetLatestGame {
        req: GetLatestGameRequest {
            appcode: target.game_appcode.clone(),
            channel: target.channels.channel().as_str().to_owned(),
            sub_channel: target.channels.sub_channel().as_str().to_owned(),
            version: current_version.unwrap_or_default().to_owned(),
            launcher_appcode: target.launcher_appcode.clone(),
        },
    }])
}

pub fn media_batch(target: &ApiTarget, language: &str) -> BatchRequest {
    let common = CommonRequest::new(
        target.game_appcode.clone(),
        language,
        target.channels.channel().as_str().to_owned(),
        target.channels.sub_channel().as_str().to_owned(),
    );

    BatchRequest::new(vec![
        ProxyRequest::GetBanner {
            req: common.clone(),
        },
        ProxyRequest::GetAnnouncement {
            req: common.clone(),
        },
        ProxyRequest::GetMainBgImage {
            req: common.clone(),
        },
        ProxyRequest::GetSidebar { req: common },
    ])
}

#[cfg(test)]
mod tests {
    use super::byte_range_from;

    #[test]
    fn byte_range_value_starts_at_requested_offset() {
        assert_eq!(byte_range_from(42), "bytes=42-");
    }
}
