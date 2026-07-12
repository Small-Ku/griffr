use serde::{Deserialize, Serialize};

use crate::api::protocol::{DEFAULT_BATCH_SEQUENCE, DEFAULT_PLATFORM, LAUNCHER_SOURCE};

use super::resources::{
    AnnouncementResponse, BannerResponse, MainBgImageResponse, PatchInfo, PrePatchInfo,
    SidebarResponse,
};

pub(super) fn null_string_as_empty<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
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

impl BatchRequest {
    pub fn new(requests: Vec<ProxyRequest>) -> Self {
        Self {
            seq: DEFAULT_BATCH_SEQUENCE.to_owned(),
            requests,
        }
    }
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
            platform: DEFAULT_PLATFORM.to_owned(),
            source: LAUNCHER_SOURCE.to_owned(),
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

    /// Predownload patch info for staged future updates
    #[serde(rename = "pre_patch")]
    pub pre_patch: Option<PrePatchInfo>,

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

    /// Check if a predownload patch payload is available
    pub fn has_pre_patch_package(&self) -> bool {
        self.pre_patch
            .as_ref()
            .is_some_and(|patch| !patch.patches.is_empty())
    }
}

/// Package information for full installs
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Get the size as u64.
    pub fn size(&self) -> u64 {
        self.package_size.parse().unwrap_or(0)
    }

    /// Return the final URL path component without its query string.
    pub fn filename(&self) -> Option<&str> {
        let path = self
            .url
            .split_once('?')
            .map_or(self.url.as_str(), |(path, _)| path);
        path.rsplit('/').next().filter(|name| !name.is_empty())
    }

    /// Return the base name shared by a single `.zip` archive or any numeric
    /// multipart archive such as `.zip.001` or `.zip.12`.
    pub fn archive_base_name(&self) -> Option<&str> {
        self.archive_identity().map(|(base_name, _)| base_name)
    }

    /// Return the extraction order for this archive volume.
    ///
    /// A single `.zip` archive uses sequence zero. Numeric multipart suffixes
    /// use their parsed number, so `.zip.2` sorts before `.zip.12`.
    pub fn archive_sequence(&self) -> Option<u64> {
        self.archive_identity().map(|(_, sequence)| sequence)
    }

    fn archive_identity(&self) -> Option<(&str, u64)> {
        let filename = self.filename()?;
        if let Some(stem) = filename.strip_suffix(".zip") {
            return (!stem.is_empty()).then_some((stem, 0));
        }
        let (stem, part) = filename.rsplit_once(".zip.")?;
        if stem.is_empty() || part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        Some((stem, part.parse().ok()?))
    }
}
