use std::io::ErrorKind;

use compio::buf::BufResult;
use compio::bytes::Bytes;
use compio::io::AsyncReadAt;

use crate::api::protocol::{byte_range_from, RANGE_HEADER, USER_AGENT_HEADER};
use crate::error::{Error, Result};
use md5::{Digest, Md5};

use super::requests::ApiClient;
use crate::api::crypto;
use crate::api::types::{GameFileEntry, ResIndex, ResourcePatch};
use crate::runtime::{launcher_metadata_url, GAME_FILES_NAME};

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

const HASH_BUFFER_BYTES: usize = 1024 * 1024;

async fn hash_file_prefix(path: &std::path::Path, len: u64, hasher: &mut Md5) -> Result<()> {
    let input = compio::fs::File::open(path)
        .await
        .map_err(|source| Error::IoAt {
            action: "open file",
            path: path.to_path_buf(),
            source,
        })?;
    let mut offset = 0u64;
    let mut buffer = vec![0u8; HASH_BUFFER_BYTES];
    while offset < len {
        let requested = usize::try_from((len - offset).min(HASH_BUFFER_BYTES as u64))
            .unwrap_or(HASH_BUFFER_BYTES);
        buffer.truncate(requested);
        let BufResult(read_result, mut returned_buffer) = input.read_at(buffer, offset).await;
        let read = read_result.map_err(|source| Error::IoAt {
            action: "read file",
            path: path.to_path_buf(),
            source,
        })?;
        if read == 0 {
            return Err(Error::Message {
                context: "API client wrapper error: ",
                detail: format!(
                    "Download resume prefix ended at byte {offset}, expected {len}: {}",
                    path.display()
                ),
            });
        }
        hasher.update(&returned_buffer[..read]);
        offset = offset.saturating_add(read as u64);
        returned_buffer.resize(HASH_BUFFER_BYTES, 0);
        buffer = returned_buffer;
    }
    Ok(())
}

pub(crate) fn parse_game_files_owned(encrypted_data: Vec<u8>) -> Result<Vec<GameFileEntry>> {
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
            let actual = crate::to_hex(&Md5::digest(&encrypted_bytes));
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

        let md5 = crate::to_hex(&Md5::digest(&encrypted_bytes));
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

    /// Download a file with optional resume support.
    ///
    /// The response body is streamed directly into the destination and hashed
    /// as it is written. Only a fixed-size buffer is used to hash an existing
    /// prefix when resuming; the complete file is never staged in memory.
    pub async fn download_file(
        &self,
        url: &str,
        output_path: &std::path::Path,
        resume: bool,
    ) -> Result<String> {
        let existing_len = if resume {
            match compio::fs::metadata(output_path).await {
                Ok(metadata) if metadata.is_file() => metadata.len(),
                Ok(_) => {
                    return Err(Error::Message {
                        context: "API client wrapper error: ",
                        detail: format!(
                            "Download destination is not a file: {}",
                            output_path.display()
                        ),
                    });
                }
                Err(err) if err.kind() == ErrorKind::NotFound => 0,
                Err(err) => {
                    return Err(Error::IoAt {
                        action: "query file metadata/stat for",
                        path: output_path.to_path_buf(),
                        source: err,
                    });
                }
            }
        } else {
            0
        };

        let mut request = self
            .client
            .get(url)?
            .header(USER_AGENT_HEADER, &self.user_agent)
            .map_err(|e| Error::Message {
                context: "API client wrapper error: ",
                detail: format!("Failed to set User-Agent header: {e}"),
            })?;
        if existing_len > 0 {
            request = request
                .header(RANGE_HEADER, byte_range_from(existing_len))
                .map_err(|e| Error::Message {
                    context: "API client wrapper error: ",
                    detail: format!("Failed to set Range header: {e}"),
                })?;
        }

        let (send_timeout, body_timeout) = crate::runtime::task_pool::download::download_timeouts();
        let response = compio::time::timeout(send_timeout, request.send())
            .await?
            .map_err(|e| Error::Message {
                context: "API client wrapper error: ",
                detail: format!("Failed to download from {url}: {e}"),
            })?;

        let status = response.status();
        if !status.is_success() {
            return Err(Error::Message {
                context: "API client wrapper error: ",
                detail: format!("Download returned error status: {status}"),
            });
        }

        if let Some(parent) = output_path.parent() {
            compio::fs::create_dir_all(parent)
                .await
                .map_err(|e| Error::IoAt {
                    action: "create directory",
                    path: parent.to_path_buf(),
                    source: e,
                })?;
        }

        let resume_effective = existing_len > 0 && status.as_u16() == 206;
        let mut hasher = Md5::new();
        let start_offset = if resume_effective {
            hash_file_prefix(output_path, existing_len, &mut hasher).await?;
            existing_len
        } else {
            0
        };

        let mut open_options = compio::fs::OpenOptions::new();
        open_options
            .create(true)
            .write(true)
            .truncate(!resume_effective);
        let output = open_options
            .open(output_path)
            .await
            .map_err(|e| Error::IoAt {
                action: "open file",
                path: output_path.to_path_buf(),
                source: e,
            })?;

        let (output, _) = crate::runtime::task_pool::download_write::write_http_body(
            response.bytes_stream(),
            output,
            output_path,
            url,
            start_offset,
            body_timeout,
            |chunk| hasher.update(chunk),
            |_| {},
        )
        .await?;
        output.sync_data().await.map_err(|e| Error::IoAt {
            action: "write to file",
            path: output_path.to_path_buf(),
            source: e,
        })?;

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
            return Err(Error::Message {
                context: "API client wrapper error: ",
                detail: format!(
                    "MD5 mismatch for {url}: expected {expected_md5}, got {actual_md5}"
                ),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn serve_once(
        body: Vec<u8>,
        range_start: usize,
        honor_range: bool,
    ) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind download fixture");
        let url = format!(
            "http://{}/payload.bin",
            listener.local_addr().expect("download fixture address")
        );
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept download fixture request");
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 2048];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).expect("read fixture request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            let request = String::from_utf8(request).expect("fixture request UTF-8");
            assert!(
                request
                    .lines()
                    .any(|line| line.eq_ignore_ascii_case(&format!("Range: bytes={range_start}-"))),
                "resume request did not contain the expected Range header:\n{request}"
            );

            if honor_range {
                let suffix = &body[range_start..];
                write!(
                    stream,
                    "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
                    suffix.len(),
                    range_start,
                    body.len() - 1,
                    body.len()
                )
                .unwrap();
                stream.write_all(suffix).unwrap();
            } else {
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
            request
        });
        (url, handle)
    }

    #[compio::test]
    async fn download_file_resumes_without_rebuilding_the_complete_file_in_memory() {
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("payload.bin");
        let body = b"prefix-and-streamed-suffix".to_vec();
        let range_start = 7;
        std::fs::write(&output, &body[..range_start]).unwrap();
        let (url, server) = serve_once(body.clone(), range_start, true);

        let actual_md5 = ApiClient::new()
            .unwrap()
            .download_file(&url, &output, true)
            .await
            .unwrap();

        server.join().expect("download fixture thread");
        assert_eq!(std::fs::read(&output).unwrap(), body);
        assert_eq!(actual_md5, crate::to_hex(&Md5::digest(&body)));
    }

    #[compio::test]
    async fn download_file_truncates_when_a_server_ignores_the_resume_range() {
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("payload.bin");
        let body = b"authoritative-complete-payload".to_vec();
        let range_start = 8;
        std::fs::write(&output, b"obsolete").unwrap();
        let (url, server) = serve_once(body.clone(), range_start, false);

        let actual_md5 = ApiClient::new()
            .unwrap()
            .download_file(&url, &output, true)
            .await
            .unwrap();

        server.join().expect("download fixture thread");
        assert_eq!(std::fs::read(&output).unwrap(), body);
        assert_eq!(actual_md5, crate::to_hex(&Md5::digest(&body)));
    }
}
