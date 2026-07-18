use std::io::ErrorKind;

use crate::api::protocol::{byte_range_from, RANGE_HEADER, USER_AGENT_HEADER};
use crate::error::{Error, Result};
use md5::{Digest, Md5};

use super::requests::ApiClient;
use crate::api::crypto;
use crate::api::types::{GameFileEntry, ResIndex, ResourcePatch};
use crate::runtime::{launcher_metadata_url, GAME_FILES_NAME};
impl ApiClient {
    pub async fn fetch_game_files(
        &self,
        base_url: &str,
        expected_md5: Option<&str>,
    ) -> Result<Vec<GameFileEntry>> {
        let url = launcher_metadata_url(base_url, GAME_FILES_NAME)?;

        let response = self
            .client
            .get(&url)?
            .header(USER_AGENT_HEADER, &self.user_agent)
            .map_err(|e| Error::ApiClient(format!("Failed to set User-Agent header: {e}")))?
            .send()
            .await
            .map_err(|e| {
                Error::ApiClient(format!("Failed to download game_files from {url}: {e}"))
            })?;

        let status = response.status();
        if !status.is_success() {
            return Err(Error::ApiClient(format!(
                "Failed to download game_files: HTTP {status}"
            )));
        }

        let encrypted_data = response.bytes().await.map_err(|e| {
            Error::ApiClient(format!("Failed to read game_files response bytes: {e}"))
        })?;

        // Verify MD5 if provided
        if let Some(expected) = expected_md5 {
            let actual = crate::to_hex(&Md5::digest(&encrypted_data));
            if actual != expected.to_lowercase() {
                return Err(Error::ApiClient(format!(
                    "game_files MD5 mismatch: expected {}, got {}",
                    expected, actual
                )));
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
                .map_err(|e| Error::ApiClient(format!("Failed to parse game_files entry: {e}")))?;
            entries.push(entry);
        }

        Ok(entries)
    }

    /// Fetch and decrypt a resource index file (index_main.json / index_initial.json)
    pub async fn fetch_res_index(&self, url: &str, key: &str) -> Result<ResIndex> {
        let response = self
            .client
            .get(url)?
            .header(USER_AGENT_HEADER, &self.user_agent)
            .map_err(|e| Error::ApiClient(format!("Failed to set User-Agent header: {e}")))?
            .send()
            .await
            .map_err(|e| {
                Error::ApiClient(format!("Failed to download resource index from {url}: {e}"))
            })?;

        let status = response.status();
        if !status.is_success() {
            return Err(Error::ApiClient(format!(
                "Failed to download resource index: HTTP {status}"
            )));
        }

        let base64_data = response
            .text()
            .await
            .map_err(|e| Error::ApiClient(format!("Failed to read resource index as text: {e}")))?;

        // Decrypt
        let decrypted = crypto::decrypt_res_index(&base64_data, key)?;

        // Parse JSON
        let index: ResIndex = serde_json::from_str(&decrypted).map_err(|e| {
            Error::ApiClient(format!(
                "Failed to parse decrypted resource index JSON: {e}"
            ))
        })?;

        Ok(index)
    }

    /// Fetch the resource patch manifest (patch.json)
    ///
    /// Unlike index files, patch.json is NOT encrypted — it's plain JSON.
    pub async fn fetch_res_patch(&self, url: &str) -> Result<ResourcePatch> {
        let response = self
            .client
            .get(url)?
            .header(USER_AGENT_HEADER, &self.user_agent)
            .map_err(|e| Error::ApiClient(format!("Failed to set User-Agent header: {e}")))?
            .send()
            .await
            .map_err(|e| {
                Error::ApiClient(format!("Failed to download resource patch from {url}: {e}"))
            })?;

        let status = response.status();
        if !status.is_success() {
            return Err(Error::ApiClient(format!(
                "Failed to download resource patch: HTTP {status}"
            )));
        }

        let patch: ResourcePatch = response
            .json()
            .await
            .map_err(|e| Error::ApiClient(format!("Failed to parse resource patch JSON: {e}")))?;

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
            .get(url)?
            .header(USER_AGENT_HEADER, &self.user_agent)
            .map_err(|e| Error::ApiClient(format!("Failed to set User-Agent header: {e}")))?;

        let existing = if resume {
            match compio::fs::read(output_path).await {
                Ok(bytes) => {
                    if !bytes.is_empty() {
                        request = request
                            .header(RANGE_HEADER, byte_range_from(bytes.len() as u64))
                            .map_err(|e| {
                                Error::ApiClient(format!("Failed to set Range header: {e}"))
                            })?;
                    }
                    bytes
                }
                Err(err) if err.kind() == ErrorKind::NotFound => Vec::new(),
                Err(err) => {
                    return Err(Error::OpenFileFailed {
                        path: output_path.to_path_buf(),
                        source: err,
                    });
                }
            }
        } else {
            Vec::new()
        };

        let response = request
            .send()
            .await
            .map_err(|e| Error::ApiClient(format!("Failed to download from {url}: {e}")))?;

        let status = response.status();
        if !status.is_success() && status.as_u16() != 206 {
            return Err(Error::ApiClient(format!(
                "Download returned error status: {status}"
            )));
        }

        if let Some(parent) = output_path.parent() {
            compio::fs::create_dir_all(parent)
                .await
                .map_err(|e| Error::CreateDirFailed {
                    path: parent.to_path_buf(),
                    source: e,
                })?;
        }

        let downloaded = response
            .bytes()
            .await
            .map_err(|e| Error::ApiClient(format!("Failed to download bytes: {e}")))?;
        let final_bytes = if !existing.is_empty() && status.as_u16() == 206 {
            let mut merged = existing;
            merged.extend_from_slice(downloaded.as_ref());
            merged
        } else {
            downloaded.to_vec()
        };

        let write_result = compio::fs::write(output_path, final_bytes.clone()).await;
        write_result.0.map_err(|e| Error::WriteFileFailed {
            path: output_path.to_path_buf(),
            source: e,
        })?;

        let mut hasher = Md5::new();
        hasher.update(&final_bytes);
        Ok(crate::to_hex(&hasher.finalize()))
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
            return Err(Error::ApiClient(format!(
                "MD5 mismatch for {url}: expected {expected_md5}, got {actual_md5}"
            )));
        }
        Ok(())
    }
}
