use super::requests::ApiClient;
use crate::api::types::{
    AnnouncementResponse, BannerResponse, MainBgImageResponse, SidebarResponse,
};
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
}
