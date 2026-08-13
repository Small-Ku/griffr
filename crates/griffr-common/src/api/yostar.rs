use std::time::{SystemTime, UNIX_EPOCH};

use md5::{Digest, Md5};
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize};

use crate::error::{Error, Result};
use crate::runtime::build_cdn_file_url;

pub const YOSTAR_GATEWAY: &str = "https://api-launcher-en.yo-star.com";
pub const YOSTAR_ARKNIGHTS_TAG: &str = "Arknights_EN";
pub const YOSTAR_LAUNCHER_VERSION: &str = "1.8.1";
const YOSTAR_AUTH_SALT: &str = "DE7108E9B2842FD460F4777702727869";
const AUTHORIZATION_HEADER: &str = "Authorization";

#[derive(Debug, Clone)]
pub struct YostarApiClient {
    client: cyper::Client,
    gateway: String,
    game_tag: String,
    launcher_version: String,
}

#[derive(Debug, Serialize)]
struct AuthHead<'a> {
    game_tag: &'a str,
    time: u64,
    version: &'a str,
}

#[derive(Debug, Serialize)]
struct Authorization<'a> {
    head: AuthHead<'a>,
    sign: String,
}

#[derive(Debug, Deserialize)]
struct ApiEnvelope {
    code: i64,
    #[serde(default)]
    data: serde_json::Value,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    msg: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct YostarGameConfig {
    pub game_latest_version: String,
    pub game_lowest_version: String,
    pub game_latest_file_path: String,
    pub game_start_exe_name: String,
    #[serde(default)]
    pub game_start_params: Vec<String>,
    #[serde(default)]
    pub game_uninstall_script: String,
    #[serde(default, deserialize_with = "deserialize_optional_u64")]
    pub decompression_size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct ManifestLocator {
    url: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct YostarCdnConfig {
    pub primary_cdn: String,
    #[serde(default)]
    pub back_up_cdn: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct YostarManifest {
    pub source: String,
    #[serde(rename = "file")]
    pub files: Vec<YostarManifestEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct YostarManifestEntry {
    pub path: String,
    #[serde(deserialize_with = "deserialize_u64")]
    pub size: u64,
    pub hash: String,
}

#[derive(Debug, Clone)]
pub struct YostarReleaseSnapshot {
    pub config: YostarGameConfig,
    pub manifest: YostarManifest,
    pub cdn: YostarCdnConfig,
}

impl YostarApiClient {
    pub fn arknights_en() -> Result<Self> {
        Self::new(
            YOSTAR_GATEWAY,
            YOSTAR_ARKNIGHTS_TAG,
            YOSTAR_LAUNCHER_VERSION,
        )
    }

    pub fn new(
        gateway: impl Into<String>,
        game_tag: impl Into<String>,
        launcher_version: impl Into<String>,
    ) -> Result<Self> {
        Ok(Self {
            client: cyper::Client::new()?,
            gateway: gateway.into().trim_end_matches('/').to_string(),
            game_tag: game_tag.into(),
            launcher_version: launcher_version.into(),
        })
    }

    fn authorization(&self, request_body: &str) -> Result<String> {
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| Error::Message {
                context: "YoStar API error: ",
                detail: format!("system clock is before UNIX epoch: {error}"),
            })?
            .as_secs();
        let head = AuthHead {
            game_tag: &self.game_tag,
            time,
            version: &self.launcher_version,
        };
        let head_json = serde_json::to_string(&head).map_err(|error| Error::Message {
            context: "YoStar API error: ",
            detail: format!("failed to serialize authorization head: {error}"),
        })?;
        let sign = crate::to_hex(&Md5::digest(format!(
            "{head_json}{request_body}{YOSTAR_AUTH_SALT}"
        )));
        serde_json::to_string(&Authorization { head, sign }).map_err(|error| Error::Message {
            context: "YoStar API error: ",
            detail: format!("failed to serialize Authorization header: {error}"),
        })
    }

    async fn get_api<T: DeserializeOwned>(&self, url: &str) -> Result<T> {
        let authorization = self.authorization("")?;
        let response = self
            .client
            .get(url)
            .map_err(|error| Error::Message {
                context: "YoStar API error: ",
                detail: format!("failed to build request for {url}: {error}"),
            })?
            .header(AUTHORIZATION_HEADER, authorization)
            .map_err(|error| Error::Message {
                context: "YoStar API error: ",
                detail: format!("failed to attach Authorization header: {error}"),
            })?
            .send()
            .await
            .map_err(|error| Error::Message {
                context: "YoStar API error: ",
                detail: format!("request to {url} failed: {error}"),
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Message {
                context: "YoStar API error: ",
                detail: format!("{url} returned {status}: {body}"),
            });
        }
        let envelope = response
            .json::<ApiEnvelope>()
            .await
            .map_err(|error| Error::Message {
                context: "YoStar API error: ",
                detail: format!("failed to parse response from {url}: {error}"),
            })?;
        if envelope.code != 200 {
            let detail = envelope
                .message
                .or(envelope.msg)
                .unwrap_or_else(|| "no server message".to_string());
            return Err(Error::Message {
                context: "YoStar API error: ",
                detail: format!("{url} returned API code {}: {detail}", envelope.code),
            });
        }
        serde_json::from_value(envelope.data).map_err(|error| Error::Message {
            context: "YoStar API error: ",
            detail: format!("failed to decode data from {url}: {error}"),
        })
    }

    pub async fn game_config(&self) -> Result<YostarGameConfig> {
        self.get_api(&format!("{}/api/launcher/game/config", self.gateway))
            .await
    }

    pub async fn manifest_for(&self, version: &str, basis: &str) -> Result<YostarManifest> {
        let mut url = url::Url::parse(&format!("{}/api/launcher/game/config/json", self.gateway))
            .map_err(|error| Error::Message {
            context: "YoStar API error: ",
            detail: format!("invalid manifest resolver URL: {error}"),
        })?;
        url.query_pairs_mut()
            .append_pair("version", version)
            .append_pair("file_path", basis);
        let locator: ManifestLocator = self.get_api(url.as_str()).await?;
        self.fetch_manifest(&locator.url).await
    }

    pub async fn fetch_manifest(&self, url: &str) -> Result<YostarManifest> {
        let response = self
            .client
            .get(url)
            .map_err(|error| Error::Message {
                context: "YoStar API error: ",
                detail: format!("failed to build manifest request for {url}: {error}"),
            })?
            .send()
            .await
            .map_err(|error| Error::Message {
                context: "YoStar API error: ",
                detail: format!("manifest request to {url} failed: {error}"),
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Message {
                context: "YoStar API error: ",
                detail: format!("manifest URL returned {status}: {body}"),
            });
        }
        response.json().await.map_err(|error| Error::Message {
            context: "YoStar API error: ",
            detail: format!("failed to parse game manifest from {url}: {error}"),
        })
    }

    pub async fn cdn_config(&self) -> Result<YostarCdnConfig> {
        self.get_api(&format!(
            "{}/api/launcher/advanced/game/download/cdn",
            self.gateway
        ))
        .await
    }

    pub async fn latest_release(&self) -> Result<YostarReleaseSnapshot> {
        let (config, cdn) = futures_util::try_join!(self.game_config(), self.cdn_config())?;
        let manifest = self
            .manifest_for(&config.game_latest_version, &config.game_latest_file_path)
            .await?;
        Ok(YostarReleaseSnapshot {
            config,
            manifest,
            cdn,
        })
    }
}

impl YostarManifest {
    pub fn file_url(&self, cdn_root: &str, entry: &YostarManifestEntry) -> String {
        let base = [
            cdn_root.trim_end_matches('/'),
            self.source.trim_matches('/'),
        ]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/");
        build_cdn_file_url(&format!("{base}/"), &entry.path)
    }
}

fn deserialize_u64<'de, D>(deserializer: D) -> std::result::Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Number {
        Integer(u64),
        Text(String),
    }
    match Number::deserialize(deserializer)? {
        Number::Integer(value) => Ok(value),
        Number::Text(value) => value.parse().map_err(serde::de::Error::custom),
    }
}

fn deserialize_optional_u64<'de, D>(deserializer: D) -> std::result::Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OptionalNumber {
        Integer(u64),
        Text(String),
        Null,
    }
    match OptionalNumber::deserialize(deserializer)? {
        OptionalNumber::Integer(value) => Ok(Some(value)),
        OptionalNumber::Text(value) if value.trim().is_empty() => Ok(None),
        OptionalNumber::Text(value) => value.parse().map(Some).map_err(serde::de::Error::custom),
        OptionalNumber::Null => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_accepts_string_sizes_and_builds_cdn_url() {
        let manifest: YostarManifest = serde_json::from_str(
            r#"{"source":"game/files","file":[{"path":"a b/[x].bin","size":"9","hash":"11051210869376104954"}]}"#,
        )
        .unwrap();
        assert_eq!(manifest.files[0].size, 9);
        assert_eq!(
            manifest.file_url("https://cdn.example/root/", &manifest.files[0]),
            "https://cdn.example/root/game/files/a%20b/%5Bx%5D.bin"
        );
    }

    #[test]
    fn authorization_uses_observed_launcher_shape() {
        let client =
            YostarApiClient::new("https://example.invalid", "Arknights_EN", "1.8.1").unwrap();
        let auth = client.authorization("").unwrap();
        let value: serde_json::Value = serde_json::from_str(&auth).unwrap();
        assert_eq!(value["head"]["game_tag"], "Arknights_EN");
        assert_eq!(value["head"]["version"], "1.8.1");
        assert_eq!(value["sign"].as_str().unwrap().len(), 32);
    }
}
