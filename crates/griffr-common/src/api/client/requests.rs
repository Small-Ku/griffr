use super::media::MediaResponse;
use crate::{
    api::{
        protocol::{
            batch_proxy_url, latest_game_batch, latest_resources_url, media_batch,
            web_batch_proxy_url, CONTENT_TYPE_HEADER, JSON_CONTENT_TYPE,
            MIN_USER_AGENT as PROTOCOL_MIN_USER_AGENT,
            OFFICIAL_USER_AGENT as PROTOCOL_OFFICIAL_USER_AGENT, USER_AGENT_HEADER,
        },
        BatchRequest, BatchResponse, GetLatestGameResponse, GetLatestResourcesResponse,
        ProxyResponse,
    },
    config::ApiTarget,
    error::{Error, Result},
};

/// API client for Hypergryph game services
#[derive(Debug, Clone)]
pub struct ApiClient {
    pub(super) client: cyper::Client,
    pub(super) user_agent: String,
}

impl ApiClient {
    /// Minimum User-Agent that works with the API
    pub const MIN_USER_AGENT: &'static str = PROTOCOL_MIN_USER_AGENT;

    /// Official launcher User-Agent
    pub const OFFICIAL_USER_AGENT: &'static str = PROTOCOL_OFFICIAL_USER_AGENT;

    /// Create a new API client
    pub fn new() -> Result<Self> {
        Self::with_user_agent(Self::MIN_USER_AGENT)
    }

    /// Create a new API client with a custom User-Agent
    pub fn with_user_agent(user_agent: impl Into<String>) -> Result<Self> {
        let user_agent = user_agent.into();

        let client = cyper::Client::new()?;

        Ok(Self { client, user_agent })
    }

    /// Send a batch API request
    pub async fn batch_request(
        &self,
        gateway: &str,
        request: &BatchRequest,
    ) -> Result<BatchResponse> {
        let url = batch_proxy_url(gateway);
        self.batch_request_with_url(&url, request).await
    }

    /// Send a batch API request to a specific URL
    async fn batch_request_with_url<T>(&self, url: &str, request: &BatchRequest) -> Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let response = self
            .client
            .post(url)?
            .header(USER_AGENT_HEADER, &self.user_agent)
            .map_err(|e| Error::Message {
                context: "API client wrapper error: ",
                detail: format!("Failed to set User-Agent header: {e}"),
            })?
            .header(CONTENT_TYPE_HEADER, JSON_CONTENT_TYPE)
            .map_err(|e| Error::Message {
                context: "API client wrapper error: ",
                detail: format!("Failed to set Content-Type header: {e}"),
            })?
            .json(request)
            .map_err(|e| Error::Message {
                context: "API client wrapper error: ",
                detail: format!("Failed to serialize batch request body: {e}"),
            })?
            .send()
            .await
            .map_err(|e| Error::Message {
                context: "API client wrapper error: ",
                detail: format!("Failed to send batch request to {url}: {e}"),
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Message {
                context: "API client wrapper error: ",
                detail: format!("API returned error {status}: {body}"),
            });
        }

        response.json::<T>().await.map_err(|e| Error::Message {
            context: "API client wrapper error: ",
            detail: format!("Failed to parse batch response: {e}"),
        })
    }

    /// Get latest game version info
    pub async fn get_latest_game(
        &self,
        target: &ApiTarget,
        current_version: Option<&str>,
    ) -> Result<GetLatestGameResponse> {
        let request = latest_game_batch(target, current_version);

        let response = self.batch_request(&target.gateway, &request).await?;

        response
            .responses
            .into_iter()
            .next()
            .and_then(|r| match r {
                ProxyResponse::GetLatestGame { rsp } => Some(rsp),
                _ => None,
            })
            .ok_or_else(|| Error::Message {
                context: "API client wrapper error: ",
                detail: "Missing get_latest_game response".to_string(),
            })
    }

    /// Get media resources (banners, announcements, background)
    pub async fn get_media(&self, target: &ApiTarget, language: &str) -> Result<MediaResponse> {
        let request = media_batch(target, language);

        // Use web batch URL for media APIs
        let url = web_batch_proxy_url(&target.gateway);
        let response: BatchResponse =
            self.batch_request_with_url(&url, &request)
                .await
                .map_err(|e| Error::Message {
                    context: "API client wrapper error: ",
                    detail: format!("Media API request failed: {e}"),
                })?;

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
        let request = media_batch(target, language);

        let url = web_batch_proxy_url(&target.gateway);
        self.batch_request_with_url::<serde_json::Value>(&url, &request)
            .await
            .map_err(|e| Error::Message {
                context: "API client wrapper error: ",
                detail: format!("Media API request failed: {e}"),
            })
    }

    /// Get latest game resources (VFS files) via the direct API endpoint
    pub async fn get_latest_resources(
        &self,
        target: &ApiTarget,
        game_version: &str,
        rand_str: &str,
        platform: &str,
    ) -> Result<Option<GetLatestResourcesResponse>> {
        let url = latest_resources_url(&target.gateway);

        // Derive the version minor (major.minor) from the full version
        let version_minor = game_version
            .split('.')
            .take(2)
            .collect::<Vec<_>>()
            .join(".");

        let url = format!(
            "{}?appcode={}&game_version={}&version={}&platform={}&rand_str={}",
            url, target.game_appcode, version_minor, game_version, platform, rand_str
        );

        let response = self
            .client
            .get(&url)
            .map_err(|e| Error::Message {
                context: "API client wrapper error: ",
                detail: format!("Failed to build get_latest_resources request: {e}"),
            })?
            .header(USER_AGENT_HEADER, &self.user_agent)
            .map_err(|e| Error::Message {
                context: "API client wrapper error: ",
                detail: format!("Failed to set User-Agent header: {e}"),
            })?
            .send()
            .await
            .map_err(|e| Error::Message {
                context: "API client wrapper error: ",
                detail: format!("Failed to get latest resources from {url}: {e}"),
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            if status.as_u16() == 400 && body.contains("resource not exist") {
                return Ok(None);
            }
            return Err(Error::Message {
                context: "API client wrapper error: ",
                detail: format!("get_latest_resources returned error {status}: {body}"),
            });
        }

        let resources: GetLatestResourcesResponse =
            response.json().await.map_err(|e| Error::Message {
                context: "API client wrapper error: ",
                detail: format!("Failed to parse get_latest_resources response: {e}"),
            })?;

        Ok(Some(resources))
    }
}
