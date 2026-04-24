//! HTTP client for Hypergryph batch API

use anyhow::{Context, Result};
use md5::{Digest, Md5};
use std::io::ErrorKind;

use super::crypto;
use super::types::*;
use crate::config::{GameId, ServerId};

/// API client for Hypergryph game services
#[derive(Debug, Clone)]
pub struct ApiClient {
    client: cyper::Client,
    user_agent: String,
}

impl ApiClient {
    /// Minimum User-Agent that works with the API
    pub const MIN_USER_AGENT: &'static str = "Mozilla/5.0";

    /// Official launcher User-Agent
    pub const OFFICIAL_USER_AGENT: &'static str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) QtWebEngine/5.15.8 Chrome/92.0.4515.159 PC/WIN/HGSDK HGWebPC/1.30.1 Safari/537.36";

    /// Create a new API client
    pub fn new() -> Result<Self> {
        Self::with_user_agent(Self::MIN_USER_AGENT)
    }

    /// Create a new API client with a custom User-Agent
    pub fn with_user_agent(user_agent: impl Into<String>) -> Result<Self> {
        let user_agent = user_agent.into();

        let client = cyper::Client::new();

        Ok(Self { client, user_agent })
    }

    /// Get the base URL for the batch API with region
    fn batch_url(&self, game: Game, region: Region) -> String {
        match (game, region) {
            (Game::Arknights, Region::CN) => {
                "https://launcher.hypergryph.com/api/proxy/batch_proxy".to_string()
            }
            (Game::Arknights, Region::OS) => {
                "https://launcher.gryphline.com/api/proxy/batch_proxy".to_string()
            }
            (Game::Endfield, Region::CN) => {
                "https://launcher.hypergryph.com/api/proxy/batch_proxy".to_string()
            }
            (Game::Endfield, Region::OS) => {
                "https://launcher.gryphline.com/api/proxy/batch_proxy".to_string()
            }
        }
    }

    /// Get the base URL for the web batch API (media resources)
    fn web_batch_url(&self, game: Game, region: Region) -> String {
        match (game, region) {
            (Game::Arknights, Region::CN) => {
                "https://launcher.hypergryph.com/api/proxy/web/batch_proxy".to_string()
            }
            (Game::Arknights, Region::OS) => {
                "https://launcher.gryphline.com/api/proxy/web/batch_proxy".to_string()
            }
            (Game::Endfield, Region::CN) => {
                "https://launcher.hypergryph.com/api/proxy/web/batch_proxy".to_string()
            }
            (Game::Endfield, Region::OS) => {
                "https://launcher.gryphline.com/api/proxy/web/batch_proxy".to_string()
            }
        }
    }

    /// Determine the region from server ID
    fn region_for_server(server: ServerId) -> Region {
        match server {
            ServerId::CnOfficial | ServerId::CnBilibili => Region::CN,
            ServerId::GlobalOfficial | ServerId::GlobalEpic | ServerId::GlobalGoogleplay => {
                Region::OS
            }
        }
    }

    /// Send a batch API request
    pub async fn batch_request(
        &self,
        game: Game,
        region: Region,
        request: &BatchRequest,
    ) -> Result<BatchResponse> {
        let url = self.batch_url(game, region);
        self.batch_request_with_url(&url, request).await
    }

    /// Send a batch API request to a specific URL
    async fn batch_request_with_url(
        &self,
        url: &str,
        request: &BatchRequest,
    ) -> Result<BatchResponse> {
        let response = self
            .client
            .post(url)
            .context("Failed to build batch request")?
            .header("User-Agent", &self.user_agent)
            .context("Failed to set User-Agent header")?
            .header("Content-Type", "application/json")
            .context("Failed to set Content-Type header")?
            .json(request)
            .context("Failed to serialize batch request body")?
            .send()
            .await
            .with_context(|| format!("Failed to send batch request to {}", url))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("API returned error {}: {}", status, body);
        }

        let batch_response = response
            .json::<BatchResponse>()
            .await
            .context("Failed to parse batch response")?;

        Ok(batch_response)
    }

    /// Get latest game version info
    pub async fn get_latest_game(
        &self,
        game: GameId,
        server: ServerId,
        current_version: Option<&str>,
    ) -> Result<GetLatestGameResponse> {
        let (game_type, region) = match game {
            GameId::Arknights => (Game::Arknights, Region::CN),
            GameId::Endfield => (Game::Endfield, Self::region_for_server(server)),
        };

        let channel = ChannelConfig::for_game_server(game, server);
        let app_code = game_type.app_code(region).to_string();
        let launcher_app_code = game_type.launcher_app_code(region).to_string();

        let request = BatchRequest {
            seq: "1".to_string(),
            requests: vec![ProxyRequest::GetLatestGame {
                req: GetLatestGameRequest {
                    appcode: app_code,
                    channel: channel.channel.to_string(),
                    sub_channel: channel.sub_channel.to_string(),
                    version: current_version.unwrap_or("").to_string(),
                    launcher_appcode: launcher_app_code,
                },
            }],
        };

        let response = self.batch_request(game_type, region, &request).await?;

        response
            .responses
            .into_iter()
            .next()
            .and_then(|r| match r {
                ProxyResponse::GetLatestGame { rsp } => Some(rsp),
                _ => None,
            })
            .context("Missing get_latest_game response")
    }

    /// Get media resources (banners, announcements, background)
    pub async fn get_media(
        &self,
        game: GameId,
        server: ServerId,
        language: &str,
    ) -> Result<MediaResponse> {
        let (game_type, region) = match game {
            GameId::Arknights => (Game::Arknights, Region::CN),
            GameId::Endfield => (Game::Endfield, Self::region_for_server(server)),
        };

        let channel = ChannelConfig::for_game_server(game, server);
        let app_code = game_type.app_code(region).to_string();
        let common_req =
            CommonRequest::new(app_code, language, channel.channel, channel.sub_channel);

        let request = BatchRequest {
            seq: "1".to_string(),
            requests: vec![
                ProxyRequest::GetBanner {
                    req: common_req.clone(),
                },
                ProxyRequest::GetAnnouncement {
                    req: common_req.clone(),
                },
                ProxyRequest::GetMainBgImage {
                    req: common_req.clone(),
                },
                ProxyRequest::GetSidebar { req: common_req },
            ],
        };

        // Use web batch URL for media APIs
        let url = self.web_batch_url(game_type, region);
        let response = self
            .batch_request_with_url(&url, &request)
            .await
            .map_err(|e| anyhow::anyhow!("Media API request failed: {}", e))?;

        let mut media = MediaResponse::default();

        for proxy_response in response.responses {
            match proxy_response {
                ProxyResponse::GetBanner { rsp } => media.banners = Some(rsp),
                ProxyResponse::GetAnnouncement { rsp } => media.announcements = Some(rsp),
                ProxyResponse::GetMainBgImage { rsp } => media.background = Some(rsp),
                ProxyResponse::GetSidebar { rsp } => media.sidebar = Some(rsp),
                _ => {}
            }
        }

        Ok(media)
    }

    /// Get raw media batch response JSON as returned by the launcher web batch API.
    pub async fn get_media_raw(
        &self,
        game: GameId,
        server: ServerId,
        language: &str,
    ) -> Result<serde_json::Value> {
        let (game_type, region) = match game {
            GameId::Arknights => (Game::Arknights, Region::CN),
            GameId::Endfield => (Game::Endfield, Self::region_for_server(server)),
        };

        let channel = ChannelConfig::for_game_server(game, server);
        let app_code = game_type.app_code(region).to_string();
        let common_req =
            CommonRequest::new(app_code, language, channel.channel, channel.sub_channel);

        let request = BatchRequest {
            seq: "1".to_string(),
            requests: vec![
                ProxyRequest::GetBanner {
                    req: common_req.clone(),
                },
                ProxyRequest::GetAnnouncement {
                    req: common_req.clone(),
                },
                ProxyRequest::GetMainBgImage {
                    req: common_req.clone(),
                },
                ProxyRequest::GetSidebar { req: common_req },
            ],
        };

        let url = self.web_batch_url(game_type, region);
        let response = self
            .client
            .post(&url)
            .context("Failed to build media batch request")?
            .header("User-Agent", &self.user_agent)
            .context("Failed to set User-Agent header")?
            .header("Content-Type", "application/json")
            .context("Failed to set Content-Type header")?
            .json(&request)
            .context("Failed to serialize media batch request body")?
            .send()
            .await
            .with_context(|| format!("Failed to send media batch request to {}", url))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Media API returned error {}: {}", status, body);
        }

        response
            .json::<serde_json::Value>()
            .await
            .context("Failed to parse media batch response JSON")
    }

    /// Get latest game resources (VFS files) via the direct API endpoint
    ///
    /// This is a separate GET endpoint from the batch proxy API.
    /// It returns VFS resource metadata including index URLs.
    ///
    /// `rand_str` can be extracted from `pkg.file_path` of a prior `get_latest_game` call
    /// (the segment after the version number in `{version}_{randStr}/files`).
    pub async fn get_latest_resources(
        &self,
        game: GameId,
        server: ServerId,
        game_version: &str,
        rand_str: &str,
        platform: &str,
    ) -> Result<GetLatestResourcesResponse> {
        let (game_type, region) = match game {
            GameId::Arknights => (Game::Arknights, Region::CN),
            GameId::Endfield => (Game::Endfield, Self::region_for_server(server)),
        };

        // Determine the API base URL
        let api_base = match region {
            Region::CN => game_type.cn_gateway(),
            Region::OS => game_type.os_gateway(),
        };
        let url = format!("https://{}/api/game/get_latest_resources", api_base);

        // Derive the version minor (major.minor) from the full version
        let version_minor = game_version
            .split('.')
            .take(2)
            .collect::<Vec<_>>()
            .join(".");

        let app_code = game_type.app_code(region);

        let url = format!(
            "{}?appcode={}&game_version={}&version={}&platform={}&rand_str={}",
            url, app_code, version_minor, game_version, platform, rand_str
        );

        let response = self
            .client
            .get(&url)
            .context("Failed to build get_latest_resources request")?
            .header("User-Agent", &self.user_agent)
            .context("Failed to set User-Agent header")?
            .send()
            .await
            .with_context(|| format!("Failed to get latest resources from {}", url))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("get_latest_resources returned error {}: {}", status, body);
        }

        let resources: GetLatestResourcesResponse = response
            .json()
            .await
            .context("Failed to parse get_latest_resources response")?;

        Ok(resources)
    }

    /// Fetch and decrypt the game_files manifest
    pub async fn fetch_game_files(
        &self,
        base_url: &str,
        expected_md5: Option<&str>,
    ) -> Result<Vec<GameFileEntry>> {
        let url = format!("{}/game_files", base_url.trim_end_matches('/'));

        let response = self
            .client
            .get(&url)
            .context("Failed to build game_files request")?
            .header("User-Agent", &self.user_agent)
            .context("Failed to set User-Agent header")?
            .send()
            .await
            .with_context(|| format!("Failed to download game_files from {}", url))?;

        let status = response.status();
        if !status.is_success() {
            anyhow::bail!("Failed to download game_files: HTTP {}", status);
        }

        let encrypted_data = response
            .bytes()
            .await
            .context("Failed to read game_files response bytes")?;

        // Verify MD5 if provided
        if let Some(expected) = expected_md5 {
            let actual = format!("{:x}", Md5::digest(&encrypted_data));
            if actual != expected.to_lowercase() {
                anyhow::bail!(
                    "game_files MD5 mismatch: expected {}, got {}",
                    expected,
                    actual
                );
            }
        }

        // Decrypt
        let decrypted = crypto::decrypt_game_files(&encrypted_data)?;

        // Parse JSON Lines
        let mut entries = Vec::new();
        for line in decrypted.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let entry: GameFileEntry = serde_json::from_str(line)
                .with_context(|| format!("Failed to parse game_files entry: {}", line))?;
            entries.push(entry);
        }

        Ok(entries)
    }

    /// Fetch and decrypt a resource index file (index_main.json / index_initial.json)
    pub async fn fetch_res_index(&self, url: &str, key: &str) -> Result<ResIndex> {
        let response = self
            .client
            .get(url)
            .context("Failed to build resource index request")?
            .header("User-Agent", &self.user_agent)
            .context("Failed to set User-Agent header")?
            .send()
            .await
            .with_context(|| format!("Failed to download resource index from {}", url))?;

        let status = response.status();
        if !status.is_success() {
            anyhow::bail!("Failed to download resource index: HTTP {}", status);
        }

        let base64_data = response
            .text()
            .await
            .context("Failed to read resource index as text")?;

        // Decrypt
        let decrypted = crypto::decrypt_res_index(&base64_data, key)?;

        // Parse JSON
        let index: ResIndex = serde_json::from_str(&decrypted)
            .context("Failed to parse decrypted resource index JSON")?;

        Ok(index)
    }

    /// Fetch the resource patch manifest (patch.json)
    ///
    /// Unlike index files, patch.json is NOT encrypted — it's plain JSON.
    pub async fn fetch_res_patch(&self, url: &str) -> Result<ResourcePatch> {
        let response = self
            .client
            .get(url)
            .context("Failed to build resource patch request")?
            .header("User-Agent", &self.user_agent)
            .context("Failed to set User-Agent header")?
            .send()
            .await
            .with_context(|| format!("Failed to download resource patch from {}", url))?;

        let status = response.status();
        if !status.is_success() {
            anyhow::bail!("Failed to download resource patch: HTTP {}", status);
        }

        let patch: ResourcePatch = response
            .json()
            .await
            .context("Failed to parse resource patch JSON")?;

        Ok(patch)
    }

    /// Download a file with optional resume support
    pub async fn download_file(
        &self,
        url: &str,
        output_path: &std::path::Path,
        resume: bool,
    ) -> Result<String> {
        let mut request = self
            .client
            .get(url)
            .context("Failed to build download request")?
            .header("User-Agent", &self.user_agent)
            .context("Failed to set User-Agent header")?;

        let existing = if resume {
            match compio::fs::read(output_path).await {
                Ok(bytes) => {
                    if !bytes.is_empty() {
                        request = request
                            .header("Range", format!("bytes={}-", bytes.len()))
                            .context("Failed to set Range header")?;
                    }
                    bytes
                }
                Err(err) if err.kind() == ErrorKind::NotFound => Vec::new(),
                Err(err) => {
                    return Err(err)
                        .with_context(|| format!("Failed to read {}", output_path.display()))
                }
            }
        } else {
            Vec::new()
        };

        let response = request
            .send()
            .await
            .with_context(|| format!("Failed to download from {}", url))?;

        let status = response.status();
        if !status.is_success() && status.as_u16() != 206 {
            anyhow::bail!("Download returned error status: {}", status);
        }

        if let Some(parent) = output_path.parent() {
            compio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }

        let downloaded = response.bytes().await.context("Failed to download bytes")?;
        let final_bytes = if !existing.is_empty() && status.as_u16() == 206 {
            let mut merged = existing;
            merged.extend_from_slice(downloaded.as_ref());
            merged
        } else {
            downloaded.to_vec()
        };

        let write_result = compio::fs::write(output_path, final_bytes.clone()).await;
        write_result
            .0
            .with_context(|| format!("Failed to write output file {}", output_path.display()))?;

        let mut hasher = Md5::new();
        hasher.update(&final_bytes);
        Ok(format!("{:x}", hasher.finalize()))
    }

    /// Download a file and verify its MD5
    pub async fn download_file_with_verify(
        &self,
        url: &str,
        output_path: &std::path::Path,
        expected_md5: &str,
    ) -> Result<()> {
        let actual_md5 = self.download_file(url, output_path, false).await?;
        if actual_md5 != expected_md5.to_lowercase() {
            anyhow::bail!(
                "MD5 mismatch for {}: expected {}, got {}",
                url,
                expected_md5,
                actual_md5
            );
        }
        Ok(())
    }
}

impl Default for ApiClient {
    fn default() -> Self {
        Self::new().expect("Failed to create default API client")
    }
}

/// Aggregated media response
#[derive(Debug, Clone, Default)]
pub struct MediaResponse {
    pub banners: Option<BannerResponse>,
    pub announcements: Option<AnnouncementResponse>,
    pub background: Option<MainBgImageResponse>,
    pub sidebar: Option<SidebarResponse>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_client_creation() {
        let client = ApiClient::new();
        assert!(client.is_ok());
    }

    #[test]
    fn test_batch_url() {
        let client = ApiClient::new().unwrap();

        let ark_url = client.batch_url(Game::Arknights, Region::CN);
        assert!(ark_url.contains("hypergryph.com"));

        let ef_url = client.batch_url(Game::Endfield, Region::CN);
        assert!(ef_url.contains("hypergryph.com"));
    }
}
