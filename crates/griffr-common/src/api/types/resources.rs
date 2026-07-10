use super::core::{null_string_as_empty, PackFile};
use serde::{Deserialize, Serialize};
/// Patch information for delta updates
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchInfo {
    /// Base patch archive URL when the channel exposes a single-file view
    pub url: String,

    /// MD5 for the base patch archive field
    pub md5: String,

    /// Alternative file/package identifier used by the launcher API
    #[serde(rename = "file_id")]
    pub file_id: String,

    /// Archive password used for encrypted patch ZIP volumes
    #[serde(default)]
    pub cd_key: Option<String>,

    /// List of patch files to download
    pub patches: Vec<PackFile>,

    /// Total size in bytes (as string)
    #[serde(rename = "total_size")]
    pub total_size: String,

    /// Alternative total size field
    #[serde(rename = "package_size")]
    pub package_size: String,
}

/// Predownload patch information for staged future updates
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrePatchInfo {
    /// Target version that the staged patch will update to
    pub version: String,

    /// List of patch files to stage locally
    pub patches: Vec<PackFile>,

    /// Total download size in bytes (as string)
    #[serde(rename = "package_size")]
    pub package_size: String,

    /// Total installed size in bytes (as string)
    #[serde(rename = "total_size")]
    pub total_size: String,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    #[serde(default)]
    pub vfs_base_path: String,
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
    /// Extracted local file path within the patch payload, when the file ships directly.
    #[serde(default)]
    pub local_path: Option<String>,
    /// Diff type (1 = binary diff)
    #[serde(rename = "diffType", default)]
    pub diff_type: u64,
    /// Available patches from older versions
    #[serde(default)]
    pub patch: Vec<ResourcePatchDiff>,
}

/// A diff patch entry within a ResourcePatchEntry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourcePatchDiff {
    /// Old file relative path
    #[serde(rename = "base_file")]
    pub base_file: String,
    /// Alternate old file relative path, when the manifest uses `base_file_path`.
    #[serde(rename = "base_file_path", default)]
    pub base_file_path: Option<String>,
    /// Old file MD5
    #[serde(rename = "base_md5")]
    pub base_md5: String,
    /// Old file size
    #[serde(rename = "base_size")]
    pub base_size: u64,
    /// Patch filename (relative to {path}/Patch/)
    pub patch: String,
    /// Alternate patch payload path, when the manifest uses `patch_path`.
    #[serde(rename = "patch_path", default)]
    pub patch_path: Option<String>,
    /// Patch file size
    #[serde(rename = "patch_size")]
    pub patch_size: u64,
}
