use compio::bytes::Bytes;

use crate::protocol::USER_AGENT_HEADER;
use crate::{Error, Result};
use md5::{Digest, Md5};

use super::requests::ApiClient;
use crate::crypto;
use crate::types::{GameFileEntry, ResIndex, ResourcePatch};
use crate::{launcher_metadata_url, GAME_FILES_NAME};

#[derive(Debug, Clone)]
pub struct GameFilesDocument {
    pub entries: Vec<GameFileEntry>,
    pub encrypted_bytes: Bytes,
}

#[derive(Debug, Clone)]
pub struct ResIndexDocument {
    pub index: ResIndex,
    pub encrypted_bytes: Bytes,
    pub md5: String,
}

pub fn parse_game_files_owned(encrypted_data: Vec<u8>) -> Result<Vec<GameFileEntry>> {
    let decrypted = crypto::decrypt_game_files_owned(encrypted_data)?;
    let mut entries = Vec::new();
    for line in decrypted.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: GameFileEntry = serde_json::from_str(line).map_err(|e| Error::Message {
            context: "API client wrapper error: ",
            detail: format!("Failed to parse game_files entry: {e}"),
        })?;
        entries.push(entry);
    }
    Ok(entries)
}

impl ApiClient {
    pub async fn fetch_game_files(
        &self,
        base_url: &str,
        expected_md5: Option<&str>,
    ) -> Result<Vec<GameFileEntry>> {
        Ok(self
            .fetch_game_files_document(base_url, expected_md5)
            .await?
            .entries)
    }

    /// Fetches and verifies one canonical encrypted `game_files` document while
    /// retaining the exact ciphertext bytes for the later launcher metadata commit.
    pub async fn fetch_game_files_document(
        &self,
        base_url: &str,
        expected_md5: Option<&str>,
    ) -> Result<GameFilesDocument> {
        let url = launcher_metadata_url(base_url, GAME_FILES_NAME)?;

        let response = self
            .client
            .get(&url)?
            .header(USER_AGENT_HEADER, &self.user_agent)
            .map_err(|e| Error::Message {
                context: "API client wrapper error: ",
                detail: format!("Failed to set User-Agent header: {e}"),
            })?
            .send()
            .await
            .map_err(|e| Error::Message {
                context: "API client wrapper error: ",
                detail: format!("Failed to download game_files from {url}: {e}"),
            })?;

        let status = response.status();
        if !status.is_success() {
            return Err(Error::Message {
                context: "API client wrapper error: ",
                detail: format!("Failed to download game_files: HTTP {status}"),
            });
        }

        let encrypted_bytes = response.bytes().await.map_err(|e| Error::Message {
            context: "API client wrapper error: ",
            detail: format!("Failed to read game_files response bytes: {e}"),
        })?;

        if let Some(expected) = expected_md5 {
            let actual = griffr_core::to_hex(&Md5::digest(&encrypted_bytes));
            if !actual.eq_ignore_ascii_case(expected) {
                return Err(Error::Message {
                    context: "API client wrapper error: ",
                    detail: format!(
                        "game_files MD5 mismatch: expected {}, got {}",
                        expected, actual
                    ),
                });
            }
        }

        let entries = parse_game_files_owned(encrypted_bytes.to_vec())?;
        Ok(GameFilesDocument {
            entries,
            encrypted_bytes,
        })
    }

    /// Fetch the exact encrypted resource index document and its parsed content.
    pub async fn fetch_res_index_document(&self, url: &str, key: &str) -> Result<ResIndexDocument> {
        let response = self
            .client
            .get(url)?
            .header(USER_AGENT_HEADER, &self.user_agent)
            .map_err(|e| Error::Message {
                context: "API client wrapper error: ",
                detail: format!("Failed to set User-Agent header: {e}"),
            })?
            .send()
            .await
            .map_err(|e| Error::Message {
                context: "API client wrapper error: ",
                detail: format!("Failed to download resource index from {url}: {e}"),
            })?;

        let status = response.status();
        if !status.is_success() {
            return Err(Error::Message {
                context: "API client wrapper error: ",
                detail: format!("Failed to download resource index: HTTP {status}"),
            });
        }

        let encrypted_bytes = response.bytes().await.map_err(|e| Error::Message {
            context: "API client wrapper error: ",
            detail: format!("Failed to read resource index bytes: {e}"),
        })?;
        let base64_data = std::str::from_utf8(&encrypted_bytes).map_err(|e| Error::Message {
            context: "API client wrapper error: ",
            detail: format!("Resource index response is not UTF-8 text: {e}"),
        })?;

        // Decrypt
        let decrypted = crypto::decrypt_res_index(base64_data.trim(), key)?;

        // Parse JSON
        let index: ResIndex = serde_json::from_str(&decrypted).map_err(|e| Error::Message {
            context: "API client wrapper error: ",
            detail: format!("Failed to parse decrypted resource index JSON: {e}"),
        })?;

        let md5 = griffr_core::to_hex(&Md5::digest(&encrypted_bytes));
        Ok(ResIndexDocument {
            index,
            encrypted_bytes,
            md5,
        })
    }

    /// Fetch and decrypt a resource index file (index_main.json / index_initial.json).
    pub async fn fetch_res_index(&self, url: &str, key: &str) -> Result<ResIndex> {
        Ok(self.fetch_res_index_document(url, key).await?.index)
    }

    /// Fetch the resource patch manifest (patch.json)
    ///
    /// Unlike index files, patch.json is NOT encrypted — it's plain JSON.
    pub async fn fetch_res_patch(&self, url: &str) -> Result<ResourcePatch> {
        let response = self
            .client
            .get(url)?
            .header(USER_AGENT_HEADER, &self.user_agent)
            .map_err(|e| Error::Message {
                context: "API client wrapper error: ",
                detail: format!("Failed to set User-Agent header: {e}"),
            })?
            .send()
            .await
            .map_err(|e| Error::Message {
                context: "API client wrapper error: ",
                detail: format!("Failed to download resource patch from {url}: {e}"),
            })?;

        let status = response.status();
        if !status.is_success() {
            return Err(Error::Message {
                context: "API client wrapper error: ",
                detail: format!("Failed to download resource patch: HTTP {status}"),
            });
        }

        let patch: ResourcePatch = response.json().await.map_err(|e| Error::Message {
            context: "API client wrapper error: ",
            detail: format!("Failed to parse resource patch JSON: {e}"),
        })?;

        Ok(patch)
    }
}
