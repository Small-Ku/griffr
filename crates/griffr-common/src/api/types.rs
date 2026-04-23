//! API request/response types for Hypergryph batch API

use serde::{Deserialize, Serialize};

fn null_string_as_empty<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

/// Game identifier for API requests
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Game {
    Arknights,
    Endfield,
}

impl Game {
    /// Get the CDN prefix for this game
    pub fn cdn_prefix(&self) -> &'static str {
        match self {
            Game::Arknights => "ak",
            Game::Endfield => "beyond",
        }
    }

    /// Get the API gateway domain for CN region
    pub fn cn_gateway(&self) -> &'static str {
        match self {
            Game::Arknights => "launcher.hypergryph.com",
            Game::Endfield => "launcher.hypergryph.com",
        }
    }

    /// Get the API gateway domain for OS (overseas) region
    pub fn os_gateway(&self) -> &'static str {
        match self {
            Game::Arknights => "launcher.gryphline.com", // Assumed
            Game::Endfield => "launcher.gryphline.com",
        }
    }

    /// Get the game app code
    pub fn app_code(&self, region: Region) -> &'static str {
        match (self, region) {
            (Game::Arknights, Region::CN) => "GzD1CpaWgmSq1wew",
            (Game::Endfield, Region::CN) => "6LL0KJuqHBVz33WK",
            (Game::Endfield, Region::OS) => "YDUTE5gscDZ229CW",
            (Game::Arknights, Region::OS) => "GzD1CpaWgmSq1wew", // Assumed same
        }
    }

    /// Get the launcher app code
    pub fn launcher_app_code(&self, region: Region) -> &'static str {
        match (self, region) {
            (Game::Arknights, Region::CN) => "abYeZZ16BPluCFyT",
            (Game::Endfield, Region::CN) => "abYeZZ16BPluCFyT",
            (Game::Endfield, Region::OS) => "TiaytKBUIEdoEwRT",
            (Game::Arknights, Region::OS) => "abYeZZ16BPluCFyT", // Assumed
        }
    }
}

/// Region identifier
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    CN,
    OS,
}

impl Region {
    /// Get the CDN domain suffix
    pub fn cdn_domain(&self, game: Game) -> &'static str {
        match (self, game) {
            (Region::CN, _) => ".hycdn.cn",
            (Region::OS, _) => ".hg-cdn.com",
        }
    }

    /// Get the API domain suffix
    pub fn api_domain_suffix(&self) -> &'static str {
        match self {
            Region::CN => ".hypergryph.com",
            Region::OS => ".gryphline.com",
        }
    }
}

/// Channel and sub-channel configuration
#[derive(Debug, Clone, Copy)]
pub struct ChannelConfig {
    pub channel: &'static str,
    pub sub_channel: &'static str,
}

impl ChannelConfig {
    const CN_OFFICIAL: Self = Self {
        channel: "1",
        sub_channel: "1",
    };

    const CN_BILIBILI: Self = Self {
        channel: "2",
        sub_channel: "2",
    };

    const GLOBAL_OFFICIAL: Self = Self {
        channel: "6",
        sub_channel: "6",
    };

    const GLOBAL_EPIC: Self = Self {
        channel: "6",
        sub_channel: "801",
    };

    /// Get channel configuration for a game/server pair.
    pub fn for_game_server(game: crate::config::GameId, server: crate::config::ServerId) -> Self {
        let _ = game;
        use crate::config::ServerId;
        match server {
            ServerId::CnOfficial => Self::CN_OFFICIAL,
            ServerId::CnBilibili => Self::CN_BILIBILI,
            ServerId::GlobalOfficial => Self::GLOBAL_OFFICIAL,
            ServerId::GlobalEpic => Self::GLOBAL_EPIC,
        }
    }
}

/// Extract rand_str from a CDN path (pkg.file_path format).
///
/// The path format is: `.../{version}_{randStr}/files`
/// Returns the randStr portion, or None if the pattern doesn't match.
fn extract_rand_str_from_path(path: &str) -> Option<String> {
    let segment = path.split('/').nth_back(1)?; // Gets the "{version}_{randStr}" segment
    extract_rand_str_from_segment(segment)
}

/// Extract rand_str from a CDN URL (patch.url format).
///
/// The URL contains a `{version}_{randStr}` segment somewhere in the path.
/// Returns the randStr portion, or None if the pattern doesn't match.
fn extract_rand_str_from_url(url: &str) -> Option<String> {
    // Strip query string
    let path = url.split('?').next()?;
    // Try each path segment
    for segment in path.split('/') {
        if let Some(rand) = extract_rand_str_from_segment(segment) {
            return Some(rand);
        }
    }
    None
}

/// Extract rand_str from a path segment like "{version}_{randStr}".
///
/// The version uses dots (e.g., "1.1.9") and the rand_str is alphanumeric.
/// We find the last underscore and validate the candidate.
fn extract_rand_str_from_segment(segment: &str) -> Option<String> {
    let last_underscore = segment.rfind('_')?;
    let candidate = &segment[last_underscore + 1..];
    // Validate: rand_str contains only alphanumeric chars
    if candidate.chars().all(|c| c.is_alphanumeric()) && !candidate.is_empty() {
        Some(candidate.to_string())
    } else {
        None
    }
}

/// Batch API request
#[derive(Debug, Clone, Serialize)]
pub struct BatchRequest {
    /// Request sequence ID (can be "1" for API calls)
    pub seq: String,

    /// Proxy requests to batch
    #[serde(rename = "proxy_reqs")]
    pub requests: Vec<ProxyRequest>,
}

/// Individual proxy request in a batch
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind")]
pub enum ProxyRequest {
    #[serde(rename = "get_latest_game")]
    GetLatestGame {
        #[serde(rename = "get_latest_game_req")]
        req: GetLatestGameRequest,
    },
    #[serde(rename = "get_banner")]
    GetBanner {
        #[serde(rename = "get_banner_req")]
        req: CommonRequest,
    },
    #[serde(rename = "get_announcement")]
    GetAnnouncement {
        #[serde(rename = "get_announcement_req")]
        req: CommonRequest,
    },
    #[serde(rename = "get_main_bg_image")]
    GetMainBgImage {
        #[serde(rename = "get_main_bg_image_req")]
        req: CommonRequest,
    },
    #[serde(rename = "get_sidebar")]
    GetSidebar {
        #[serde(rename = "get_sidebar_req")]
        req: CommonRequest,
    },
}

/// Get latest game request
#[derive(Debug, Clone, Serialize)]
pub struct GetLatestGameRequest {
    pub appcode: String,
    pub channel: String,
    pub sub_channel: String,
    pub version: String,
    pub launcher_appcode: String,
}

/// Common request for media resources
#[derive(Debug, Clone, Serialize)]
pub struct CommonRequest {
    pub appcode: String,
    pub language: String,
    pub channel: String,
    pub sub_channel: String,
    pub platform: String,
    pub source: String,
}

impl CommonRequest {
    pub fn new(
        appcode: impl Into<String>,
        language: impl Into<String>,
        channel: impl Into<String>,
        sub_channel: impl Into<String>,
    ) -> Self {
        Self {
            appcode: appcode.into(),
            language: language.into(),
            channel: channel.into(),
            sub_channel: sub_channel.into(),
            platform: "Windows".to_string(),
            source: "launcher".to_string(),
        }
    }
}

/// Batch API response
#[derive(Debug, Clone, Deserialize)]
pub struct BatchResponse {
    #[serde(rename = "proxy_rsps")]
    pub responses: Vec<ProxyResponse>,
}

/// Individual proxy response
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind")]
#[allow(clippy::large_enum_variant)]
pub enum ProxyResponse {
    #[serde(rename = "get_latest_game")]
    GetLatestGame {
        #[serde(rename = "get_latest_game_rsp")]
        rsp: GetLatestGameResponse,
    },
    #[serde(rename = "get_banner")]
    GetBanner {
        #[serde(rename = "get_banner_rsp")]
        rsp: BannerResponse,
    },
    #[serde(rename = "get_announcement")]
    GetAnnouncement {
        #[serde(rename = "get_announcement_rsp")]
        rsp: AnnouncementResponse,
    },
    #[serde(rename = "get_main_bg_image")]
    GetMainBgImage {
        #[serde(rename = "get_main_bg_image_rsp")]
        rsp: MainBgImageResponse,
    },
    #[serde(rename = "get_sidebar")]
    GetSidebar {
        #[serde(rename = "get_sidebar_rsp")]
        rsp: SidebarResponse,
    },
}

/// Get latest game response
#[derive(Debug, Clone, Deserialize)]
pub struct GetLatestGameResponse {
    /// Action code reported by the launcher API.
    ///
    /// Observed Endfield responses can include both `pkg` and `patch` payloads even when
    /// `action == 1`, so callers should inspect the payload fields directly instead of
    /// relying on `action` alone to decide between patch vs full reinstall.
    pub action: i32,

    /// The version string that was requested
    #[serde(rename = "request_version")]
    pub request_version: String,

    /// Latest available version
    pub version: String,

    /// Package info for full install
    pub pkg: Option<PackageInfo>,

    /// Patch info for delta updates
    pub patch: Option<PatchInfo>,

    /// State code (usually 0)
    pub state: i32,

    /// Launcher action code
    #[serde(rename = "launcher_action")]
    pub launcher_action: i32,
}

impl GetLatestGameResponse {
    /// Check if an update is available
    pub fn has_update(&self) -> bool {
        self.action == 1 || self.action == 2
    }

    /// Extract the rand_str from pkg.file_path or patch URL.
    ///
    /// The file_path format is: `.../{version}_{randStr}/files`
    /// The patch URL format contains a similar `{version}_{randStr}` segment.
    ///
    /// Returns the randStr portion, or empty string if unavailable from both sources.
    pub fn rand_str(&self) -> String {
        // Try pkg.file_path first (available for full package updates)
        if let Some(rand) = self
            .pkg
            .as_ref()
            .and_then(|pkg| extract_rand_str_from_path(&pkg.file_path))
        {
            return rand;
        }
        // Fall back to patch URL (available for patch-only updates)
        if let Some(rand) = self
            .patch
            .as_ref()
            .and_then(|patch| extract_rand_str_from_url(&patch.url))
        {
            return rand;
        }
        String::new()
    }

    /// Check if a full package payload is available
    pub fn has_full_package(&self) -> bool {
        self.pkg.as_ref().is_some_and(|pkg| !pkg.packs.is_empty())
    }

    /// Check if a delta patch payload is available
    pub fn has_patch_package(&self) -> bool {
        self.patch
            .as_ref()
            .is_some_and(|patch| !patch.patches.is_empty())
    }

    /// Legacy helper retained for compatibility.
    pub fn is_full_install(&self) -> bool {
        self.action == 1
    }

    /// Legacy helper retained for compatibility.
    pub fn is_patch(&self) -> bool {
        self.action == 2
    }
}

/// Package information for full installs
#[derive(Debug, Clone, Deserialize)]
pub struct PackageInfo {
    /// List of pack files to download
    pub packs: Vec<PackFile>,

    /// Total size in bytes (as string)
    #[serde(rename = "total_size")]
    pub total_size: String,

    /// Base path for files.json and game_files
    #[serde(rename = "file_path")]
    pub file_path: String,

    /// MD5 hash of the encrypted game_files manifest
    #[serde(rename = "game_files_md5")]
    pub game_files_md5: Option<String>,
}

/// Individual pack file
#[derive(Debug, Clone, Deserialize)]
pub struct PackFile {
    /// Download URL
    pub url: String,

    /// MD5 hash of the file
    pub md5: String,

    /// Package size in bytes (as string)
    #[serde(rename = "package_size")]
    pub package_size: String,
}

impl PackFile {
    /// Get the size as u64
    pub fn size(&self) -> u64 {
        self.package_size.parse().unwrap_or(0)
    }

    /// Get the filename from the URL
    pub fn filename(&self) -> Option<&str> {
        self.url.split('/').next_back()
    }
}

/// Patch information for delta updates
#[derive(Debug, Clone, Deserialize)]
pub struct PatchInfo {
    /// Base patch archive URL when the server exposes a single-file view
    pub url: String,

    /// MD5 for the base patch archive field
    pub md5: String,

    /// Alternative file/package identifier used by the launcher API
    #[serde(rename = "file_id")]
    pub file_id: String,

    /// List of patch files to download
    pub patches: Vec<PackFile>,

    /// Total size in bytes (as string)
    #[serde(rename = "total_size")]
    pub total_size: String,

    /// Alternative total size field
    #[serde(rename = "package_size")]
    pub package_size: String,
}

/// Banner response
#[derive(Debug, Clone, Deserialize)]
pub struct BannerResponse {
    /// Data version
    #[serde(rename = "data_version")]
    pub data_version: String,

    pub banners: Vec<Banner>,
}

/// Individual banner
#[derive(Debug, Clone, Deserialize)]
pub struct Banner {
    /// Banner ID
    pub id: String,

    /// Banner image URL
    pub url: String,

    /// MD5 hash of the image
    pub md5: String,

    /// Jump URL when clicked
    #[serde(rename = "jump_url")]
    pub jump_url: String,

    /// Whether authentication token is required
    #[serde(rename = "need_token")]
    pub need_token: bool,
}

/// Announcement response
#[derive(Debug, Clone, Deserialize)]
pub struct AnnouncementResponse {
    /// Data version
    #[serde(rename = "data_version")]
    pub data_version: String,

    pub tabs: Vec<AnnouncementTab>,
}

/// Announcement tab
#[derive(Debug, Clone, Deserialize)]
pub struct AnnouncementTab {
    #[serde(rename = "tabName")]
    pub tab_name: String,
    pub announcements: Vec<Announcement>,
}

/// Individual announcement
#[derive(Debug, Clone, Deserialize)]
pub struct Announcement {
    /// Announcement ID
    pub id: String,

    /// Announcement title/content
    pub content: String,

    /// Jump URL for full content
    #[serde(rename = "jump_url")]
    pub jump_url: String,

    /// Start timestamp (Unix timestamp in milliseconds)
    #[serde(rename = "start_ts")]
    pub start_ts: String,

    /// Whether authentication token is required
    #[serde(rename = "need_token")]
    pub need_token: bool,
}

/// Main background image response
#[derive(Debug, Clone, Deserialize)]
pub struct MainBgImageResponse {
    /// Data version
    #[serde(rename = "data_version")]
    pub data_version: String,

    /// Main background image info
    #[serde(rename = "main_bg_image")]
    pub main_bg_image: MainBgImage,
}

/// Main background image info
#[derive(Debug, Clone, Deserialize)]
pub struct MainBgImage {
    /// Background image URL
    pub url: String,

    /// MD5 hash of the image
    pub md5: String,

    /// Video URL (often empty)
    #[serde(rename = "video_url")]
    pub video_url: String,
}

/// Sidebar response
#[derive(Debug, Clone, Deserialize)]
pub struct SidebarResponse {
    /// Data version
    #[serde(rename = "data_version")]
    pub data_version: String,

    pub sidebars: Vec<SidebarItem>,
}

/// Individual sidebar item
#[derive(Debug, Clone, Deserialize)]
pub struct SidebarItem {
    /// Display type
    #[serde(rename = "display_type")]
    pub display_type: String,

    /// Media type identifier
    pub media: String,

    /// Picture/icon info (can be null)
    pub pic: Option<SidebarPic>,

    /// Jump URL
    #[serde(rename = "jump_url")]
    pub jump_url: String,

    /// Labels for this sidebar item
    #[serde(rename = "sidebar_labels")]
    pub sidebar_labels: Vec<SidebarLabel>,

    /// Grid info (optional)
    #[serde(rename = "grid_info")]
    pub grid_info: Option<serde_json::Value>,

    /// Whether authentication token is required
    #[serde(rename = "need_token")]
    pub need_token: bool,
}

/// Sidebar picture info
#[derive(Debug, Clone, Deserialize)]
pub struct SidebarPic {
    pub url: String,
    pub md5: String,
    pub description: String,
}

/// Sidebar label
#[derive(Debug, Clone, Deserialize)]
pub struct SidebarLabel {
    pub content: String,
    #[serde(rename = "jump_url")]
    pub jump_url: String,
}

/// Entry in the decrypted game_files manifest
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameFileEntry {
    /// Relative path from game root
    pub path: String,
    /// MD5 hash of the file
    pub md5: String,
    /// File size in bytes
    pub size: u64,
}

/// Decrypted resource index (index_main.json / index_initial.json)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResIndex {
    #[serde(default, deserialize_with = "null_string_as_empty")]
    pub version: String,
    #[serde(default, deserialize_with = "null_string_as_empty")]
    pub path: String,
    pub files: Vec<ResIndexFile>,
}

/// Individual file in resource index
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResIndexFile {
    #[serde(default)]
    pub index: u64,
    #[serde(default, deserialize_with = "null_string_as_empty")]
    pub name: String,
    #[serde(default)]
    pub hash: Option<String>,
    pub size: u64,
    #[serde(default)]
    pub r#type: u64,
    #[serde(default)]
    pub md5: Option<String>,
    #[serde(default)]
    pub manifest: u64,
}

/// Response from /game/get_latest_resources API
#[derive(Debug, Clone, Deserialize)]
pub struct GetLatestResourcesResponse {
    /// Available resource groups (main, initial)
    pub resources: Vec<GameResource>,

    /// JSON-encoded config string
    pub configs: String,

    /// Resource version string (e.g., "initial_6331530-16_main_6331530-16")
    #[serde(rename = "res_version")]
    pub res_version: String,

    /// Path to patch index (often empty)
    #[serde(rename = "patch_index_path")]
    pub patch_index_path: String,

    /// CDN domain base URL
    pub domain: String,
}

/// A resource group in the get_latest_resources response
#[derive(Debug, Clone, Deserialize)]
pub struct GameResource {
    /// Resource group name ("main" or "initial")
    pub name: String,

    /// Resource version (e.g., "6331530-16")
    pub version: String,

    /// Base URL for resource files
    pub path: String,
}

/// Patch manifest (patch.json) for incremental VFS updates
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourcePatch {
    #[serde(default)]
    pub version: String,
    pub files: Vec<ResourcePatchEntry>,
}

/// An entry in the resource patch manifest
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourcePatchEntry {
    /// New file relative path (e.g., "VFS/0CE8FA57/8A8746477A4254C6069BCC7124B229A2.chk")
    pub name: String,
    /// New file MD5
    pub md5: String,
    /// New file size
    pub size: u64,
    /// Diff type (1 = binary diff)
    #[serde(rename = "diffType", default)]
    pub diff_type: u64,
    /// Available patches from older versions
    pub patch: Vec<ResourcePatchDiff>,
}

/// A diff patch entry within a ResourcePatchEntry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourcePatchDiff {
    /// Old file relative path
    #[serde(rename = "base_file")]
    pub base_file: String,
    /// Old file MD5
    #[serde(rename = "base_md5")]
    pub base_md5: String,
    /// Old file size
    #[serde(rename = "base_size")]
    pub base_size: u64,
    /// Patch filename (relative to {path}/Patch/)
    pub patch: String,
    /// Patch file size
    #[serde(rename = "patch_size")]
    pub patch_size: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_game_app_codes() {
        assert_eq!(Game::Arknights.app_code(Region::CN), "GzD1CpaWgmSq1wew");
        assert_eq!(Game::Endfield.app_code(Region::CN), "6LL0KJuqHBVz33WK");
        assert_eq!(Game::Endfield.app_code(Region::OS), "YDUTE5gscDZ229CW");
    }

    #[test]
    fn test_launcher_app_codes() {
        assert_eq!(
            Game::Arknights.launcher_app_code(Region::CN),
            "abYeZZ16BPluCFyT"
        );
        assert_eq!(
            Game::Endfield.launcher_app_code(Region::CN),
            "abYeZZ16BPluCFyT"
        );
        assert_eq!(
            Game::Endfield.launcher_app_code(Region::OS),
            "TiaytKBUIEdoEwRT"
        );
    }

    #[test]
    fn test_cdn_prefixes() {
        assert_eq!(Game::Arknights.cdn_prefix(), "ak");
        assert_eq!(Game::Endfield.cdn_prefix(), "beyond");
    }

    #[test]
    fn test_region_domain_suffixes() {
        assert_eq!(Region::CN.api_domain_suffix(), ".hypergryph.com");
        assert_eq!(Region::OS.api_domain_suffix(), ".gryphline.com");
        assert_eq!(Region::CN.cdn_domain(Game::Arknights), ".hycdn.cn");
        assert_eq!(Region::OS.cdn_domain(Game::Endfield), ".hg-cdn.com");
    }

    #[test]
    fn test_channel_config() {
        use crate::config::{GameId, ServerId};
        let official = ChannelConfig::for_game_server(GameId::Endfield, ServerId::CnOfficial);
        assert_eq!(official.channel, "1");
        assert_eq!(official.sub_channel, "1");

        let bilibili = ChannelConfig::for_game_server(GameId::Arknights, ServerId::CnBilibili);
        assert_eq!(bilibili.channel, "2");
        assert_eq!(bilibili.sub_channel, "2");

        let endfield_bilibili =
            ChannelConfig::for_game_server(GameId::Endfield, ServerId::CnBilibili);
        assert_eq!(endfield_bilibili.channel, "2");
        assert_eq!(endfield_bilibili.sub_channel, "2");

        let global = ChannelConfig::for_game_server(GameId::Endfield, ServerId::GlobalOfficial);
        assert_eq!(global.channel, "6");
        assert_eq!(global.sub_channel, "6");

        let epic = ChannelConfig::for_game_server(GameId::Endfield, ServerId::GlobalEpic);
        assert_eq!(epic.channel, "6");
        assert_eq!(epic.sub_channel, "801");
    }

    #[test]
    fn test_pack_file_parsing() {
        let json = r#"{
            "url": "https://beyond.hycdn.cn/pack.zip.001?auth_key=xxx",
            "md5": "abc123",
            "package_size": "1073741824"
        }"#;

        let pack: PackFile = serde_json::from_str(json).unwrap();
        assert_eq!(pack.size(), 1073741824);
        assert_eq!(pack.filename(), Some("pack.zip.001?auth_key=xxx"));
        assert_eq!(pack.md5, "abc123");
    }

    #[test]
    fn test_get_latest_game_response_helpers() {
        let no_update = GetLatestGameResponse {
            action: 0,
            request_version: "1.0.0".to_string(),
            version: "1.0.0".to_string(),
            pkg: None,
            patch: None,
            state: 0,
            launcher_action: 0,
        };
        assert!(!no_update.has_update());
        assert!(!no_update.is_full_install());
        assert!(!no_update.is_patch());
        assert!(!no_update.has_full_package());
        assert!(!no_update.has_patch_package());

        let full_update = GetLatestGameResponse {
            action: 1,
            request_version: "".to_string(),
            version: "1.1.0".to_string(),
            pkg: Some(PackageInfo {
                packs: vec![PackFile {
                    url: "https://example.com/full.zip.001".to_string(),
                    md5: "abc".to_string(),
                    package_size: "1000".to_string(),
                }],
                total_size: "1000".to_string(),
                file_path: "/files".to_string(),
                game_files_md5: None,
            }),
            patch: None,
            state: 0,
            launcher_action: 0,
        };
        assert!(full_update.has_update());
        assert!(full_update.is_full_install());
        assert!(!full_update.is_patch());
        assert!(full_update.has_full_package());
        assert!(!full_update.has_patch_package());

        let patch_update = GetLatestGameResponse {
            action: 2,
            request_version: "1.0.0".to_string(),
            version: "1.0.1".to_string(),
            pkg: None,
            patch: Some(PatchInfo {
                url: "https://example.com/patch.zip".to_string(),
                md5: "abc123".to_string(),
                file_id: "1".to_string(),
                patches: vec![PackFile {
                    url: "https://example.com/patch.zip.001".to_string(),
                    md5: "abc123".to_string(),
                    package_size: "100".to_string(),
                }],
                total_size: "100".to_string(),
                package_size: "100".to_string(),
            }),
            state: 0,
            launcher_action: 0,
        };
        assert!(patch_update.has_update());
        assert!(!patch_update.is_full_install());
        assert!(patch_update.is_patch());
        assert!(!patch_update.has_full_package());
        assert!(patch_update.has_patch_package());
    }

    #[test]
    fn test_common_request_serialization() {
        let req = CommonRequest::new("test_appcode", "zh-cn", "1", "1");
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("test_appcode"));
        assert!(json.contains("zh-cn"));
        assert!(json.contains("Windows"));
        assert!(json.contains("launcher"));
        assert!(!json.contains("common_req"));
    }

    #[test]
    fn test_banner_response_parsing() {
        let json = r#"{
            "data_version": "v1",
            "banners": [
                {
                    "id": "77",
                    "url": "https://example.com/banner.png",
                    "md5": "abc123",
                    "jump_url": "https://example.com/link",
                    "need_token": true
                }
            ]
        }"#;

        let rsp: BannerResponse = serde_json::from_str(json).unwrap();
        assert_eq!(rsp.data_version, "v1");
        assert_eq!(rsp.banners.len(), 1);
        assert_eq!(rsp.banners[0].id, "77");
        assert_eq!(rsp.banners[0].url, "https://example.com/banner.png");
        assert_eq!(rsp.banners[0].md5, "abc123");
        assert_eq!(rsp.banners[0].jump_url, "https://example.com/link");
        assert!(rsp.banners[0].need_token);
    }

    #[test]
    fn test_main_bg_image_response_parsing() {
        let json = r#"{
            "data_version": "v1",
            "main_bg_image": {
                "url": "https://example.com/bg.webp",
                "md5": "def456",
                "video_url": ""
            }
        }"#;

        let rsp: MainBgImageResponse = serde_json::from_str(json).unwrap();
        assert_eq!(rsp.data_version, "v1");
        assert_eq!(rsp.main_bg_image.url, "https://example.com/bg.webp");
        assert_eq!(rsp.main_bg_image.md5, "def456");
        assert_eq!(rsp.main_bg_image.video_url, "");
    }

    #[test]
    fn test_sidebar_response_parsing() {
        let json = r#"{
            "data_version": "v1",
            "sidebars": [
                {
                    "display_type": "DisplayType_RESERVE",
                    "media": "Bilibili",
                    "pic": null,
                    "jump_url": "https://space.bilibili.com",
                    "sidebar_labels": [],
                    "grid_info": null,
                    "need_token": true
                },
                {
                    "display_type": "DisplayType_RESERVE",
                    "media": "Weibo",
                    "pic": {
                        "url": "https://example.com/icon.png",
                        "md5": "abc123",
                        "description": "Weibo Icon"
                    },
                    "jump_url": "https://weibo.com",
                    "sidebar_labels": [
                        {
                            "content": "Official",
                            "jump_url": "https://weibo.com/official"
                        }
                    ],
                    "grid_info": null,
                    "need_token": false
                }
            ]
        }"#;

        let rsp: SidebarResponse = serde_json::from_str(json).unwrap();
        assert_eq!(rsp.data_version, "v1");
        assert_eq!(rsp.sidebars.len(), 2);
        assert_eq!(rsp.sidebars[0].media, "Bilibili");
        assert!(rsp.sidebars[0].pic.is_none());
        assert!(rsp.sidebars[0].need_token);
        assert_eq!(rsp.sidebars[1].media, "Weibo");
        assert!(rsp.sidebars[1].pic.is_some());
        let pic = rsp.sidebars[1].pic.as_ref().unwrap();
        assert_eq!(pic.description, "Weibo Icon");
        assert_eq!(rsp.sidebars[1].sidebar_labels.len(), 1);
        assert_eq!(rsp.sidebars[1].sidebar_labels[0].content, "Official");
    }

    #[test]
    fn test_announcement_response_parsing() {
        let json = r#"{
            "data_version": "v1",
            "tabs": [
                {
                    "tabName": "公告",
                    "tab_id": "30",
                    "announcements": [
                        {
                            "id": "133",
                            "content": "Update Notice",
                            "jump_url": "https://example.com/news",
                            "start_ts": "1775466000000",
                            "need_token": true
                        }
                    ]
                }
            ]
        }"#;

        let rsp: AnnouncementResponse = serde_json::from_str(json).unwrap();
        assert_eq!(rsp.data_version, "v1");
        assert_eq!(rsp.tabs.len(), 1);
        assert_eq!(rsp.tabs[0].tab_name, "公告");
        assert_eq!(rsp.tabs[0].announcements.len(), 1);
        assert_eq!(rsp.tabs[0].announcements[0].id, "133");
        assert_eq!(rsp.tabs[0].announcements[0].content, "Update Notice");
        assert!(rsp.tabs[0].announcements[0].need_token);
    }

    #[test]
    fn test_game_file_entry() {
        let json = r#"{"path":"Endfield.exe","md5":"abc123","size":826424}"#;
        let entry: GameFileEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.path, "Endfield.exe");
        assert_eq!(entry.md5, "abc123");
        assert_eq!(entry.size, 826424);
    }
}
