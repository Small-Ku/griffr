use super::models::MediaResponse;
use crate::{
    api::{
        BatchRequest, BatchResponse, CommonRequest, GetLatestGameRequest, GetLatestGameResponse,
        GetLatestResourcesResponse, ProxyRequest, ProxyResponse,
    },
    config::ApiTarget,
    error::{Error, Result},
};

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("Resource pipeline unavailable: {0}")]
    ResourcePipelineUnavailable(String),
    #[error(transparent)]
    Other(#[from] crate::error::Error),
}

/// API client for Hypergryph game services
#[derive(Debug, Clone)]
pub struct ApiClient {
    pub(super) client: cyper::Client,
    pub(super) user_agent: String,
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

    /// Send a batch API request
    pub async fn batch_request(
        &self,
        gateway: &str,
        request: &BatchRequest,
    ) -> Result<BatchResponse> {
        let url = format!("{}/api/proxy/batch_proxy", gateway.trim_end_matches('/'));
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
            .post(url)?
            .header("User-Agent", &self.user_agent)
            .map_err(|e| Error::ApiClient(format!("Failed to set User-Agent header: {e}")))?
            .header("Content-Type", "application/json")
            .map_err(|e| Error::ApiClient(format!("Failed to set Content-Type header: {e}")))?
            .json(request)
            .map_err(|e| Error::ApiClient(format!("Failed to serialize batch request body: {e}")))?
            .send()
            .await
            .map_err(|e| Error::ApiClient(format!("Failed to send batch request to {url}: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(Error::ApiClient(format!(
                "API returned error {status}: {body}"
            )));
        }

        let batch_response = response
            .json::<BatchResponse>()
            .await
            .map_err(|e| Error::ApiClient(format!("Failed to parse batch response: {e}")))?;

        Ok(batch_response)
    }

    /// Get latest game version info
    pub async fn get_latest_game(
        &self,
        target: &ApiTarget,
        current_version: Option<&str>,
    ) -> Result<GetLatestGameResponse> {
        let request = BatchRequest {
            seq: "1".to_string(),
            requests: vec![ProxyRequest::GetLatestGame {
                req: GetLatestGameRequest {
                    appcode: target.game_appcode.0.clone(),
                    channel: target.channel.as_str().to_owned(),
                    sub_channel: target.sub_channel.as_str().to_owned(),
                    version: current_version.unwrap_or("").to_string(),
                    launcher_appcode: target.launcher_appcode.0.clone(),
                },
            }],
        };

        let response = self.batch_request(&target.gateway.0, &request).await?;

        response
            .responses
            .into_iter()
            .next()
            .and_then(|r| match r {
                ProxyResponse::GetLatestGame { rsp } => Some(rsp),
                _ => None,
            })
            .ok_or_else(|| Error::ApiClient("Missing get_latest_game response".to_string()))
    }

    /// Get media resources (banners, announcements, background)
    pub async fn get_media(&self, target: &ApiTarget, language: &str) -> Result<MediaResponse> {
        let common_req = CommonRequest::new(
            target.game_appcode.0.clone(),
            language,
            target.channel.as_str().to_owned(),
            target.sub_channel.as_str().to_owned(),
        );

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
        let url = format!(
            "{}/api/proxy/web/batch_proxy",
            target.gateway.0.trim_end_matches('/')
        );
        let response = self
            .batch_request_with_url(&url, &request)
            .await
            .map_err(|e| Error::ApiClient(format!("Media API request failed: {e}")))?;

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
        target: &ApiTarget,
        language: &str,
    ) -> Result<serde_json::Value> {
        let common_req = CommonRequest::new(
            target.game_appcode.0.clone(),
            language,
            target.channel.as_str().to_owned(),
            target.sub_channel.as_str().to_owned(),
        );

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

        let url = format!(
            "{}/api/proxy/web/batch_proxy",
            target.gateway.0.trim_end_matches('/')
        );
        let response = self
            .client
            .post(&url)?
            .header("User-Agent", &self.user_agent)
            .map_err(|e| Error::ApiClient(format!("Failed to set User-Agent header: {e}")))?
            .header("Content-Type", "application/json")
            .map_err(|e| Error::ApiClient(format!("Failed to set Content-Type header: {e}")))?
            .json(&request)
            .map_err(|e| {
                Error::ApiClient(format!("Failed to serialize media batch request body: {e}"))
            })?
            .send()
            .await
            .map_err(|e| {
                Error::ApiClient(format!("Failed to send media batch request to {url}: {e}"))
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(Error::ApiClient(format!(
                "Media API returned error {status}: {body}"
            )));
        }

        response.json::<serde_json::Value>().await.map_err(|e| {
            Error::ApiClient(format!("Failed to parse media batch response JSON: {e}"))
        })
    }

    /// Get latest game resources (VFS files) via the direct API endpoint
    pub async fn get_latest_resources(
        &self,
        target: &ApiTarget,
        game_version: &str,
        rand_str: &str,
        platform: &str,
    ) -> Result<GetLatestResourcesResponse, ApiError> {
        let api_base = target.gateway.0.trim_end_matches('/');
        let url = format!("{}/api/game/get_latest_resources", api_base);

        // Derive the version minor (major.minor) from the full version
        let version_minor = game_version
            .split('.')
            .take(2)
            .collect::<Vec<_>>()
            .join(".");

        let url = format!(
            "{}?appcode={}&game_version={}&version={}&platform={}&rand_str={}",
            url, target.game_appcode.0, version_minor, game_version, platform, rand_str
        );

        let response = self
            .client
            .get(&url)
            .map_err(|e| {
                Error::ApiClient(format!("Failed to build get_latest_resources request: {e}"))
            })
            .map_err(ApiError::Other)?
            .header("User-Agent", &self.user_agent)
            .map_err(|e| Error::ApiClient(format!("Failed to set User-Agent header: {e}")))
            .map_err(ApiError::Other)?
            .send()
            .await
            .map_err(|e| {
                Error::ApiClient(format!("Failed to get latest resources from {url}: {e}"))
            })
            .map_err(ApiError::Other)?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            if status.as_u16() == 400 && body.contains("resource not exist") {
                return Err(ApiError::ResourcePipelineUnavailable(body));
            }
            return Err(ApiError::Other(Error::ApiClient(format!(
                "get_latest_resources returned error {status}: {body}"
            ))));
        }

        let resources: GetLatestResourcesResponse = response
            .json()
            .await
            .map_err(|e| {
                Error::ApiClient(format!(
                    "Failed to parse get_latest_resources response: {e}"
                ))
            })
            .map_err(ApiError::Other)?;

        Ok(resources)
    }
}
